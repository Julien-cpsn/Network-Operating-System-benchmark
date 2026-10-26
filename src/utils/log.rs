use std::fs;
use std::fs::OpenOptions;
use crate::ARGS;
use std::io::stdout;
use std::path::PathBuf;
use indexmap::IndexMap;
use tracing::{subscriber, Dispatch, Level, debug};
use tracing::metadata::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;
use crate::models::nodes::node::Node;
use crate::utils::files::dir::subtract_project_root;

const TARGET: &str = "log";

pub const EXPERIMENT_LOG_FILE_NAME: &str = "experiment";

pub fn setup_global_logger() -> anyhow::Result<()> {
    let verbosity = match (ARGS.verbosity.is_present(), ARGS.verbosity.tracing_level()) {
        (true, level) => level,
        _ => Some(Level::DEBUG)
    };

    if let Some(level) = verbosity {
        let filter = Targets::new()
            .with_default(level)
            .with_target("reqwest", LevelFilter::ERROR)
            .with_target("hyper_util", LevelFilter::ERROR);

        let subscriber = Registry::default()
            .with(filter)
            .with(
                Layer::new().with_writer(stdout.with_max_level(level))
            );

        subscriber::set_global_default(subscriber)?;
    }

    Ok(())
}

pub fn setup_experiment_logger(experiment_path: &PathBuf, name: &str) -> anyhow::Result<(Dispatch, WorkerGuard)> {
    let verbosity = match (ARGS.verbosity.is_present(), ARGS.verbosity.tracing_level()) {
        (true, Some(level)) => level,
        _ => Level::DEBUG
    };

    let log_dir_path = experiment_path.join(format!("{name}.log"));
    let log_dir_file = OpenOptions::new()
        .create(true)
        .append(true)
        .write(true)
        .open(&log_dir_path)?;

    let (log_file_writer, file_guard) = tracing_appender::non_blocking(log_dir_file);
    let log_file_writer = log_file_writer.with_max_level(Level::TRACE);

    let filter = Targets::new()
        .with_default(Level::TRACE)
        .with_target("reqwest", LevelFilter::ERROR)
        .with_target("hyper_util", LevelFilter::ERROR);

    let subscriber = Registry::default()
        .with(filter)
        .with(Layer::new()
            .with_writer(stdout.with_max_level(verbosity))
        )
        .with(
            Layer::new()
                .with_writer(log_file_writer)
                .with_ansi(false)
        );

    Ok((Dispatch::new(subscriber), file_guard))
}

pub fn find_and_delete_experiment_log_file(experiment_path: &PathBuf) -> anyhow::Result<()> {
    let experiment_log_path = experiment_path.join(format!("{EXPERIMENT_LOG_FILE_NAME}.log"));

    if experiment_log_path.exists() {
        debug!(target: TARGET, "Deleting existing experiment log file: {}", subtract_project_root(&experiment_log_path));
        fs::remove_file(&experiment_log_path)?;
    }

    Ok(())
}

pub fn find_and_delete_log_files(experiment_path: &PathBuf, nodes: &IndexMap<String, Node>) -> anyhow::Result<()> {
    for node_name in nodes.keys() {
        let path = experiment_path.join(format!("{node_name}.log"));

        if path.exists() {
            debug!(target: TARGET, "Deleting existing node log file: {}", path.display());
            fs::remove_file(&path)?;
        }
    }

    Ok(())
}
