//! Aggregates the resource measurements of an experiment's runs into one summary per node.
//!
//! Two sources are combined:
//! - the guest side: resource logs written by the commands run inside the routers
//!   (memory used and CPU load as seen by the guest OS);
//! - the host side: the host monitor's CSV files (see `utils::qemu::host_monitor`), measured
//!   from outside the VM, with the same definitions for every OS.
//!
//! Each metric comes from the source that measures it best:
//! - CPU: host side, `guest_pct` (time the vCPUs spent executing guest code). Guest-side CPU
//!   readings use OS-specific definitions and miss short interrupt-driven bursts, so they are
//!   not comparable between OSes. If no run of a node has host data (results produced before
//!   the host monitor existed), the guest-side CPU is used instead, with a warning, and the
//!   `cpu_source` column says so.
//! - Memory: guest side (memory in use). The host only sees the pages the guest ever touched.
//!
//! The differences between the two views are reported too, computed run by run then reduced:
//! - `cpu_diff_pct` = host-measured guest CPU - guest-reported CPU (> 0: the OS under-reports its load);
//! - `mem_diff_mib` = host RSS of the QEMU process - guest-reported used memory (guest page
//!   cache, pages touched once then freed, QEMU's own memory).
//!
//! Expected layout (logs are looked up in each run folder):
//! ```text
//! <experiment>/
//!   run_1/experiment.log              <- contains the "Experiment start" / "Experiment end" lines
//!   run_1/<router>.log                <- one guest-side resource log per router
//!   run_1/host_resources/<node>.csv   <- one host-side file per VM
//!   run_2/...
//!   summary/<node>.csv                <- written here, one summary per node
//! ```
//!
//! A router log is recognized by its content, not by its name: any `*.log` of a run folder
//! (other than `experiment.log`) that contains resource readings is a router log, and the
//! router name is the file name without extension.
//!
//! Method: every run is aligned on its own experiment start, resampled onto a common 1 s
//! grid by linear interpolation (never across a gap, never beyond the first/last reading),
//! then the runs are reduced point by point (median, mean, min, max).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::DateTime;
use tracing::{info, warn};
use crate::utils::files::dir::subtract_project_root;
use crate::utils::qemu::host_monitor::{COL_GUEST_PCT, COL_RSS_ANON_MIB, COL_TIMESTAMP, HOST_RESOURCES_DIR_NAME};
use crate::utils::results::flent::summary::SUMMARY_FOLDER_NAME;
use crate::utils::results::utils::natural_key;

const TARGET: &str = "results";


const RUN_DIR_PREFIX: &str = "run_";
const EXPERIMENT_LOG_NAME: &str = "experiment.log";

/// Sampling interval of the resource monitors (the `sleep 1` of the router loop, the host monitor).
const GRID_STEP_S: f64 = 1.0;
/// Data kept before the start and after the end of the experiment (same as the plot script).
const MARGIN_S: f64 = 10.0;
/// Two readings further apart than this are not interpolated across.
const MAX_GAP_S: f64 = 2.5 * GRID_STEP_S;

/// One reading of a router's guest-side resource log; `t` is in seconds since the experiment start.
#[derive(Debug, Clone, PartialEq)]
struct GuestSample {
    t: f64,
    mem_used_mib: f64,
    /// `None` when the reported value is out of the 0..=100 range (broken reading).
    cpu_pct: Option<f64>,
}

/// One reading of the host monitor; `t` is in seconds since the experiment start.
#[derive(Debug, Clone, PartialEq)]
struct HostSample {
    t: f64,
    /// Host-measured guest execution time, in % of the vCPU capacity.
    guest_cpu_pct: Option<f64>,
    rss_anon_mib: Option<f64>,
}

