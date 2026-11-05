use std::collections::HashMap;
use std::path::PathBuf;
use tracing::warn;
use crate::args::args::ExperimentSelectionArgs;
use crate::models::experiment::Experiment;
use crate::utils::files::experiments::parse_experiments_files;
use crate::utils::files::results_dir::RESULT_DIR_PATH;
use crate::utils::results::flent::summary::SUMMARY_FOLDER_NAME;

const TARGET: &str = "results";

#[derive(Debug)]
pub struct ExperimentAndResults {
    pub experiment: Experiment,
    pub experiment_keywords: Vec<String>,
    pub result_paths_per_run_per_test: HashMap<String, HashMap<String, Vec<PathBuf>>>,
}

pub fn extract_experiments_and_results(experiment_selection: &ExperimentSelectionArgs, summary: bool) -> anyhow::Result<Vec<ExperimentAndResults>> {
    let experiments = parse_experiments_files(&experiment_selection)?;

    let mut experiment_results: Vec<ExperimentAndResults> = Vec::new();

    for experiment in experiments {
        let mut result_paths = HashMap::new();

        let experiment_results_dir_path = RESULT_DIR_PATH.join(&experiment.experiment_name);

        if !experiment_results_dir_path.exists() {
            //warn!(target: TARGET, "Result files for \"{}\" does not exist", &experiment.experiment_name);
            continue;
        }

        match summary {
            true => extract_summaries(&experiment, &experiment_results_dir_path, &mut result_paths)?,
            false => extract_runs(&experiment, &experiment_results_dir_path, &mut result_paths)?,
        }

        if result_paths.is_empty() {
            continue;
        }

        let experiment_keywords = experiment.experiment_name.split(',').map(|k| k.to_string()).collect();

        let experiment_and_results = ExperimentAndResults {
            experiment,
            experiment_keywords,
            result_paths_per_run_per_test: result_paths,
        };

        experiment_results.push(experiment_and_results);
    }

    experiment_results.sort_by(|a, b| a.experiment.experiment_name.cmp(&b.experiment.experiment_name));

    Ok(experiment_results)
}


fn extract_summaries(experiment: &Experiment, experiment_results_dir_path: &PathBuf, result_paths: &mut HashMap<String, HashMap<String, Vec<PathBuf>>>) -> anyhow::Result<()> {
    let run_entry = result_paths.entry(String::from("summary")).or_insert(HashMap::new());

    for test in &experiment.test_batch {
        let test_entry = run_entry.entry(test.test.clone()).or_insert(Vec::new());

        if !experiment_results_dir_path.exists() {
            warn!(target: TARGET, "Result file for \"{}\" \"{}\" does not exist", &experiment.experiment_name, &test.name);
            continue;
        }

        let result_dir_path = experiment_results_dir_path.join(SUMMARY_FOLDER_NAME).join(&test.name);

        if !result_dir_path.exists() {
            warn!(target: TARGET, "Test \"{}\" results do not exist in experiment {}", &test.name, &experiment.experiment_name);
        }

        for file_path in result_dir_path.read_dir()? {
            let file_path = file_path?;

            if file_path.file_type()?.is_dir() {
                continue;
            }

            if let Some(extension) = file_path.path().to_str() && extension.ends_with(".flent.gz") {
                test_entry.push(file_path.path())
            }
        }
    }

    Ok(())
}

fn extract_runs(experiment: &Experiment, experiment_results_dir_path: &PathBuf, result_paths: &mut HashMap<String, HashMap<String, Vec<PathBuf>>>) -> anyhow::Result<()> {
    for run_path in experiment_results_dir_path
        .read_dir()?
        .filter_map(Result::ok)
        .filter(|d| d.path().is_dir() && d.path().components().last().unwrap().as_os_str().to_string_lossy().starts_with("run_"))
    {
        let run_name = run_path.file_name().to_string_lossy().to_string();
        let run_entry = result_paths.entry(run_name.clone()).or_insert(HashMap::new());

        if !run_path.path().exists() {
            warn!(target: TARGET, "Result file for \"{}\" {} does not exist", &experiment.experiment_name, run_name.replace('_', " "));
            continue;
        }

        for test in &experiment.test_batch {
            let test_entry = run_entry.entry(test.name.clone()).or_insert(Vec::new());

            let result_dir_path = run_path.path().join(&test.name);

            if !result_dir_path.exists() {
                warn!(target: TARGET, "Result file for \"{}\" {} \"{}\" does not exist", &experiment.experiment_name, run_name.replace('_', " "), &test.name);
                continue;
            }

            for file_path in result_dir_path.read_dir()? {
                let file_path = file_path?;

                if file_path.file_type()?.is_dir() {
                    continue;
                }

                if let Some(extension) = file_path.path().to_str() && extension.ends_with(".flent.gz") {
                    test_entry.push(file_path.path())
                }
            }
        }
    }

    Ok(())
}