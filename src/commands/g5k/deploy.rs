use std::{env, fs};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use clap::{Args, Command, FromArgMatches};
use indexmap::IndexMap;
use serde_json::{json, Value};
use tokio::time::sleep;
use tracing::{info, warn};
use crate::args::g5k::DeployG5kCommand;
use crate::args::run::RunCommand;
use crate::commands::g5k::utils::{connect_to_ssh, ensure_identifier, scp_upload, ssh_exec, G5K_API_URL, G5K_ACCESS_HOST};
use crate::models::operating_system::OperatingSystem;
use crate::utils::files::network_stacks::parse_network_stack_list_file;
use crate::utils::files::oses::parse_os_list_file;
use crate::utils::files::routing_stacks::parse_routing_stack_list_file;

const TARGET: &str = "deploy_g5k";

const JOB_POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 3;

pub async fn deploy_g5k(command: DeployG5kCommand) -> anyhow::Result<()> {
    // Everything below ends up in a remote shell command or in a path, so only accept safe identifiers
    ensure_identifier("username", &command.login.user, true)?;
    ensure_identifier("site", &command.login.site, false)?;
    ensure_identifier("cluster", &command.cluster, false)?;
    ensure_identifier("queue", &command.queue, false)?;
    ensure_identifier("experiment name", &command.experiment_name, true)?;

    if let Some(walltime) = &command.walltime {
        if walltime.is_empty() || !walltime.chars().all(|c| c.is_ascii_digit() || c == ':') {
            bail!("Invalid walltime \"{walltime}\", expected something like \"3\" or \"3:30\"");
        }
    }

    let network_stack_list = parse_network_stack_list_file()?;
    let routing_stack_list = parse_routing_stack_list_file()?;
    let os_list = parse_os_list_file(network_stack_list.keys().collect(), routing_stack_list.keys().collect())?;

    let mut operating_systems_to_use = IndexMap::new();

    let experiment_commands = fs::read_to_string(&command.experiment_commands)?;
    for (index, command_line) in experiment_commands.lines().enumerate() {
        if command_line.starts_with('#') || command_line.is_empty() {
            continue;
        }

        if !command_line.starts_with("cargo run -- run ") {
            bail!("Commands at line {index} must start with \"cargo run -- \"");
        }

        let arguments = shlex::split(&command_line[17..]).context("invalid quoting")?;
        let cmd = RunCommand::augment_args(Command::new("inline").no_binary_name(true));
        let matches = cmd.try_get_matches_from(arguments)?;
        let args = RunCommand::from_arg_matches(&matches)?;

        if let Some(os_name) = args.experiment_selection.os {
            let os = os_list.get(&os_name).ok_or_else(|| anyhow!("No operating system {}", os_name))?;
            operating_systems_to_use.insert(os_name, os.clone());
        }
        else {
            // Use every OS
            operating_systems_to_use = os_list;
            break;
        }
    }

    // Find the images to upload

    for (os_name, os) in &operating_systems_to_use {
        if os.images_path.is_empty() {
            bail!("Operating system \"{}\" has no image to upload", os_name);
        }

        for image_path in &os.images_path {
            if !image_path.is_file() {
                bail!("Image \"{}\" does not exist or is not a file", image_path.display());
            }
        }
    }

    let password = rpassword::prompt_password("Grid5000 password: ")?;
    println!();

    prepare_site_machine(&command.login.user, &command.login.site, &command.experiment_name, &command.experiment_commands, &operating_systems_to_use)?;
    let job_id = submit_job(&command, &password).await?;

    info!(target: TARGET, "Results will be available at: https://api.grid5000.fr/stable/sites/{}/public/{}/", &command.login.site,  &command.login.user);

    if command.no_wait {
        info!(target: TARGET, "Job {job_id} submitted, not waiting for it (--no-wait)");
        return Ok(());
    }

    wait_for_job(&command, &password, job_id).await?;

    info!(target: TARGET, "Job {job_id} terminated. Its output is in \"~/{}/stdout-benchmark-nos-{}.txt\" on Grid'5000", command.login.site, command.experiment_name);

    Ok(())
}

// ---------------------------------------------------------------- SSH

