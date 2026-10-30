//! Host-side resource monitor of the QEMU VMs (Rust port of `host_resources.sh`).
//!
//! Every second, for every monitored VM, the per-thread CPU time of its QEMU process is read
//! from `/proc` and split into:
//! - `guest_pct`: time the vCPU threads spent executing guest code (`guest_time` of `/proc/<tid>/stat`)
//! - `exit_pct`: the rest of the vCPU threads' time, spent in the host on the guest's behalf
//!   (VM exits, interrupt injection)
//! - `vhost_pct`: vhost-net workers (QEMU threads named `vhost-<pid>` on host kernels >= 6.4,
//!   kernel threads of the same name before)
//! - `qemu_pct`: every other QEMU thread (main loop, UDP socket netdevs, e1000/rtl8139 emulation)
//!
//! CPU percentages are normalised to the VM's vCPU capacity (100 % = all its vCPUs fully busy),
//! except `busiest_thread_pct`, which is the busiest single thread in % of ONE core: around
//! 100 %, that thread is the bottleneck even when the averages look fine.
//! `busy_vhost_threads` counts the vhost workers busy at least 10 % of the interval: with multiqueue
//! VirtIO NICs (one worker per queue pair), it shows how many queue pairs the guest actually uses.
//!
//! Packet rates come from the VM's taps (`/sys/class/net/<tap>/statistics`): a tap's rx is the
//! VM's tx. `drop_pps` counts the packets the host could not hand to the VM (tap `tx_dropped`),
//! `ns_per_packet` is the total host CPU time per packet leaving the VM (only above
//! [`MIN_PPS_FOR_COST`], below that it mostly measures the VM's idle cost).
//!
//! Deltas are computed per thread, so threads that start or exit between two samples (QEMU
//! worker pools) cannot produce negative values. The flip side: the time of a thread that
//! exits before the next sample is lost for its last interval, and a thread that lives less
//! than one interval is never seen. Only short-lived QEMU workers (block I/O thread pool) are
//! concerned, never vCPU nor vhost threads, so `qemu_pct` may slightly under-count. Requires QEMU to be started with
//! `-name <name>,debug-threads=on`, so that vCPU threads are named `CPU <n>/KVM`.
//!
//! One CSV per VM is written to `<run>/host_resources/<node>.csv`.

use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};
use anyhow::Context;
use chrono::{SecondsFormat, Utc};
use tracing::{debug, info, warn};
use crate::utils::qemu::vm::{Connection, QemuVm};

const TARGET: &str = "host_monitor";

/// Folder of a run that holds the host-side resource files, one CSV per node
pub const HOST_RESOURCES_DIR_NAME: &str = "host_resources";

pub const COL_TIMESTAMP: &str = "timestamp";
/// Host-measured guest execution time, in % of the vCPU capacity ("Guest (perc)" of the script)
pub const COL_GUEST_PCT: &str = "guest_pct";
/// Resident anonymous memory of the QEMU process (pages of the guest touched at least once)
pub const COL_RSS_ANON_MIB: &str = "rss_anon_mib";

const HEADER: [&str; 15] = [
    COL_TIMESTAMP,
    "vcpu",
    COL_GUEST_PCT,
    "exit_pct",
    "vhost_pct",
    "qemu_pct",
    "total_pct",
    COL_RSS_ANON_MIB,
    "vm_tx_pps",
    "vm_rx_pps",
    "drop_pps",
    "ns_per_packet",
    "busiest_thread_pct",
    "busiest_thread",
    "busy_vhost_threads",
];

const SAMPLING_INTERVAL: Duration = Duration::from_secs(1);
/// A vhost worker running at least this share of the interval counts in `busy_vhost_threads`
const BUSY_VHOST_SHARE: f64 = 0.10;
/// Below this rate, `ns_per_packet` is left empty
const MIN_PPS_FOR_COST: f64 = 1000.0;
/// Field of `/proc/<pid>/stat` holding `guest_time`, counted from 1 (see proc(5))
const STAT_FIELD_GUEST_TIME: usize = 43;
const STAT_FIELD_UTIME: usize = 14;
const STAT_FIELD_STIME: usize = 15;

