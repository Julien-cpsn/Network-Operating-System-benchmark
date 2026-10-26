use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use std::thread::sleep;
use tracing::{debug, info, warn};
use walkdir::WalkDir;
use crate::utils::qemu::network::delete_all_taps;

const TARGET: &str = "process";

pub const PID_FILE_NAME: &str = "qemu.pid";

/// Reads a QEMU pidfile
pub fn read_pid_file(pid_file: &Path) -> Option<u32> {
    fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}

/// True if `pid` is alive and is the QEMU process that wrote `pid_file` (guards against PID reuse)
pub fn is_qemu_alive(pid: u32, pid_file: &Path) -> bool {
    match fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => {
            let pid_file = pid_file.to_string_lossy();
            cmdline
                .split(|byte| *byte == 0)
                .any(|arg| String::from_utf8_lossy(arg) == pid_file)
        },
        Err(_) => false
    }
}

/// Waits for the process to exit, returns false on timeout
pub fn wait_for_exit(pid: u32, pid_file: &Path, max_wait: Duration) -> bool {
    let start = Instant::now();

    while start.elapsed() < max_wait {
        if !is_qemu_alive(pid, pid_file) {
            return true;
        }
        sleep(Duration::from_millis(100));
    }

    !is_qemu_alive(pid, pid_file)
}

fn send_signal(pid: u32, signal: &str) {
    let _ = Command::new("kill").args([format!("-{signal}"), pid.to_string()]).status();
}

/// SIGTERM (QEMU powers the VM off and exits), then SIGKILL if it did not exit in time
pub fn terminate_qemu(pid: u32, pid_file: &Path) {
    if !is_qemu_alive(pid, pid_file) {
        return;
    }

    send_signal(pid, "TERM");

    if !wait_for_exit(pid, pid_file, Duration::from_secs(10)) {
        warn!(target: TARGET, "QEMU process {pid} did not exit after SIGTERM, killing it");
        send_signal(pid, "KILL");
        wait_for_exit(pid, pid_file, Duration::from_secs(5));
    }
}

/// Stops every QEMU VM whose pidfile lies in `work_dir` (e.g. left running by `--no-stop`, a crash or Ctrl-C).
/// Blocking, so that it can also be called from the Ctrl-C handler.
pub fn stop_all_vms(work_dir: &Path) -> anyhow::Result<()> {
    if !work_dir.exists() {
        return Ok(());
    }

    let mut stopped_vm = false;

    for entry in WalkDir::new(work_dir).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() || entry.file_name() != PID_FILE_NAME {
            continue;
        }

        if let Some(pid) = read_pid_file(entry.path()) && is_qemu_alive(pid, entry.path()) {
            info!(target: TARGET, "Stopping VM (pid {}): {}", pid, entry.path().parent().unwrap().display());
            terminate_qemu(pid, entry.path());
            stopped_vm = true;
        }
    }

    if !stopped_vm {
        debug!(target: TARGET, "No running VM to stop");
    }

    Ok(())
}

/// Stops leftover VMs, wipes their disks and deletes their taps
pub fn stop_and_delete_all_vms(work_dir: &Path) -> anyhow::Result<()> {
    stop_all_vms(work_dir)?;
    delete_all_taps()?;

    if work_dir.exists() {
        debug!(target: TARGET, "Deleting old VMs in {}", work_dir.display());
        fs::remove_dir_all(work_dir)?;
    }

    fs::create_dir_all(work_dir)?;

    Ok(())
}
