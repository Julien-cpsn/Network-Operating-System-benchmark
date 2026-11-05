use std::{env, fs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

use anyhow::{anyhow, bail, Context};
use ssh2::Session;
use tracing::{info, warn};

use crate::args::g5k::HarvestG5kCommand;
use crate::commands::g5k::utils::{connect_to_ssh, ensure_identifier, ensure_safe_relative_path, scp_download, ssh_exec};

const TARGET: &str = "harvest_g5k";

/// Results directory on the site frontend, relative to the home directory
const REMOTE_ROOT: &str = "public/results";

/// A failed download is tried again once before giving up (the file is written atomically, so this is safe)
const DOWNLOAD_ATTEMPTS: u32 = 2;

struct RemoteFile {
    /// Path relative to the results directory
    relative_path: String,
    size: u64,
}

pub async fn harvest_g5k(command: HarvestG5kCommand) -> anyhow::Result<()> {
    // Everything below is blocking (libssh2), keep it off the async runtime threads
    tokio::task::spawn_blocking(move || harvest(command)).await?
}

fn harvest(command: HarvestG5kCommand) -> anyhow::Result<()> {
    let user = &command.login.user;
    let site = &command.login.site;

    ensure_identifier("username", user, true)?;
    ensure_identifier("site", site, false)?;

    let local_root = env::current_dir()?.join("experimentation").join("results");

    info!(target: TARGET, "Listing the results on Grid'5000 site {site}");
    let (mut files, empty_dirs) = list_remote_results(&command)?;

    if files.is_empty() && empty_dirs.is_empty() {
        info!(target: TARGET, "No result files found on Grid'5000");
        return Ok(());
    }

    // Biggest files first, so the slowest transfers do not end up alone at the end
    files.sort_by(|a, b| b.size.cmp(&a.size));

    let total_files = files.len();
    let total_size: u64 = files.iter().map(|file| file.size).sum();
    let workers = 8;

    info!(target: TARGET, "Harvesting {total_files} file(s) ({} MiB) with {workers} connection(s)", total_size / (1024 * 1024));

    let start = Instant::now();
    let downloaded = download_files(&command, &files, &local_root, workers)?;

    // `find -type f` does not list empty directories, keep them
    for relative_path in &empty_dirs {
        fs::create_dir_all(local_root.join(relative_path))?;
    }

    let seconds = start.elapsed().as_secs_f64().max(0.001);
    info!(
        target: TARGET,
        "Results harvested into {} ({downloaded} file(s), {} MiB in {seconds:.1}s, {:.1} MiB/s)",
        local_root.display(),
        total_size / (1024 * 1024),
        total_size as f64 / (1024.0 * 1024.0) / seconds
    );

    Ok(())
}

/// Lists every file and empty directory of the results directory, with a single connection and two commands
/// (instead of one listing per experiment). Files directly in the results directory are ignored.
fn list_remote_results(command: &HarvestG5kCommand) -> anyhow::Result<(Vec<RemoteFile>, Vec<String>)> {
    let session = connect_to_ssh(&command.login)?;

    let files_output = ssh_exec(&session, &format!(r#"if [ -d "{REMOTE_ROOT}" ]; then find "{REMOTE_ROOT}" -mindepth 2 -type f -printf '%s %P\0'; fi"#))?;
    let dirs_output = ssh_exec(&session, &format!(r#"if [ -d "{REMOTE_ROOT}" ]; then find "{REMOTE_ROOT}" -mindepth 1 -type d -empty -printf '%P\0'; fi"#))?;

    session.disconnect(None, "done", None)?;

    let mut files = Vec::new();

    for entry in files_output.split('\0').filter(|entry| !entry.is_empty()) {
        let (size, relative_path) = entry.split_once(' ').ok_or_else(|| anyhow!("Unexpected listing entry \"{entry}\""))?;
        let size = size.parse().with_context(|| format!("Unexpected file size in listing entry \"{entry}\""))?;

        // Refuse paths outside the expected results directory
        ensure_safe_relative_path(Path::new(relative_path))?;

        files.push(RemoteFile { relative_path: relative_path.to_string(), size });
    }

    let mut empty_dirs = Vec::new();

    for relative_path in dirs_output.split('\0').filter(|path| !path.is_empty()) {
        ensure_safe_relative_path(Path::new(relative_path))?;
        empty_dirs.push(relative_path.to_string());
    }

    Ok((files, empty_dirs))
}

/// Downloads the files with `workers` threads, each one with its own SSH connection and taking the next file
/// of the list when it is done. Stops at the first failure. Returns the number of downloaded files.
fn download_files(command: &HarvestG5kCommand, files: &[RemoteFile], local_root: &Path, workers: usize) -> anyhow::Result<usize> {
    let total_files = files.len();
    let progress_step = (total_files / 20).max(1);

    let next_file = AtomicUsize::new(0);
    let downloaded_files = AtomicUsize::new(0);
    let downloaded_bytes = AtomicU64::new(0);
    let abort = AtomicBool::new(false);
    let download_error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let connect_error: Mutex<Option<anyhow::Error>> = Mutex::new(None);

    thread::scope(|scope| {
        for worker in 0..workers {
            let (next_file, downloaded_files, downloaded_bytes) = (&next_file, &downloaded_files, &downloaded_bytes);
            let (abort, download_error, connect_error) = (&abort, &download_error, &connect_error);

            scope.spawn(move || {
                let session = match connect_to_ssh(&command.login) {
                    Ok(session) => session,
                    Err(error) => {
                        warn!(target: TARGET, "Connection {} failed, continuing without it: {error:#}", worker + 1);
                        connect_error.lock().unwrap().get_or_insert(error);
                        return;
                    }
                };

                while !abort.load(Ordering::Relaxed) {
                    let Some(file) = files.get(next_file.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };

                    match download_file(&session, file, local_root) {
                        Ok(bytes) => {
                            downloaded_bytes.fetch_add(bytes, Ordering::Relaxed);
                            let done = downloaded_files.fetch_add(1, Ordering::Relaxed) + 1;

                            if done % progress_step == 0 || done == total_files {
                                info!(
                                    target: TARGET,
                                    "{done}/{total_files} file(s), {} MiB",
                                    downloaded_bytes.load(Ordering::Relaxed) / (1024 * 1024)
                                );
                            }
                        },
                        Err(error) => {
                            abort.store(true, Ordering::Relaxed);
                            download_error.lock().unwrap().get_or_insert(error);
                            break;
                        }
                    }
                }

                let _ = session.disconnect(None, "done", None);
            });
        }
    });

    if let Some(error) = download_error.into_inner()? {
        return Err(error);
    }

    let downloaded = downloaded_files.load(Ordering::Relaxed);

    if downloaded < total_files {
        // Every connection failed (a partial failure is absorbed by the other workers)
        return Err(connect_error.into_inner()?.unwrap_or_else(|| anyhow!("Only {downloaded}/{total_files} files were downloaded")))
            .context("Could not connect to Grid'5000");
    }

    Ok(downloaded)
}

fn download_file(session: &Session, file: &RemoteFile, local_root: &Path) -> anyhow::Result<u64> {
    let remote_path = format!("{REMOTE_ROOT}/{}", file.relative_path);
    let local_path = local_root.join(&file.relative_path);

    let mut attempt = 1;

    loop {
        match scp_download(session, &remote_path, &local_path) {
            Ok(bytes) => return Ok(bytes),
            Err(error) if attempt < DOWNLOAD_ATTEMPTS => {
                warn!(target: TARGET, "Could not download \"{}\" (attempt {attempt}/{DOWNLOAD_ATTEMPTS}): {error:#}", file.relative_path);
                attempt += 1;
            },
            Err(error) => {
                bail!("Could not download \"{}\" into \"{}\": {error:#}", file.relative_path, local_path.display());
            }
        }
    }
}