/// A VM to monitor: its QEMU process and its taps
#[derive(Debug, Clone)]
pub struct MonitoredVm {
    pub name: String,
    pub pid: u32,
    pub taps: Vec<String>,
}

impl MonitoredVm {
    /// `None` if the VM is not running
    pub fn from_vm(vm: &QemuVm) -> Option<MonitoredVm> {
        let taps = vm.adapters
            .iter()
            .filter_map(|adapter| match &adapter.connection {
                Some(Connection::Tap { ifname, .. }) => Some(ifname.clone()),
                _ => None,
            })
            .collect();

        Some(MonitoredVm { name: vm.name.clone(), pid: vm.pid?, taps })
    }
}

/// Cumulative counters of one thread
#[derive(Debug, Clone, Copy, PartialEq)]
struct ThreadTimes {
    runtime_ns: u64,
    guest_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ThreadKind {
    Vcpu,
    Vhost,
    Qemu,
}

impl ThreadKind {
    fn of(comm: &str) -> ThreadKind {
        let is_vcpu = comm
            .strip_prefix("CPU ")
            .and_then(|rest| rest.strip_suffix("/KVM"))
            .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()));

        if is_vcpu {
            ThreadKind::Vcpu
        }
        else if comm.starts_with("vhost-") {
            ThreadKind::Vhost
        }
        else {
            ThreadKind::Qemu
        }
    }
}

/// CPU time spent by a VM's threads during one interval
#[derive(Debug, Clone, Default, PartialEq)]
struct CpuDelta {
    vcpu_ns: u64,
    guest_ticks: u64,
    vhost_ns: u64,
    qemu_ns: u64,
    nb_vcpu: u32,
    busiest_ns: u64,
    busiest_thread: String,
    /// Runtime of each vhost worker during the interval
    vhost_runtimes_ns: Vec<u64>,
}

impl CpuDelta {
    /// `previous` holds the counters of the last sample, `key` identifies the thread
    fn add(&mut self, previous: &HashMap<String, ThreadTimes>, key: &str, name: &str, kind: ThreadKind, times: ThreadTimes) {
        // A thread seen for the first time was created during the interval: all its time belongs to it
        let before = previous.get(key).copied().unwrap_or(ThreadTimes { runtime_ns: 0, guest_ticks: 0 });
        let runtime = times.runtime_ns.saturating_sub(before.runtime_ns);

        match kind {
            ThreadKind::Vcpu => {
                self.vcpu_ns += runtime;
                self.guest_ticks += times.guest_ticks.saturating_sub(before.guest_ticks);
                self.nb_vcpu += 1;
            },
            ThreadKind::Vhost => {
                self.vhost_ns += runtime;
                self.vhost_runtimes_ns.push(runtime);
            },
            ThreadKind::Qemu => self.qemu_ns += runtime,
        }

        if runtime > self.busiest_ns {
            self.busiest_ns = runtime;
            self.busiest_thread = name.replace(' ', "_");
        }
    }
}

/// Packet counters of a VM's taps, from the VM's point of view
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct PacketCounters {
    from_vm: u64,
    to_vm: u64,
    dropped_to_vm: u64,
}

/// One line of the output file
#[derive(Debug, Clone, PartialEq)]
struct HostRow {
    nb_vcpu: u32,
    guest_pct: f64,
    exit_pct: f64,
    vhost_pct: f64,
    qemu_pct: f64,
    total_pct: f64,
    rss_anon_mib: Option<f64>,
    vm_tx_pps: f64,
    vm_rx_pps: f64,
    drop_pps: f64,
    ns_per_packet: Option<f64>,
    busiest_thread_pct: f64,
    busiest_thread: String,
    /// vhost workers doing real work: with multiqueue VirtIO NICs, shows how many queue pairs the guest uses
    busy_vhost_threads: u32,
}