fn prepare_site_machine(user: &str, site: &str, experiment_name: &String, experiment_commands: &PathBuf, operating_systems_to_use: &IndexMap<String, OperatingSystem>) -> anyhow::Result<()> {
    info!(target: TARGET, "Connecting to Grid'5000 SSH ({})", G5K_ACCESS_HOST);
    let session = connect_to_ssh(user)?;
    info!(target: TARGET, "SSH connected successfully");

    for (os_name, os) in operating_systems_to_use {
        let remote_os_dir = format!("{}/survey/vm/{}", site, os_name);

        let command = format!("mkdir -p -- ~/{remote_os_dir}");
        info!(target: TARGET, "> {command}");
        ssh_exec(&session, &command)?;

        for image_path in &os.images_path {
            let file_name = image_path.file_name().ok_or_else(|| anyhow!("Invalid image path \"{}\"", image_path.display()))?;
            // Relative to the remote home directory, which is where SCP starts
            let remote_path = format!("{remote_os_dir}/{}", file_name.to_string_lossy());

            info!(target: TARGET, "Uploading \"{}\" to \"~/{remote_path}\"", image_path.display());
            scp_upload(&session, &image_path, &remote_path, 0o644)?;
        }
    }

    info!(target: TARGET, "All images uploaded");

    info!(target: TARGET, "Uploading the commands to Grid'5000");
    scp_upload(&session, &experiment_commands, &format!("{site}/survey/Network-Operating-System-benchmark/run_experiment.sh"), 0o774)?;
    info!(target: TARGET, "Commands uploaded as \"run_experiment.sh\"");

    info!(target: TARGET, "Uploading run_benchmark_g5k.sh to Grid'5000");
    scp_upload(&session, &env::current_dir()?.as_path().join("run_benchmark_g5k.sh"), &format!("{site}/run_benchmark_g5k.sh"), 0o774)?;
    info!(target: TARGET, "run_benchmark_g5k.sh uploaded");

    let command = format!("rm ~/stdout-benchmark-nos-{experiment_name}.txt");
    info!(target: TARGET, "> {command}");
    let _ = ssh_exec(&session, &command);

    session.disconnect(None, "done", None)?;

    Ok(())
}

// ---------------------------------------------------------------- Grid'5000 API

async fn submit_job(command: &DeployG5kCommand, password: &str) -> anyhow::Result<u64> {
    let client = reqwest::Client::new();
    let jobs_url = format!("{}/sites/{}/jobs", G5K_API_URL, command.login.site);

    let resources = match &command.walltime {
        Some(walltime) => format!("nodes=1,walltime={walltime}"),
        None => "nodes=1".to_string(),
    };

    let queue = command.effective_queue();

    let payload = json!({
        "resources": resources,
        "command": format!("/home/{}/run_benchmark_g5k.sh \"{}\"", command.login.user, command.login.user),
        "stdout": format!("stdout-benchmark-nos-{}.txt", command.experiment_name),
        "queue": queue,
        "properties": format!("cluster='{}'", command.cluster),
        "name": format!("benchmark-nos-{}", command.experiment_name),
    });

    info!(target: TARGET, "Reserving a node on cluster \"{}\" (queue \"{}\") at {}", command.cluster, queue, command.login.site);

    let response = client
        .post(&jobs_url)
        .basic_auth(&command.login.user, Some(password))
        .json(&payload)
        .send()
        .await
        .context("Could not reach the Grid'5000 API")?;

    let status = response.status();

    if status.as_u16() != 201 {
        let body = response.text().await.unwrap_or_default();
        if status.as_u16() == 400 && body.contains("not enough resources") {
            bail!(
                "No resource matches cluster '{}' in queue '{}'. You probably don't have this priority level \
                 on this cluster (check https://api.grid5000.fr/explorer). Raw error: {body}",
                command.cluster,
                command.effective_queue(),
            );
        }
        bail!("Job submission failed ({status}): {body}");
    }

    let job: Value = response.json().await?;
    let job_id = job["uid"].as_u64().ok_or_else(|| anyhow!("The Grid'5000 API response has no job uid: {job}"))?;

    info!(target: TARGET, "Job submitted with ID: {job_id}");

    Ok(job_id)
}

async fn wait_for_job(command: &DeployG5kCommand, password: &str, job_id: u64) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let job_url = format!("{G5K_API_URL}/sites/{}/jobs/{job_id}", command.login.site);

    let mut last_state = String::new();
    let mut failures = 0;

    loop {
        sleep(JOB_POLL_INTERVAL).await;

        let state = match fetch_job_state(&client, &job_url, &command.login.user, password).await {
            Ok(state) => {
                failures = 0;
                state
            },
            Err(e) => {
                failures += 1;
                warn!(target: TARGET, "Could not fetch the job state ({failures}/{MAX_CONSECUTIVE_POLL_FAILURES}): {e}");

                if failures >= MAX_CONSECUTIVE_POLL_FAILURES {
                    bail!("Giving up on following job {job_id}, it may still be running: {e}");
                }

                continue;
            }
        };

        if state != last_state {
            info!(target: TARGET, "Job {job_id} state: {state}");
            last_state = state.clone();
        }

        match state.as_str() {
            "terminated" => return Ok(()),
            "error" => bail!("Job {job_id} ended in error"),
            _ => {}
        }
    }
}

async fn fetch_job_state(client: &reqwest::Client, job_url: &str, user: &str, password: &str) -> anyhow::Result<String> {
    let response = client.get(job_url).basic_auth(user, Some(password)).send().await?;

    if !response.status().is_success() {
        bail!("HTTP {}", response.status());
    }

    let job: Value = response.json().await?;

    job["state"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no \"state\" field in {job}"))
}
