use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool};
use tracing::{error, info};
use crate::models::os_command::OsCommand;
use crate::utils::os_commands::execute::execute_commands;

const TARGET: &str = "monitor";

pub async fn monitor_task(
    experiment_run_path: PathBuf,
    router_name: String,
    console_host: String,
    console: u16,
    input_ready: String,
    mut monitor_command: Vec<OsCommand>,
    stop_monitoring: Arc<AtomicBool>
) -> anyhow::Result<()> {
    info!(target: TARGET, "Running monitoring: {}", &router_name);

    monitor_command.push(OsCommand::new_line(&input_ready));

    if let Err(err) = execute_commands(
        &experiment_run_path,
        &router_name,
        &console_host,
        console,
        monitor_command,
        Some(900),
        Some(stop_monitoring)
    ) {
        error!(target: TARGET, "{:?}", err);
    }
    
    info!(target: TARGET, "End monitoring: {}", &router_name);

    Ok(())
}