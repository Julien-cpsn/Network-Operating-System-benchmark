use std::{env};
use std::path::Path;
use anyhow::Context;
use tracing::info;
use crate::args::g5k::HarvestG5kCommand;
use crate::commands::g5k::utils::{connect_to_ssh, download_dir, ensure_identifier, ensure_safe_relative_path, ssh_exec};

const TARGET: &str = "harvest_g5k";

pub async fn harvest_g5k(command: HarvestG5kCommand) -> anyhow::Result<()> {
    let user = &command.login.user;
    let site = &command.login.site;

    ensure_identifier("username", user, true)?;
    ensure_identifier("site", site, false)?;

    let local_root = env::current_dir()?.join("experimentation").join("results");

    info!(target: TARGET, "Connecting to Grid'5000 SSH");
    let session = connect_to_ssh(&command.login)?;


    let list_command = format!(r#"if [ -d "{site}/public/results" ]; then find "{site}/public/results" -mindepth 1 -maxdepth 1 -type d -print0; else exit 0; fi"#);
    let output = ssh_exec(&session, &list_command)?;

    let remote_files: Vec<&str> = output
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect();

    if remote_files.is_empty() {
        info!(target: TARGET, "No result files found on Grid'5000");
        session.disconnect(None, "done", None)?;
        return Ok(());
    }

    info!(target: TARGET, "Harvesting results from Grid'5000 site {site}");

    let remote_root = Path::new(site).join("public").join("results");

    let mut total_files = 0;
    let mut total_bytes = 0;

    for (index, remote_file) in remote_files.iter().enumerate() {
        let remote_path = Path::new(remote_file);

        // Refuse paths outside the expected results directory.
        let relative_path = remote_path.strip_prefix(&remote_root)?;
        ensure_safe_relative_path(relative_path)?;

        info!(target: TARGET, "Downloading {}/{}: {}", index, remote_files.len(), relative_path.display());

        let local_dir = local_root.join(relative_path);
        let (files, bytes) = download_dir(&session, remote_file, &remote_root, &local_root)
            .with_context(|| format!("Could not download \"{}\" into \"{}\"", relative_path.display(), local_dir.display()))?;

        total_files += files;
        total_bytes += bytes;
    }

    session.disconnect(None, "done", None)?;

    info!(target: TARGET, "Results harvested into {} ({total_files} file(s), {} MiB)", local_root.display(), total_bytes / (1024 * 1024));

    Ok(())
}