impl HostRow {
    fn new(cpu: &CpuDelta, packets: &PacketCounters, elapsed_ns: f64, clock_ticks_per_s: f64, rss_anon_kib: Option<u64>) -> HostRow {
        let capacity_ns = if cpu.nb_vcpu > 0 { elapsed_ns * cpu.nb_vcpu as f64 } else { elapsed_ns };
        let pct = |ns: f64| 100.0 * ns / capacity_ns;

        // guest_time has the resolution of a clock tick, it cannot exceed the vCPU threads' runtime
        let vcpu_ns = cpu.vcpu_ns as f64;
        let guest_ns = (cpu.guest_ticks as f64 * 1e9 / clock_ticks_per_s).min(vcpu_ns);
        let total_ns = vcpu_ns + cpu.vhost_ns as f64 + cpu.qemu_ns as f64;

        let elapsed_s = elapsed_ns / 1e9;
        let vm_tx_pps = packets.from_vm as f64 / elapsed_s;

        HostRow {
            nb_vcpu: cpu.nb_vcpu,
            guest_pct: pct(guest_ns),
            exit_pct: pct(vcpu_ns - guest_ns),
            vhost_pct: pct(cpu.vhost_ns as f64),
            qemu_pct: pct(cpu.qemu_ns as f64),
            total_pct: pct(total_ns),
            rss_anon_mib: rss_anon_kib.map(|kib| kib as f64 / 1024.0),
            vm_tx_pps,
            vm_rx_pps: packets.to_vm as f64 / elapsed_s,
            drop_pps: packets.dropped_to_vm as f64 / elapsed_s,
            ns_per_packet: (vm_tx_pps >= MIN_PPS_FOR_COST).then(|| total_ns / packets.from_vm as f64),
            busiest_thread_pct: 100.0 * cpu.busiest_ns as f64 / elapsed_ns,
            busiest_thread: if cpu.busiest_thread.is_empty() { String::from("-") } else { cpu.busiest_thread.clone() },
            busy_vhost_threads: cpu.vhost_runtimes_ns.iter().filter(|&&ns| ns as f64 >= BUSY_VHOST_SHARE * elapsed_ns).count() as u32,
        }
    }

    fn record(&self, timestamp: &str) -> Vec<String> {
        let optional = |value: Option<f64>, precision: usize| value.map_or_else(String::new, |v| format!("{v:.precision$}"));

        vec![
            timestamp.to_string(),
            self.nb_vcpu.to_string(),
            format!("{:.2}", self.guest_pct),
            format!("{:.2}", self.exit_pct),
            format!("{:.2}", self.vhost_pct),
            format!("{:.2}", self.qemu_pct),
            format!("{:.2}", self.total_pct),
            optional(self.rss_anon_mib, 1),
            format!("{:.0}", self.vm_tx_pps),
            format!("{:.0}", self.vm_rx_pps),
            format!("{:.0}", self.drop_pps),
            optional(self.ns_per_packet, 0),
            format!("{:.2}", self.busiest_thread_pct),
            self.busiest_thread.clone(),
            self.busy_vhost_threads.to_string(),
        ]
    }
}

/// `field` (counted from 1, as in proc(5)) of a `/proc/<pid>/stat` line. The command name
/// (field 2) may contain spaces and parentheses, so fields are counted after its last ')'.
fn stat_field(stat: &str, field: usize) -> Option<u64> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    after_comm.split_whitespace().nth(field.checked_sub(3)?)?.parse().ok()
}

fn read_thread_times(thread_dir: &Path, clock_ticks_per_s: f64) -> Option<ThreadTimes> {
    let stat = fs::read_to_string(thread_dir.join("stat")).ok()?;
    let guest_ticks = stat_field(&stat, STAT_FIELD_GUEST_TIME).unwrap_or(0);

    // schedstat is in nanoseconds; without CONFIG_SCHED_INFO, fall back to utime + stime (clock ticks)
    let runtime_ns = match fs::read_to_string(thread_dir.join("schedstat")).ok().and_then(|s| s.split_whitespace().next()?.parse().ok()) {
        Some(ns) => ns,
        None => {
            let ticks = stat_field(&stat, STAT_FIELD_UTIME)? + stat_field(&stat, STAT_FIELD_STIME)?;
            (ticks as f64 * 1e9 / clock_ticks_per_s) as u64
        }
    };

    Some(ThreadTimes { runtime_ns, guest_ticks })
}

fn read_comm(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("comm")).ok().map(|comm| comm.trim_end_matches('\n').to_string())
}

