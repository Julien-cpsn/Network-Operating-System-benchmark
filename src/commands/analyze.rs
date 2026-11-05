//! Cross-experiment analysis: runs the Python `analysis` package (see `analysis/__init__.py`)
//! on `experimentation/results`, with the same experiment selection as the other commands.
//! Tables, figures and `report.md` are written to `experimentation/analysis/`.

use std::path::Path;
use std::process::Command;
use anyhow::{bail, Context};
use tracing::info;
use crate::args::analyze::AnalyzeCommand;
use crate::utils::files::analysis_dir::ANALYSIS_DIR_PATH;
use crate::utils::files::experiments::EXPERIMENTS_PATH;
use crate::utils::files::results_dir::RESULT_DIR_PATH;

const TARGET: &str = "analyze";

pub fn analyze(analyze_command: AnalyzeCommand) -> anyhow::Result<()> {
    let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let experimentation_dir = project_dir.join("experimentation");
    let selection = &analyze_command.experiment_selection;

    let mut args: Vec<String> = vec![
        "-m".into(), "analysis".into(),
        "--results-dir".into(), RESULT_DIR_PATH.display().to_string(),
        "--experiments-dir".into(), EXPERIMENTS_PATH.display().to_string(),
        "--os-file".into(), experimentation_dir.join("operating_systems.toml").display().to_string(),
        "--families-file".into(), experimentation_dir.join("os_families.toml").display().to_string(),
        "--output-dir".into(), ANALYSIS_DIR_PATH.display().to_string(),
        "--min-coverage".into(), analyze_command.min_coverage.to_string(),
        "--bootstrap".into(), analyze_command.bootstrap.to_string(),
    ];

    // Same values as in the experiment names (Display of the enums)
    let selection_args = [
        ("--os", selection.os.clone()),
        ("--topology", selection.topology.clone()),
        ("--test-batch", selection.test_batch.clone()),
        ("--resources", selection.resources.as_ref().map(|r| r.to_string())),
        ("--nic", selection.nic.as_ref().map(|n| n.to_string())),
        ("--protocol", selection.protocol.as_ref().map(|p| p.to_string())),
        ("--failures", selection.failures.map(|f| f.to_string())),
        ("--jobs", analyze_command.jobs.map(|j| j.to_string())),
        ("--metrics", analyze_command.metrics.clone()),
    ];
    for (flag, value) in selection_args {
        if let Some(value) = value {
            args.push(flag.into());
            args.push(value);
        }
    }
    for (flag, enabled) in [("--reuse", analyze_command.reuse), ("--no-model", analyze_command.no_model), ("--no-plots", analyze_command.no_plots)] {
        if enabled {
            args.push(flag.into());
        }
    }

    // uv provides the dependencies of pyproject.toml; without it, the current Python must have them
    let mut command = match which("uv") {
        true => {
            let mut command = Command::new("uv");
            command.args(["run", "--project"]).arg(project_dir).arg("python");
            command
        }
        false => Command::new("python3"),
    };
    command.args(&args).env("PYTHONPATH", project_dir);

    info!(target: TARGET, "Analyzing results of {}", RESULT_DIR_PATH.display());
    let status = command.status().context("cannot start the Python analysis (is uv or python3 installed?)")?;
    if !status.success() {
        bail!("analysis failed ({status})");
    }
    info!(target: TARGET, "Report: {}", ANALYSIS_DIR_PATH.join("report.md").display());
    Ok(())
}

fn which(binary: &str) -> bool {
    Command::new(binary).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}
