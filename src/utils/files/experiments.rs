use std::ffi::OsStr;
use std::{env, fs};
use std::path::PathBuf;
use std::process::exit;
use once_cell::sync::Lazy;
use tracing::{error, info};
use walkdir::WalkDir;
use crate::args::args::ExperimentSelectionArgs;
use crate::models::experiment::Experiment;
use crate::utils::files::dir::subtract_project_root;

const TARGET: &str = "files";

pub static EXPERIMENTS_PATH: Lazy<PathBuf> = Lazy::new(|| env::current_dir().unwrap().join("experimentation").join("experiments"));

pub fn parse_experiments_files(experiment_selection: &ExperimentSelectionArgs) -> anyhow::Result<Vec<Experiment>> {
    if !EXPERIMENTS_PATH.exists() {
        error!(
            target: TARGET,
            "Could not find experiment dir \"{}\", make sure you have created or generated experiments first by using the `generate` command",
            subtract_project_root(&EXPERIMENTS_PATH)
        );
        exit(1);
    }

    let mut experiments = Vec::new();

    for file in WalkDir::new(&*EXPERIMENTS_PATH).sort_by_file_name() {
        let file = file?;

        if file.file_type().is_dir() || file.path().extension() != Some(OsStr::new("json")) {
            continue;
        }

        let experiment_content = fs::read_to_string(&file.path())?;
        let experiment: Experiment = serde_json::from_str(&experiment_content)?;

        // Exclude not selected experiment
        
        if let Some(topology_name) = &experiment_selection.topology && !experiment.experiment_name.starts_with(&format!("{topology_name},")) {
            continue;
        }

        if let Some(os) = &experiment_selection.os && !experiment.experiment_name.contains(&format!(",{os},")) {
            continue;
        }

        if let Some(test_batch) = &experiment_selection.test_batch && !experiment.experiment_name.contains(&format!(",{test_batch},")) {
            continue;
        }

        if let Some(resources) = &experiment_selection.resources && !experiment.experiment_name.contains(&format!(",{resources},")) {
            continue;
        }

        if let Some(nic) = &experiment_selection.nic && !experiment.experiment_name.contains(&format!(",{nic},")) {
            continue;
        }

        if let Some(protocol) = &experiment_selection.protocol && !experiment.experiment_name.contains(&format!(",{protocol},")) {
            continue;
        }

        if let Some(failures) = experiment_selection.failures &&
            (failures && experiment.experiment_name.ends_with(",no-failures")
            ||
            (!failures && experiment.experiment_name.ends_with(",failures")))
        {
            continue;
        }


        experiments.push(experiment);
    }

    if experiments.is_empty() {
        info!(target: TARGET, "Experiment list is empty");
    }
    else {
        info!(target: TARGET, "Found experiment list:");

        for key in &experiments {
            info!(target: TARGET, "- {}", key.experiment_name);
        }
    }

    Ok(experiments)
}