/// A node's resources during one run.
#[derive(Default)]
struct NodeRun {
    duration_s: f64,
    guest: Vec<GuestSample>,
    host: Vec<HostSample>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CpuSource {
    Host,
    Guest,
}

impl CpuSource {
    fn as_str(&self) -> &'static str {
        match self {
            CpuSource::Host => "host",
            CpuSource::Guest => "guest",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Stats {
    n: usize,
    median: f64,
    mean: f64,
    min: f64,
    max: f64,
}

impl Stats {
    fn of(values: &[f64]) -> Option<Stats> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let mid = sorted.len() / 2;
        let median = if sorted.len() % 2 == 1 { sorted[mid] } else { (sorted[mid - 1] + sorted[mid]) / 2.0 };
        Some(Stats {
            n: sorted.len(),
            median,
            mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
            min: sorted[0],
            max: sorted[sorted.len() - 1],
        })
    }
}

#[derive(Debug)]
struct Row {
    elapsed_s: f64,
    in_experiment: bool,
    /// Guest-reported used memory
    mem: Option<Stats>,
    /// CPU from `cpu_source`
    cpu: Option<Stats>,
    /// Guest-reported CPU (kept for reference)
    guest_cpu: Option<Stats>,
    /// Host RSS of the QEMU process
    host_mem: Option<Stats>,
    /// Host-measured guest CPU - guest-reported CPU, per run
    cpu_diff: Option<Stats>,
    /// Host RSS - guest-reported used memory, per run
    mem_diff: Option<Stats>,
}

/// A node's summary
struct Summary {
    cpu_source: CpuSource,
    rows: Vec<Row>,
}

/// Writes `<experiment_dir>/summary/<node>.csv` for every node found in the runs and returns
/// the paths. Files are rewritten on every call, since runs may have been added.
pub fn write_resources_summaries(experiment_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut per_node: BTreeMap<String, Vec<NodeRun>> = BTreeMap::new();
    let mut usable_runs = 0usize;

    for run_dir in list_run_dirs(experiment_dir)? {
        let window = read_text_lossy(&run_dir.join(EXPERIMENT_LOG_NAME)).ok().and_then(|t| experiment_window(&t));
        let Some((start, end)) = window else {
            warn!(target: TARGET, "{}: no experiment start/end found, run ignored for resources", run_dir.display());
            continue;
        };
        usable_runs += 1;

        let mut runs: BTreeMap<String, NodeRun> = BTreeMap::new();
        let new_run = || NodeRun { duration_s: end - start, ..NodeRun::default() };

        for log_path in list_router_log_candidates(&run_dir)? {
            let samples = parse_resource_log(&read_text_lossy(&log_path)?, start, end);
            if samples.is_empty() {
                continue;
            }
            runs.entry(file_stem(&log_path)).or_insert_with(new_run).guest = samples;
        }

        for csv_path in list_host_files(&run_dir)? {
            let samples = parse_host_csv(&read_text_lossy(&csv_path)?, start, end)
                .with_context(|| format!("cannot parse {}", csv_path.display()))?;
            if samples.is_empty() {
                continue;
            }
            runs.entry(file_stem(&csv_path)).or_insert_with(new_run).host = samples;
        }

        for (node, run) in runs {
            per_node.entry(node).or_default().push(run);
        }
    }

    let output_dir = experiment_dir.join(SUMMARY_FOLDER_NAME);
    let mut written = Vec::new();
    for (node, runs) in &per_node {
        if runs.len() < usable_runs {
            warn!(target: TARGET, "Node \"{node}\": resource data found in {}/{usable_runs} runs", runs.len());
        }

        let summary = summarize(runs);
        if summary.cpu_source == CpuSource::Guest && runs.iter().any(|r| !r.guest.is_empty()) {
            warn!(target: TARGET, "Node \"{node}\": no host monitor data, CPU taken from the guest-side readings (not comparable between OSes)");
        }

        let output_path = output_dir.join(format!("{node}.csv"));
        write_summary_csv(&output_path, &summary)?;
        info!(target: TARGET, "Summarized resources of \"{node}\" over {} run(s) (CPU from {}):", runs.len(), summary.cpu_source.as_str());
        info!(target: TARGET, "\t{}", subtract_project_root(&output_path));
        written.push(output_path);
    }
    if written.is_empty() {
        info!(target: TARGET, "No resource data found in {}", experiment_dir.display());
    }
    Ok(written)
}

type Series = Vec<(f64, f64)>;

/// Reduces the runs of a node, point by point, on a common time grid.
fn summarize(runs: &[NodeRun]) -> Summary {
    let durations: Vec<f64> = runs.iter().map(|r| r.duration_s).collect();
    let max_duration = durations.iter().copied().fold(0.0, f64::max);
    let median_duration = Stats::of(&durations).map_or(max_duration, |s| s.median);

    // One series per run (possibly empty)
    let guest_mem: Vec<Series> = runs.iter().map(|r| r.guest.iter().map(|s| (s.t, s.mem_used_mib)).collect()).collect();
    let guest_cpu: Vec<Series> = runs.iter().map(|r| r.guest.iter().filter_map(|s| s.cpu_pct.map(|c| (s.t, c))).collect()).collect();
    let host_cpu: Vec<Series> = runs.iter().map(|r| r.host.iter().filter_map(|s| s.guest_cpu_pct.map(|c| (s.t, c))).collect()).collect();
    let host_mem: Vec<Series> = runs.iter().map(|r| r.host.iter().filter_map(|s| s.rss_anon_mib.map(|m| (s.t, m))).collect()).collect();

    let cpu_source = if host_cpu.iter().any(|s| !s.is_empty()) { CpuSource::Host } else { CpuSource::Guest };
    let cpu = match cpu_source {
        CpuSource::Host => &host_cpu,
        CpuSource::Guest => &guest_cpu,
    };

    let steps = ((max_duration + 2.0 * MARGIN_S) / GRID_STEP_S).round() as usize;
    let mut rows = Vec::new();
    for k in 0..=steps {
        let t = -MARGIN_S + k as f64 * GRID_STEP_S;

        let at = |series: &[Series]| -> Option<Stats> {
            let values: Vec<f64> = series.iter().filter_map(|s| interpolate(s, t)).collect();
            Stats::of(&values)
        };
        // Difference within each run, then reduced over the runs
        let diff_at = |minuend: &[Series], subtrahend: &[Series]| -> Option<Stats> {
            let values: Vec<f64> = minuend
                .iter()
                .zip(subtrahend)
                .filter_map(|(a, b)| Some(interpolate(a, t)? - interpolate(b, t)?))
                .collect();
            Stats::of(&values)
        };

        let row = Row {
            elapsed_s: t,
            in_experiment: t >= 0.0 && t <= median_duration + 1e-9,
            mem: at(&guest_mem),
            cpu: at(cpu),
            guest_cpu: at(&guest_cpu),
            host_mem: at(&host_mem),
            cpu_diff: diff_at(&host_cpu, &guest_cpu),
            mem_diff: diff_at(&host_mem, &guest_mem),
        };

        if row.mem.is_none() && row.cpu.is_none() && row.guest_cpu.is_none() && row.host_mem.is_none() {
            continue;
        }
        rows.push(row);
    }

    Summary { cpu_source, rows }
}

/// Linear interpolation of `points` (sorted by time) at time `t`. `None` before the first
/// point, after the last one, or when the two surrounding points are too far apart.
fn interpolate(points: &[(f64, f64)], t: f64) -> Option<f64> {
    let next = points.partition_point(|p| p.0 <= t);
    if next == 0 {
        return None;
    }
    let (t0, v0) = points[next - 1];
    if t0 == t {
        return Some(v0);
    }
    let &(t1, v1) = points.get(next)?;
    if t1 - t0 > MAX_GAP_S {
        return None;
    }
    Some(v0 + (v1 - v0) * (t - t0) / (t1 - t0))
}

/// Keeps the samples of the experiment window plus a margin, sorted by time.
fn in_window<T>(mut samples: Vec<T>, time: impl Fn(&T) -> f64, start: f64, end: f64) -> Vec<T> {
    samples.retain(|s| time(s) >= -MARGIN_S && time(s) <= end - start + MARGIN_S);
    samples.sort_by(|a, b| time(a).total_cmp(&time(b)));
    samples
}

/// Readings of a resource log, as seconds since `start`, restricted to the experiment
/// window plus a margin. Each reading is a line like
/// `2026-10-19T11:31:54.226655Z TRACE rx: 3921<TAB>3661<TAB>260<TAB>8.63268`
/// (total MiB, free MiB, used MiB, CPU %). Echoed commands, the header and any line that
/// does not hold four numbers are ignored, as are impossible readings (used > total).
fn parse_resource_log(text: &str, start: f64, end: f64) -> Vec<GuestSample> {
    let samples = text
        .lines()
        .filter_map(|line| {
            let (head, payload) = line.split_once("TRACE rx:")?;
            let time = head.split_whitespace().find_map(parse_time)?;
            let mut fields = payload.split_whitespace().take(4).map(|f| f.parse::<f64>().ok().filter(|v| v.is_finite()));
            let (total, _free, used, cpu) = (fields.next()??, fields.next()??, fields.next()??, fields.next()??);
            if total <= 0.0 || used < 0.0 || used > total {
                return None;
            }
            Some(GuestSample { t: time - start, mem_used_mib: used, cpu_pct: (0.0..=100.0).contains(&cpu).then_some(cpu) })
        })
        .collect();
    in_window(samples, |s: &GuestSample| s.t, start, end)
}

/// Readings of a host monitor file, as seconds since `start`, restricted to the experiment
/// window plus a margin. Columns are looked up by name; empty or non-finite values are
/// skipped (e.g. no RSS when the process could not be read).
fn parse_host_csv(text: &str, start: f64, end: f64) -> anyhow::Result<Vec<HostSample>> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let headers = reader.headers()?.clone();
    let column = |name: &str| headers.iter().position(|h| h == name);

