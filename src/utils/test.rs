use crate::models::test::Test;
use crate::utils::files::shared_dir::SHARED_DIR_PATH;
use crate::utils::os_commands::execute::execute_commands;
use std::fs;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use tracing::{error, info};
use walkdir::{DirEntry, WalkDir};
use crate::models::os_command::OsCommand;
use crate::utils::os_commands::guest::GUEST_INPUT_READY;

const TARGET: &str = "test";


pub fn network_test_task(
    experiment_run_path: &PathBuf,
    from_guest_name: &str,
    to_guest_name: &str,
    console_host: &str,
    console: u16,
    distant_ip: Ipv4Addr
) {
    info!(target: TARGET, "Running network test from {} to {}", &from_guest_name, &to_guest_name);

    let mut network_test_commands = vec![];

    network_test_commands.push(OsCommand::new_text(GUEST_INPUT_READY, format!("printf '%s\n' 'waiting for {distant_ip}'"), true, false));
    network_test_commands.push(OsCommand::new_text(GUEST_INPUT_READY, format!(r#"while ! timeout 1.0 ping -c 1 -n {distant_ip} &> /dev/null
do
    printf '%c' '.'
done
printf '\n%s\n'  'Server is online'"#), true, false));
    network_test_commands.push(OsCommand::new_text(GUEST_INPUT_READY, "printf '\n%s\n'  'Server is back online'", true, false));
    network_test_commands.push(OsCommand::new_line(GUEST_INPUT_READY));
    network_test_commands.push(OsCommand::new_line(GUEST_INPUT_READY));

    if let Err(err) = execute_commands(
        &experiment_run_path,
        &from_guest_name,
        &console_host,
        console,
        network_test_commands,
        Some(u64::MAX),
        None
    ) {
        error!(target: TARGET, "{:?}", err);
    }

    info!(target: TARGET, "Connection established between {} and {}", &from_guest_name, &to_guest_name);
}

pub async fn test_task(
    experiment_run_path: PathBuf,
    from_node_name: String,
    console_host: String,
    console: u16,
    test_commands: Vec<OsCommand>,
) -> anyhow::Result<()> {
    execute_commands(
        &experiment_run_path,
        &from_node_name,
        &console_host,
        console,
        test_commands,
        Some(180_000),
        None
    )?;

    Ok(())
}

pub fn harvest_results(experiment_run_path: &PathBuf, test: &Test) -> anyhow::Result<()> {
    info!(target: TARGET, "Harvesting results for test {}...", &test.name);

    let result_path = experiment_run_path.join(&test.name);
    
    fs::create_dir_all(&result_path)?;

    let walk_result_files = WalkDir::new(SHARED_DIR_PATH.as_path().join(&test.name));
    let result_files: Vec<DirEntry> = walk_result_files
        .into_iter()
        .filter_map(|f| f.ok())
        .filter(|f| f.file_type().is_file() && !f.path().iter().any(|p| p.eq(".virtfs_metadata")))
        .collect();

    if result_files.is_empty() {
        error!(target: TARGET, "No test result files found");
    }
    else {
        for file in result_files {
            let output_path = result_path.join(file.file_name());

            info!(target: TARGET, "Retrieved experiment result file: {}", file.file_name().display());
            info!(target: TARGET, "Moved to: {}", output_path.display());

            fs::rename(file.path(), output_path)?;
        }
    }

    Ok(())
}

pub fn clear_shared_dir() -> anyhow::Result<()> {
    info!(target: TARGET, "Clearing shared directory");

    let shared_dir = SHARED_DIR_PATH.as_path();
    for entry in fs::read_dir(shared_dir)?.filter_map(Result::ok) {
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        }
        else {
            fs::remove_file(entry.path())?;
        }
    }

    Ok(())
}