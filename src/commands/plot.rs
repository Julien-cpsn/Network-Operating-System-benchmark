use std::fs;
use std::path::PathBuf;
use std::process::{exit, Command};
use tracing::{error, info, warn};
use crate::args::plot::PlotCommand;
use crate::utils::files::plot_dir::PLOT_DIR_PATH;
use crate::utils::files::results_dir::RESULT_DIR_PATH;
use crate::utils::results::extract::{extract_experiments_and_results, ExperimentAndResults};
use crate::utils::results::flent::summary::SUMMARY_FOLDER_NAME;
use crate::utils::utils::{extract_and_sort_common_parts, filter_routers};


const TARGET: &str = "plot";

#[derive(Debug)]
struct Plot {
    pub plot_type: &'static str,
    pub plot_name: &'static str,
    pub support_tests: &'static [&'static str],
    pub adjustment: i32,
    pub additional_args: &'static [&'static str]
}

const PLOTS: [Plot; 4] = [
    Plot {
        plot_type: "box_totals",
        plot_name: "box_plot_of_totals",
        support_tests: &["rrul", "voip-rrul"],
        adjustment: 0,
        additional_args: &[],
    },
    Plot {
        plot_type: "icmp_cdf",
        plot_name: "icmp_cdf",
        support_tests: &["rrul", "voip-rrul"],
        adjustment: 6,
        additional_args: &[]
    },
    Plot {
        plot_type: "voip_induced_delay_box",
        plot_name: "voip_induced_delay_box_plot",
        support_tests: &["voip", "voip-rrul"],
        adjustment: 10,
        additional_args: &[],
    },
    Plot {
        plot_type: "ellipsis",
        plot_name: "throughput_latency_ellipse",
        support_tests: &["rrul", "voip-rrul"],
        adjustment: 0,
        additional_args: &["--bounds-x=15,0", "--bounds-y=15000,0"]
    },
];

const EXTENSION: [&str; 2] = ["png", "svg"];

pub fn plot(plot_command: PlotCommand) -> anyhow::Result<()> {
    let experiment_results = extract_experiments_and_results(&plot_command.experiment_selection, true)?;

    if experiment_results.is_empty() {
        warn!(target: TARGET, "No experiment results found. Please use the \"compute\" command to pre-compute run results");
        exit(1);
    }

    let plot_legends: Vec<&Vec<String>> = experiment_results.iter().map(|e| &e.experiment_keywords).collect();

    let (common_words, non_common_words) = extract_and_sort_common_parts(plot_legends);

    let plot_output_directory_name = format!("merged_{}_{}", non_common_words.join("-"), common_words.join("-"));
    let plot_output_directory_path = PLOT_DIR_PATH.join(plot_output_directory_name);

    if !plot_output_directory_path.exists() {
        fs::create_dir(&plot_output_directory_path)?;
    }

    plot_flent(&plot_command, &plot_output_directory_path, &experiment_results, &common_words, non_common_words)?;
    plot_resources(&plot_output_directory_path, &experiment_results, &common_words)?;

    Ok(())
}