/// On host kernels < 6.4, vhost-net workers are kernel threads named `vhost-<QEMU pid>`,
/// outside the QEMU process. Returns their pids per QEMU pid (empty on newer kernels).
fn vhost_kernel_threads() -> HashMap<u32, Vec<u32>> {
    let mut threads: HashMap<u32, Vec<u32>> = HashMap::new();

    let Ok(entries) = fs::read_dir("/proc") else {
        return threads;
    };

    for entry in entries.filter_map(Result::ok) {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };

        if let Some(owner) = read_comm(&entry.path()).and_then(|comm| comm.strip_prefix("vhost-")?.parse::<u32>().ok()) {
            threads.entry(owner).or_default().push(pid);
        }
    }

    threads
}

fn read_tap_counter(tap: &str, counter: &str) -> Option<u64> {
    fs::read_to_string(format!("/sys/class/net/{tap}/statistics/{counter}")).ok()?.trim().parse().ok()
}

fn read_packet_counters(taps: &[String]) -> PacketCounters {
    let mut counters = PacketCounters::default();

    for tap in taps {
        counters.from_vm += read_tap_counter(tap, "rx_packets").unwrap_or(0);
        counters.to_vm += read_tap_counter(tap, "tx_packets").unwrap_or(0);
        counters.dropped_to_vm += read_tap_counter(tap, "tx_dropped").unwrap_or(0);
    }

    counters
}