    let Some(time_column) = column(COL_TIMESTAMP) else {
        anyhow::bail!("no \"{COL_TIMESTAMP}\" column");
    };
    let (cpu_column, mem_column) = (column(COL_GUEST_PCT), column(COL_RSS_ANON_MIB));

    let mut samples = Vec::new();
    for record in reader.records() {
        // A partially written last line (VM stopped, disk full) is not an error
        let Ok(record) = record else { continue };
        let Some(time) = record.get(time_column).and_then(parse_time) else { continue };
        let number = |index: Option<usize>| index
            .and_then(|i| record.get(i))
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0);

        samples.push(HostSample { t: time - start, guest_cpu_pct: number(cpu_column), rss_anon_mib: number(mem_column) });
    }
    Ok(in_window(samples, |s: &HostSample| s.t, start, end))
}

/// Start and end of the experiment (as epoch seconds) from `experiment.log`: the last
/// "Experiment start" and the last "Experiment end", like the plot script does.
fn experiment_window(text: &str) -> Option<(f64, f64)> {
    let last_time_of = |marker: &str| {
        text.lines()
            .filter(|line| line.contains(marker))
            .filter_map(|line| line.split_whitespace().find_map(parse_time))
            .last()
    };
    let start = last_time_of("INFO experiment: Experiment start")?;
    let end = last_time_of("INFO experiment: Experiment end")?;
    (end >= start).then_some((start, end))
}

