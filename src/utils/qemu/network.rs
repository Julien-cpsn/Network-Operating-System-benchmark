//! Link backends.
//!
//! - `Udp`: point-to-point UDP tunnel between two QEMU `socket` netdevs. Needs no privilege,
//!   every frame goes through QEMU's userspace.
//! - `Tap`: each link end is a persistent tap interface owned by the benchmark user, and the two taps
//!   of a link are wired back-to-back with `tc` (ingress + mirred redirect, no bridge so that no frame
//!   is filtered or learned). VirtIO NICs then use `vhost=on`: the datapath is handled by the
//!   in-kernel vhost-net instead of QEMU's userspace. Other NIC models use the same taps without vhost.
//!
//! In both cases the taps are opened with `vnet_hdr=off`, so QEMU does not expose checksum/TSO/GSO
//! offloads to the guests: frames on a link stay MTU-sized, like on a wire and like with `Udp`.

use std::fs::{self, OpenOptions};
use std::process::{exit, Command};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use anyhow::{anyhow, bail, Context};
use strum::Display;
use tracing::{debug, error, info, warn};
use crate::QEMU_ENABLE_KVM;

const TARGET: &str = "network";

/// Every tap created by the benchmark starts with this prefix, so that they can all be cleaned up
pub const TAP_PREFIX: &str = "nosb";
const PROBE_TAP: &str = "nosbprobe";

#[derive(Debug, Clone, Copy, PartialEq, Display)]
pub enum VhostMode {
    /// vhost when the context allows it, UDP tunnels otherwise
    #[strum(to_string = "auto")]
    Auto,
    /// vhost or fail
    #[strum(to_string = "on")]
    On,
    /// Always UDP tunnels
    #[strum(to_string = "off")]
    Off,
}

impl VhostMode {
    pub fn parse(value: &str) -> anyhow::Result<VhostMode> {
        match value.to_lowercase().as_str() {
            "auto" => Ok(VhostMode::Auto),
            "1" | "true" | "yes" | "on" => Ok(VhostMode::On),
            "0" | "false" | "no" | "off" => Ok(VhostMode::Off),
            _ => bail!("QEMU_VHOST must be auto, on or off, got \"{value}\""),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Display)]
pub enum LinkBackend {
    #[strum(to_string = "UDP tunnels between QEMU socket netdevs (no vhost)")]
    Udp,
    #[strum(to_string = "taps wired with tc, vhost-net for VirtIO NICs")]
    Tap,
}

static LINK_BACKEND: RwLock<LinkBackend> = RwLock::new(LinkBackend::Udp);
static TAP_VHOST: AtomicBool = AtomicBool::new(true);

pub fn link_backend() -> LinkBackend {
    *LINK_BACKEND.read().unwrap()
}

/// Whether VirtIO NICs plugged into taps use vhost-net (always, except in tests on hosts without vhost-net)
pub fn tap_vhost() -> bool {
    TAP_VHOST.load(Ordering::Relaxed)
}

#[cfg(test)]
pub fn set_link_backend(backend: LinkBackend, vhost: bool) {
    *LINK_BACKEND.write().unwrap() = backend;
    TAP_VHOST.store(vhost, Ordering::Relaxed);
}

/// Picks the link backend once, at startup
pub fn select_link_backend(mode: VhostMode) -> anyhow::Result<LinkBackend> {
    let backend = match mode {
        VhostMode::Off => LinkBackend::Udp,
        VhostMode::On => {
            probe_tap_backend().map_err(|reason| anyhow!("QEMU_VHOST is on but vhost cannot be used: {reason}"))?;
            LinkBackend::Tap
        },
        VhostMode::Auto => match probe_tap_backend() {
            Ok(()) => LinkBackend::Tap,
            Err(reason) => {
                warn!(target: TARGET, "vhost not available ({reason}). The experiment will be killed if a VirtIO NIC is used");
                LinkBackend::Udp
            }
        },
    };

    info!(target: TARGET, "Link backend: {backend}");

    *LINK_BACKEND.write().unwrap() = backend;

    Ok(backend)
}

/// Every requirement of the tap + vhost backend, the error explains the first one missing
fn probe_tap_backend() -> Result<(), String> {
    if !*QEMU_ENABLE_KVM.get().unwrap() {
        return Err(String::from("vhost-net requires KVM"));
    }

    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/vhost-net")
        .map_err(|err| format!("/dev/vhost-net is not usable: {err} (try \"sudo modprobe vhost_net\" and check its permissions)"))?;

    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .map_err(|err| format!("/dev/net/tun is not usable: {err}"))?;

    // Creating taps and tc filters needs root (directly or through passwordless sudo)
    let _ = delete_interface(PROBE_TAP);

    let probe = (|| -> anyhow::Result<()> {
        create_tap(PROBE_TAP)?;
        run_privileged("tc", &["qdisc", "add", "dev", PROBE_TAP, "ingress"])?;
        redirect(PROBE_TAP, PROBE_TAP)?;
        Ok(())
    })();

    let _ = delete_interface(PROBE_TAP);

    probe.map_err(|err| format!("cannot create taps and tc filters: {err}"))
}

fn is_root() -> bool {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .and_then(|line| line.split_whitespace().nth(2).map(|euid| euid == "0"))
        })
        .unwrap_or(false)
}