fn read_rss_anon_kib(pid: u32) -> Option<u64> {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("RssAnon:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// `getconf CLK_TCK`, the unit of the tick counters of `/proc/<pid>/stat` (100 on Linux in practice)
fn clock_ticks_per_second() -> f64 {
    Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok()?.trim().parse::<f64>().ok())
        .filter(|ticks| *ticks > 0.0)
        .unwrap_or(100.0)
}

/// State of one monitored VM between two samples
struct VmMonitor {
    vm: MonitoredVm,
    threads: HashMap<String, ThreadTimes>,
    packets: PacketCounters,
    writer: csv::Writer<File>,
    alive: bool,
}

impl VmMonitor {
    fn new(vm: MonitoredVm, output_dir: &Path) -> anyhow::Result<VmMonitor> {
        let path = output_dir.join(format!("{}.csv", vm.name));
        let mut writer = csv::Writer::from_path(&path).with_context(|| format!("cannot create {}", path.display()))?;
        writer.write_record(HEADER)?;
        writer.flush()?;

        if vm.taps.is_empty() {
            info!(target: TARGET, "{} (pid {}): no tap, packet rates will be 0 (UDP link backend)", vm.name, vm.pid);
        }
        else {
            debug!(target: TARGET, "{} (pid {}): taps {}", vm.name, vm.pid, vm.taps.join(", "));
        }

        Ok(VmMonitor { vm, threads: HashMap::new(), packets: PacketCounters::default(), writer, alive: true })
    }

    /// Reads the counters, returns the CPU time spent since the previous call
    fn sample(&mut self, vhost_threads: &HashMap<u32, Vec<u32>>, clock_ticks_per_s: f64) -> Option<(CpuDelta, PacketCounters)> {
        let task_dir = PathBuf::from(format!("/proc/{}/task", self.vm.pid));
        let entries = fs::read_dir(&task_dir).ok()?;

        let mut delta = CpuDelta::default();
        let mut current = HashMap::new();

        for entry in entries.filter_map(Result::ok) {
            let thread_dir = entry.path();
            // The thread may exit while being read
            let (Some(comm), Some(times)) = (read_comm(&thread_dir), read_thread_times(&thread_dir, clock_ticks_per_s)) else {
                continue;
            };
            let key = format!("t{}", entry.file_name().to_string_lossy());

            delta.add(&self.threads, &key, &comm, ThreadKind::of(&comm), times);
            current.insert(key, times);
        }

        for kernel_thread in vhost_threads.get(&self.vm.pid).into_iter().flatten() {
            let thread_dir = PathBuf::from(format!("/proc/{kernel_thread}"));
            let Some(times) = read_thread_times(&thread_dir, clock_ticks_per_s) else {
                continue;
            };
            let key = format!("k{kernel_thread}");

            delta.add(&self.threads, &key, &format!("vhost-kthread-{kernel_thread}"), ThreadKind::Vhost, times);
            current.insert(key, times);
        }

        self.threads = current;

        let packets = read_packet_counters(&self.vm.taps);
        let packets_delta = PacketCounters {
            from_vm: packets.from_vm.saturating_sub(self.packets.from_vm),
            to_vm: packets.to_vm.saturating_sub(self.packets.to_vm),
            dropped_to_vm: packets.dropped_to_vm.saturating_sub(self.packets.dropped_to_vm),
        };
        self.packets = packets;

        Some((delta, packets_delta))
    }
}

/// Samples the VMs every second until `stop_monitoring` is set.
///
/// Blocking: run it on its own thread (`tokio::task::spawn_blocking`), so that the console
/// interactions, which block tokio workers, cannot delay the samples.
pub fn host_monitor_task(experiment_run_path: PathBuf, vms: Vec<MonitoredVm>, stop_monitoring: Arc<AtomicBool>) -> anyhow::Result<()> {
    if vms.is_empty() {
        return Ok(());
    }

    let output_dir = experiment_run_path.join(HOST_RESOURCES_DIR_NAME);

    let clock_ticks_per_s = clock_ticks_per_second();
    let mut monitors = vms
        .into_iter()
        .map(|vm| VmMonitor::new(vm, &output_dir))
        .collect::<anyhow::Result<Vec<_>>>()?;

    info!(target: TARGET, "Running host monitoring of {} VM(s)", monitors.len());

    // Baseline: the first sample only initializes the counters
    let vhost_threads = vhost_kernel_threads();
    for monitor in &mut monitors {
        monitor.sample(&vhost_threads, clock_ticks_per_s);
    }
    let mut last_sample = Instant::now();
    let mut next_sample = last_sample;

    while !stop_monitoring.load(Ordering::Relaxed) {
        // Fixed cadence; after a late sample, the next one is scheduled from now (no burst to catch up)
        next_sample = (next_sample + SAMPLING_INTERVAL).max(Instant::now());
        sleep(next_sample.saturating_duration_since(Instant::now()));

        let now = Instant::now();
        let elapsed_ns = now.duration_since(last_sample).as_nanos() as f64;
        last_sample = now;
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
        let vhost_threads = vhost_kernel_threads();

        for monitor in &mut monitors {
            if !monitor.alive {
                continue;
            }

            let Some((cpu, packets)) = monitor.sample(&vhost_threads, clock_ticks_per_s) else {
                warn!(target: TARGET, "{} (pid {}) is not running anymore, host monitoring stopped for it", monitor.vm.name, monitor.vm.pid);
                monitor.alive = false;
                continue;
            };

            let row = HostRow::new(&cpu, &packets, elapsed_ns, clock_ticks_per_s, read_rss_anon_kib(monitor.vm.pid));
            monitor.writer.write_record(row.record(&timestamp))?;
            monitor.writer.flush()?;
        }
    }

    info!(target: TARGET, "End host monitoring");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VCPU_STAT: &str = "4242 (CPU 0/KVM) S 4200 4200 4200 0 -1 4194624 30 0 0 0 1500 2500 0 0 20 0 9 0 100 6000000000 130000 18446744073709551615 0 0 0 0 0 0 0 4096 0 0 0 0 -1 3 0 0 0 1234 0 0 0 0 0 0 0 0 0";

    #[test]
    fn stat_fields_are_counted_after_the_command_name() {
        assert_eq!(stat_field(VCPU_STAT, STAT_FIELD_UTIME), Some(1500));
        assert_eq!(stat_field(VCPU_STAT, STAT_FIELD_STIME), Some(2500));
        assert_eq!(stat_field(VCPU_STAT, STAT_FIELD_GUEST_TIME), Some(1234));

        // A command name containing ") " must not shift the fields
        let tricky = VCPU_STAT.replace("(CPU 0/KVM)", "(we) ird) name)");
        assert_eq!(stat_field(&tricky, STAT_FIELD_GUEST_TIME), Some(1234));
    }

    #[test]
    fn threads_are_classified_by_name() {
        assert_eq!(ThreadKind::of("CPU 0/KVM"), ThreadKind::Vcpu);
        assert_eq!(ThreadKind::of("CPU 12/KVM"), ThreadKind::Vcpu);
        assert_eq!(ThreadKind::of("CPU /KVM"), ThreadKind::Qemu);
        assert_eq!(ThreadKind::of("vhost-1234"), ThreadKind::Vhost);
        assert_eq!(ThreadKind::of("qemu-system-x86"), ThreadKind::Qemu);
        assert_eq!(ThreadKind::of("worker"), ThreadKind::Qemu);
    }

    #[test]
    fn deltas_are_per_thread_and_never_negative() {
        let previous = HashMap::from([
            (String::from("t1"), ThreadTimes { runtime_ns: 1_000, guest_ticks: 10 }),
            // A thread that exited: it must not reduce the totals
            (String::from("t9"), ThreadTimes { runtime_ns: 5_000_000, guest_ticks: 0 }),
        ]);

        let mut delta = CpuDelta::default();
        delta.add(&previous, "t1", "CPU 0/KVM", ThreadKind::Vcpu, ThreadTimes { runtime_ns: 3_000, guest_ticks: 12 });
        // New thread: all its runtime belongs to this interval
        delta.add(&previous, "t2", "worker", ThreadKind::Qemu, ThreadTimes { runtime_ns: 500, guest_ticks: 0 });

        assert_eq!(delta.vcpu_ns, 2_000);
        assert_eq!(delta.guest_ticks, 2);
        assert_eq!(delta.qemu_ns, 500);
        assert_eq!(delta.nb_vcpu, 1);
        assert_eq!(delta.busiest_thread, "CPU_0/KVM");
    }

    #[test]
    fn row_splits_guest_and_exit_time() {
        // 2 vCPUs during 1 s: 1.4 s of vCPU runtime, of which 1.2 s in the guest (120 ticks at 100 Hz)
        let cpu = CpuDelta {
            vcpu_ns: 1_400_000_000,
            guest_ticks: 120,
            vhost_ns: 600_000_000,
            qemu_ns: 0,
            nb_vcpu: 2,
            busiest_ns: 800_000_000,
            busiest_thread: String::from("vhost-1"),
            // 3 workers: two busy (>= 10 % of 1 s), one almost idle
            vhost_runtimes_ns: vec![450_000_000, 140_000_000, 10_000_000],
        };
        let packets = PacketCounters { from_vm: 1_000_000, to_vm: 999_000, dropped_to_vm: 10 };

        let row = HostRow::new(&cpu, &packets, 1e9, 100.0, Some(2048));

        assert!((row.guest_pct - 60.0).abs() < 1e-9);
        assert!((row.exit_pct - 10.0).abs() < 1e-9);
        assert!((row.vhost_pct - 30.0).abs() < 1e-9);
        assert!((row.total_pct - 100.0).abs() < 1e-9);
        assert_eq!(row.rss_anon_mib, Some(2.0));
        assert_eq!(row.ns_per_packet, Some(2_000.0));
        assert!((row.busiest_thread_pct - 80.0).abs() < 1e-9);
        assert_eq!(row.busy_vhost_threads, 2);
    }

    #[test]
    fn row_clamps_tick_resolution_and_hides_idle_cost() {
        // guest_time has a 10 ms resolution and may slightly exceed the measured runtime
        let cpu = CpuDelta { vcpu_ns: 5_000_000, guest_ticks: 1, nb_vcpu: 2, ..CpuDelta::default() };
        let packets = PacketCounters { from_vm: 140, to_vm: 140, dropped_to_vm: 0 };

        let row = HostRow::new(&cpu, &packets, 1e9, 100.0, None);

        assert!(row.exit_pct >= 0.0);
        assert_eq!(row.ns_per_packet, None);
        assert_eq!(row.busiest_thread, "-");
        assert_eq!(row.record("t")[7], "");
    }

    #[test]
    fn header_matches_records() {
        let row = HostRow::new(&CpuDelta::default(), &PacketCounters::default(), 1e9, 100.0, None);
        assert_eq!(row.record("t").len(), HEADER.len());
    }
}