/// RFC 3339 timestamp (`2026-10-19T11:31:54.226655Z`) as epoch seconds.
fn parse_time(token: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(token).ok().map(|dt| dt.timestamp_micros() as f64 / 1e6)
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn list_run_dirs(experiment_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(experiment_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(RUN_DIR_PREFIX)))
        .collect();
    dirs.sort_by_key(|p| natural_key(&p.file_name().unwrap_or_default().to_string_lossy()));
    Ok(dirs)
}

fn list_files_with_extension(dir: &Path, extension: &str) -> anyhow::Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == extension))
        .collect();
    files.sort();
    Ok(files)
}

fn list_router_log_candidates(run_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut logs = list_files_with_extension(run_dir, "log")?;
    logs.retain(|p| p.file_name().is_some_and(|n| n != EXPERIMENT_LOG_NAME));
    Ok(logs)
}

fn list_host_files(run_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    list_files_with_extension(&run_dir.join(HOST_RESOURCES_DIR_NAME), "csv")
}

const STATS_SUFFIXES: [&str; 4] = ["median", "mean", "min", "max"];

fn write_summary_csv(path: &Path, summary: &Summary) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut writer = csv::Writer::from_path(path).with_context(|| format!("cannot create {}", path.display()))?;

    // The first columns keep their historical names (read by resource_plot.py)
    let mut header = vec![String::from("elapsed_s"), String::from("in_experiment")];
    for (count, metric) in [
        ("n_mem", "mem_used_mib"),
        ("n_cpu", "cpu_pct"),
        ("n_guest_cpu", "guest_cpu_pct"),
        ("n_host_mem", "host_rss_mib"),
        ("n_cpu_diff", "cpu_diff_pct"),
        ("n_mem_diff", "mem_diff_mib"),
    ] {
        header.push(count.to_string());
        header.extend(STATS_SUFFIXES.iter().map(|s| format!("{metric}_{s}")));
    }
    header.push(String::from("cpu_source"));
    writer.write_record(&header)?;

    let stats_fields = |s: &Option<Stats>| -> Vec<String> {
        match s {
            Some(s) => vec![
                s.n.to_string(),
                format!("{:.4}", s.median),
                format!("{:.4}", s.mean),
                format!("{:.4}", s.min),
                format!("{:.4}", s.max),
            ],
            None => vec!["0".into(), String::new(), String::new(), String::new(), String::new()],
        }
    };
    for row in &summary.rows {
        let mut record = vec![format!("{:.3}", row.elapsed_s), row.in_experiment.to_string()];
        for stats in [&row.mem, &row.cpu, &row.guest_cpu, &row.host_mem, &row.cpu_diff, &row.mem_diff] {
            record.extend(stats_fields(stats));
        }
        record.push(summary.cpu_source.as_str().to_string());
        writer.write_record(&record)?;
    }
    writer.flush()?;
    Ok(())
}