/// Runs a command as root: directly if we already are, through non-interactive sudo otherwise
fn run_privileged(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut command = if is_root() {
        Command::new(program)
    }
    else {
        let mut command = Command::new("sudo");
        command.arg("-n").arg(program);
        command
    };

    let output = command.args(args).output().with_context(|| format!("Could not run {program}"))?;

    if !output.status.success() {
        bail!("{} {} failed: {}", program, args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }

    Ok(())
}

fn current_user() -> anyhow::Result<String> {
    let output = Command::new("id").arg("-un").output().context("Could not run id")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Persistent tap owned by the current user (so that an unprivileged QEMU can open it), up, without IPv6
/// (otherwise the host would send router solicitations and MLD reports into the experiment)
fn create_tap(name: &str) -> anyhow::Result<()> {
    let user = current_user()?;

    run_privileged("ip", &["tuntap", "add", "dev", name, "mode", "tap", "user", &user])?;

    if let Err(err) = run_privileged("sysctl", &["-q", "-w", &format!("net.ipv6.conf.{name}.disable_ipv6=1")]) {
        debug!(target: TARGET, "Could not disable IPv6 on {name} (IPv6 may be disabled on the host): {err}");
    }

    run_privileged("ip", &["link", "set", "dev", name, "up"])?;

    Ok(())
}

/// Every frame sent by the VM on `from` is delivered as is to the VM on `to`
fn redirect(from: &str, to: &str) -> anyhow::Result<()> {
    run_privileged("tc", &[
        "filter", "add", "dev", from, "parent", "ffff:", "protocol", "all",
        "u32", "match", "u32", "0", "0",
        "action", "mirred", "egress", "redirect", "dev", to
    ])
}

fn delete_interface(name: &str) -> anyhow::Result<()> {
    run_privileged("ip", &["link", "delete", "dev", name])
}

/// The two taps of physical link number `link_index`, wired to each other
pub fn create_tap_pair(link_index: usize) -> anyhow::Result<(String, String)> {
    let tap_a = format!("{TAP_PREFIX}{link_index:04x}a");
    let tap_b = format!("{TAP_PREFIX}{link_index:04x}b");

    for tap in [&tap_a, &tap_b] {
        create_tap(tap)?;
        run_privileged("tc", &["qdisc", "add", "dev", tap, "ingress"])?;
    }

    redirect(&tap_a, &tap_b)?;
    redirect(&tap_b, &tap_a)?;

    debug!(target: TARGET, "Created taps {tap_a} <-> {tap_b}");

    Ok((tap_a, tap_b))
}

/// Deletes every tap created by the benchmark. Their VMs must have been stopped before.
pub fn delete_all_taps() -> anyhow::Result<()> {
    let Ok(interfaces) = fs::read_dir("/sys/class/net") else {
        return Ok(());
    };

    for interface in interfaces.filter_map(Result::ok) {
        let name = interface.file_name().to_string_lossy().to_string();

        if name.starts_with(TAP_PREFIX) {
            debug!(target: TARGET, "Deleting tap {name}");
            delete_interface(&name)?;
        }
    }

    Ok(())
}
