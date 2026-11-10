//! One lock file per experiment in `experimentation/locks/`, held while an instance runs it: results of an
//! experiment are wiped when it starts, so two instances running it at the same time would destroy each
//! other's results. An instance that finds an experiment locked skips it.
//!
//! The lock is created with O_EXCL, which is atomic on local filesystems and on NFS (v3 and later).
//! A lock left by a process that died on this host is taken over; one left by another host must be
//! deleted by hand (its content says which host and PID created it).

use std::{env, fs, process};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use once_cell::sync::Lazy;
use tracing::{debug, warn};
use crate::utils::instance::{hostname, is_process_alive, INSTANCE_ID};

const TARGET: &str = "lock";

pub static LOCK_DIR_PATH: Lazy<PathBuf> = Lazy::new(|| env::current_dir().unwrap().join("experimentation").join("locks"));

/// The lock currently held (one experiment at a time), released on exit
static HELD_LOCK: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn lock_path(experiment_name: &str) -> PathBuf {
    LOCK_DIR_PATH.join(format!("{experiment_name}.lock"))
}

/// Ok(true) if this instance now holds the lock of the experiment, Ok(false) if another instance does
pub fn acquire_experiment_lock(experiment_name: &str) -> anyhow::Result<bool> {
    fs::create_dir_all(&*LOCK_DIR_PATH)?;

    let path = lock_path(experiment_name);

    // Second attempt only after removing a stale lock
    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                writeln!(file, "{}\n{}\n{}", hostname(), process::id(), *INSTANCE_ID)?;
                file.sync_all()?;
                debug!(target: TARGET, "Locked {}", path.display());
                *HELD_LOCK.lock().unwrap() = Some(path);
                return Ok(true);
            },
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if !is_stale(&path) {
                    return Ok(false);
                }

                warn!(target: TARGET, "Removing stale lock {} (its process is dead)", path.display());
                let _ = fs::remove_file(&path);
            },
            Err(error) => return Err(error.into()),
        }
    }

    Ok(false)
}

/// Who holds the lock of an experiment, for messages
pub fn lock_holder(experiment_name: &str) -> String {
    fs::read_to_string(lock_path(experiment_name))
        .ok()
        .and_then(|content| content.lines().nth(2).map(str::to_string))
        .unwrap_or_else(|| String::from("another instance"))
}

/// Releases the lock held by this instance, if any. Safe to call from the Ctrl-C handler and before `exit`.
pub fn release_experiment_lock() {
    let Ok(mut held) = HELD_LOCK.lock() else {
        return;
    };

    if let Some(path) = held.take() {
        debug!(target: TARGET, "Unlocked {}", path.display());
        let _ = fs::remove_file(path);
    }
}

/// Created on this host by a process that no longer exists
fn is_stale(path: &PathBuf) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };

    let mut lines = content.lines();

    match (lines.next(), lines.next().and_then(|pid| pid.trim().parse::<u32>().ok())) {
        (Some(host), Some(pid)) => host == hostname() && !is_process_alive(pid),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_exclusive_and_stale_locks_are_taken_over() {
        let name = format!("lock_test_{}", process::id());

        assert!(acquire_experiment_lock(&name).unwrap());
        // Held by a live process (this one): another attempt fails
        assert!(!acquire_experiment_lock(&name).unwrap());
        release_experiment_lock();
        assert!(!lock_path(&name).exists());

        // Left by a dead process of this host: taken over
        fs::write(lock_path(&name), format!("{}\n{}\nghost\n", hostname(), u32::MAX)).unwrap();
        assert!(acquire_experiment_lock(&name).unwrap());
        release_experiment_lock();

        // Left by another host: kept
        fs::write(lock_path(&name), "another-host\n1\nother\n").unwrap();
        assert!(!acquire_experiment_lock(&name).unwrap());
        assert_eq!(lock_holder(&name), "other");
        fs::remove_file(lock_path(&name)).unwrap();
    }
}