fn plot_flent(plot_command: &PlotCommand, plot_output_directory_path: &PathBuf, experiment_results: &Vec<ExperimentAndResults>, common_words: &Vec<String>, non_common_words: Vec<String>) -> anyhow::Result<()> {
    let notes: Vec<&str> = match plot_command.plot_command.hide_note {
        false => common_words.iter().map(|s| s.as_str()).collect(),
        true => Vec::new(),
    };

    let mut flent_additional_args = Vec::new();

    let flent_legends_to_remove: Vec<String> = common_words
        .iter()
        .map(|s| [String::from("--filter-regexp"), format!("{},?", s.as_str())])
        .flatten()
        .collect();
    let flent_legends_to_remove: Vec<&str> = flent_legends_to_remove.iter().map(|s| s.as_str()).collect();

    let flent_legends_to_modify: Vec<String> = non_common_words
        .iter()
        .map(|s| [String::from("--replace-legend"), format!("\"{}\"=\"{} ,\"", s.as_str(), s.as_str())])
        .flatten()
        .collect();
    let flent_legends_to_modify: Vec<&str> = flent_legends_to_modify.iter().map(|s| s.as_str()).collect();

    if plot_command.plot_command.log_scale {
        flent_additional_args.push("--log-scale-y");
        flent_additional_args.push("log10");
    }

    if plot_command.plot_command.no_title {
        flent_additional_args.push("--no-title");
    }

    for plot in PLOTS {
        let mut flent_input_files_args = Vec::new();

        for experiment in experiment_results {
            for (_, run_paths) in &experiment.result_paths_per_run_per_test {
                for (test, result_paths) in run_paths {
                    if !plot.support_tests.contains(&test.as_str()) {
                        continue;
                    }

                    for result_path in result_paths {

                        if !result_path.file_name().unwrap().to_str().unwrap().starts_with(test) {
                            continue;
                        }

                        flent_input_files_args.push("-i");
                        flent_input_files_args.push(result_path.to_str().unwrap());
                    }
                }
            }
        }

        if flent_input_files_args.is_empty() {
            error!(target: TARGET, "No files found for plot");
            exit(1);
        }

        let flent_notes = adjust_note(&notes, plot.adjustment);

        for extension in EXTENSION {
            let plot_path = plot_output_directory_path.join(format!("{}.{}", plot.plot_name, extension));

            let flent_args = [
                vec![
                    "-o", plot_path.to_str().unwrap(),
                    "-p", plot.plot_type,
                    "--skip-missing-series",
                    "--filter-regexp", ",",
                    "--filter-regexp", "_",
                    "--filter-regexp", "Ping \\(ms\\) --",
                    "--filter-regexp", "ICMP - ",
                    "--filter-regexp", "(?:[0-9]{1,3}\\.){3}[0-9]{1,3}",
                    "--no-annotation",
                    "--no-markers",
                    "--no-hover-highlight",
                    "--fallback-layout",
                    "--figure-dpi", "150"
                ],
                flent_input_files_args.clone(),
                flent_legends_to_remove.clone(),
                flent_legends_to_modify.clone(),
                flent_additional_args.clone(),
                vec![
                    "--figure-note",
                    &flent_notes
                ],
                plot.additional_args.to_vec()
            ]
                .concat();


            let mut command = Command::new("flent");
            let command = command.args(flent_args);
            command.spawn()?;

            info!(target: TARGET, "Plotted {}", plot_path.file_name().unwrap().to_str().unwrap());
        }
    }

    Ok(())
}

fn plot_resources(plot_output_directory_path: &PathBuf, experiment_results: &Vec<ExperimentAndResults>, common_words: &Vec<String>) -> anyhow::Result<()> {
    let comparison_mode = experiment_results.len() > 1;

    let output_path = plot_output_directory_path.join("resources.svg");
    let mut args = vec![
        format!("{}/src/utils/resource_plot.py", env!("CARGO_MANIFEST_DIR")),
        output_path.to_str().unwrap().to_string()
    ];

    for experiment_result in experiment_results {
        let adjusted_experiment_name = common_words
            .iter()
            .fold(
                experiment_result.experiment.experiment_name.clone(),
                |i, c|
                    i
                        .replace(&format!(",{c}"), " ")
                        .replace(&format!("{c},"), " ")
                        .replace(c, "")
                        .replace("  ", " ")
            )
            .replace("  ", " ")
            .replace(',', "");

        for router_name in filter_routers(&experiment_result.experiment.network.nodes).keys() {
            let router_log_file_path = RESULT_DIR_PATH
                .join(&experiment_result.experiment.experiment_name)
                .join(SUMMARY_FOLDER_NAME)
                .join(format!("{router_name}.csv"));

            if !router_log_file_path.exists() {
                continue;
            }

            if comparison_mode {
                args.push(format!("{} {}:{}", adjusted_experiment_name, router_name, router_log_file_path.display()));
            }
            else {
                args.push(format!("{}:{}", router_name, router_log_file_path.display().to_string()));
            }
        }
    }

    Command::new("python").args(&args).spawn()?;

    Ok(())
}

fn adjust_note(notes: &[&str], adjustment: i32) -> String {
    const DEFAULT_PADDING: i32 = 190;

    notes
        .iter()
        .enumerate()
        .map(|(index, note)| {
            let width = note.chars().count() as i32;

            let mut padding = DEFAULT_PADDING
                - adjustment
                - width / 2
                - index as i32;

            if index == 0 {
                padding -= 1;
            }


            format!("{}{}\n", " ".repeat(padding.max(0) as usize), note)
        })
        .collect()
}