/// The logs may contain bytes that are not valid UTF-8 (serial console noise).
fn read_text_lossy(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: f64 = 1_792_765_774.0; // 2026-10-23T14:29:34Z

    fn guest_run(points: &[(f64, f64, f64)]) -> Vec<GuestSample> {
        points.iter().map(|&(t, mem, cpu)| GuestSample { t, mem_used_mib: mem, cpu_pct: Some(cpu) }).collect()
    }

    fn host_run(points: &[(f64, f64, f64)]) -> Vec<HostSample> {
        points.iter().map(|&(t, cpu, rss)| HostSample { t, guest_cpu_pct: Some(cpu), rss_anon_mib: Some(rss) }).collect()
    }

    #[test]
    fn host_csv_is_parsed_by_column_name() {
        let text = "\
timestamp,vcpu,guest_pct,exit_pct,vhost_pct,qemu_pct,total_pct,rss_anon_mib,vm_tx_pps,vm_rx_pps,drop_pps,ns_per_packet,busiest_thread_pct,busiest_thread
2026-10-23T14:29:33.000000Z,2,0.50,0.10,0.00,1.00,1.60,529.9,1,1,0,,0.70,CPU_1/KVM
2026-10-23T14:29:35.000000Z,2,70.30,1.40,70.40,0.50,142.60,,1108637,1108600,235,2575,80.00,vhost-1
2026-10-23T14:29:36.0000";

        let samples = parse_host_csv(text, START, START + 60.0).unwrap();

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0], HostSample { t: -1.0, guest_cpu_pct: Some(0.5), rss_anon_mib: Some(529.9) });
        assert_eq!(samples[1], HostSample { t: 1.0, guest_cpu_pct: Some(70.3), rss_anon_mib: None });
    }

    #[test]
    fn host_csv_without_timestamp_is_an_error() {
        assert!(parse_host_csv("a,b\n1,2\n", START, START + 1.0).is_err());
    }

    #[test]
    fn cpu_comes_from_the_host_and_memory_from_the_guest() {
        let runs = vec![NodeRun {
            duration_s: 2.0,
            guest: guest_run(&[(0.0, 260.0, 46.0), (1.0, 262.0, 46.0), (2.0, 264.0, 46.0)]),
            host: host_run(&[(0.0, 70.0, 530.0), (1.0, 70.0, 530.0), (2.0, 70.0, 530.0)]),
        }];

        let summary = summarize(&runs);
        let row = summary.rows.iter().find(|r| r.elapsed_s == 1.0).unwrap();

        assert_eq!(summary.cpu_source, CpuSource::Host);
        assert_eq!(row.cpu.as_ref().unwrap().median, 70.0);
        assert_eq!(row.mem.as_ref().unwrap().median, 262.0);
        assert_eq!(row.guest_cpu.as_ref().unwrap().median, 46.0);
        assert_eq!(row.host_mem.as_ref().unwrap().median, 530.0);
        assert_eq!(row.cpu_diff.as_ref().unwrap().median, 24.0);
        assert_eq!(row.mem_diff.as_ref().unwrap().median, 268.0);
    }

    #[test]
    fn differences_are_computed_within_each_run() {
        // Run 1: host 70 / guest 46 (diff 24). Run 2: host 40 / guest 38 (diff 2).
        // Reducing each side first would give median(70, 40) - median(46, 38) = 13: wrong pairing.
        let run = |host_cpu: f64, guest_cpu: f64| NodeRun {
            duration_s: 1.0,
            guest: guest_run(&[(0.0, 100.0, guest_cpu), (1.0, 100.0, guest_cpu)]),
            host: host_run(&[(0.0, host_cpu, 500.0), (1.0, host_cpu, 500.0)]),
        };
        let runs = vec![run(70.0, 46.0), run(40.0, 38.0), run(60.0, 40.0)];

        let row = summarize(&runs).rows.into_iter().find(|r| r.elapsed_s == 0.0).unwrap();
        let diff = row.cpu_diff.unwrap();

        assert_eq!(diff.n, 3);
        assert_eq!(diff.median, 20.0);
        assert_eq!(diff.min, 2.0);
        assert_eq!(diff.max, 24.0);
    }

    #[test]
    fn falls_back_to_guest_cpu_without_host_data() {
        let runs = vec![NodeRun {
            duration_s: 1.0,
            guest: guest_run(&[(0.0, 100.0, 12.0), (1.0, 100.0, 14.0)]),
            host: Vec::new(),
        }];

        let summary = summarize(&runs);
        let row = &summary.rows.iter().find(|r| r.elapsed_s == 0.0).unwrap();

        assert_eq!(summary.cpu_source, CpuSource::Guest);
        assert_eq!(row.cpu.as_ref().unwrap().median, 12.0);
        assert!(row.cpu_diff.is_none());
        assert!(row.mem_diff.is_none());
    }

    #[test]
    fn host_only_nodes_are_summarized() {
        // Clients and servers have no guest-side resource log
        let runs = vec![NodeRun { duration_s: 1.0, guest: Vec::new(), host: host_run(&[(0.0, 30.0, 900.0), (1.0, 32.0, 900.0)]) }];

        let summary = summarize(&runs);
        let row = summary.rows.iter().find(|r| r.elapsed_s == 1.0).unwrap();

        assert_eq!(summary.cpu_source, CpuSource::Host);
        assert_eq!(row.cpu.as_ref().unwrap().median, 32.0);
        assert!(row.mem.is_none());
        assert!(row.cpu_diff.is_none());
    }

    #[test]
    fn summary_header_and_records_have_the_same_width() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("R1.csv");
        let runs = vec![NodeRun {
            duration_s: 1.0,
            guest: guest_run(&[(0.0, 100.0, 10.0), (1.0, 100.0, 10.0)]),
            host: host_run(&[(0.0, 20.0, 500.0), (1.0, 20.0, 500.0)]),
        }];

        write_summary_csv(&path, &summarize(&runs)).unwrap();

        let mut reader = csv::Reader::from_path(&path).unwrap();
        let width = reader.headers().unwrap().len();
        assert_eq!(width, 2 + 6 * 5 + 1);
        for record in reader.records() {
            assert_eq!(record.unwrap().len(), width);
        }
    }
}
