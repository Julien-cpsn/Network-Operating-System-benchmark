//! Identity of this benchmark process, so that several of them can run at the same time, on one host
//! or on several hosts sharing the project directory (e.g. the NFS home of Grid'5000).
//!
//! What it scopes:
//! - host-wide resources (taps): named after the PID, so a process only deletes its own taps
//!   (and the ones left by dead processes of the same host, see `delete_orphan_taps`)
//! - files on the shared filesystem (VM disks and pidfiles, folder shared with the guests):
//!   in a sub-directory named after `INSTANCE_ID`
//! - results: one lock per experiment (`experiment_lock`), so that two processes never run the same
//!   experiment at the same time

use std::{env, fs, process};
use std::path::Path;
use once_cell::sync::Lazy;

/// Unique among the processes sharing the project directory: `NOSB_INSTANCE_ID` if set,
/// `<hostname>-<pid>` otherwise
pub static INSTANCE_ID: Lazy<String> = Lazy::new(|| {
    match env::var("NOSB_INSTANCE_ID") {
        Ok(id) if !id.trim().is_empty() => sanitize(id.trim()),
        _ => format!("{}-{}", hostname(), process::id()),
    }
});

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|hostname| sanitize(hostname.trim()))
        .ok()
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| String::from("localhost"))
}

/// Taps live in the host kernel, so the PID is enough to tell apart the processes that can see them.
/// 6 hex digits: Linux PIDs are at most 2^22.
pub fn tap_tag() -> String {
    format!("{:06x}", process::id() & 0xFF_FFFF)
}

pub fn is_process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn sanitize(value: &str) -> String {
    value.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect()
}
