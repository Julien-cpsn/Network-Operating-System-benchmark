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
//!
//! VirtIO NICs are multiqueue (`QEMU_VIRTIO_QUEUES`, one queue pair per vCPU by default): each queue
//! pair has its own vhost-net worker on the host and its own interrupt in the guest, so that a
//! multi-vCPU router can spread its packets over its vCPUs instead of going through one vhost thread.
//! The kernel refuses to open a tap whose multiqueue flag differs from the one it was created with,
//! so each tap is created with `multi_queue` exactly when its VM opens it with more than one queue.

use std::fs::{self, OpenOptions};
use std::process::{Command};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use anyhow::{anyhow, bail, Context};
use strum::Display;
use tracing::{debug, info, warn};
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

/// Number of queue pairs of the VirtIO NICs
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VirtioQueues {
    /// One queue pair per vCPU of the VM
    PerVcpu,
    /// The same number for every VM (1 disables multiqueue)
    Fixed(u32),
}

impl VirtioQueues {
    /// Maximum supported by QEMU's virtio-net
    const MAX: u32 = 1024;

    pub fn parse(value: &str) -> anyhow::Result<VirtioQueues> {
        match value.to_lowercase().as_str() {
            "auto" | "vcpu" => Ok(VirtioQueues::PerVcpu),
            "off" | "no" | "false" => Ok(VirtioQueues::Fixed(1)),
            number => match number.parse::<u32>() {
                Ok(queues) if (1..=Self::MAX).contains(&queues) => Ok(VirtioQueues::Fixed(queues)),
                _ => bail!("QEMU_VIRTIO_QUEUES must be auto, off or a number between 1 and {}, got \"{value}\"", Self::MAX),
            },
        }
    }

    /// Queue pairs of a VirtIO NIC of a VM with `vcpu` vCPUs
    pub fn for_vcpu(&self, vcpu: u32) -> u32 {
        match self {
            VirtioQueues::PerVcpu => vcpu.clamp(1, Self::MAX),
            VirtioQueues::Fixed(queues) => *queues,
        }
    }
}

impl std::fmt::Display for VirtioQueues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VirtioQueues::PerVcpu => write!(f, "one queue pair per vCPU"),
            VirtioQueues::Fixed(1) => write!(f, "1 queue pair (multiqueue off)"),
            VirtioQueues::Fixed(queues) => write!(f, "{queues} queue pairs"),
        }
    }
}

static VIRTIO_QUEUES: RwLock<VirtioQueues> = RwLock::new(VirtioQueues::PerVcpu);

pub fn virtio_queues() -> VirtioQueues {
    *VIRTIO_QUEUES.read().unwrap()
}

pub fn set_virtio_queues(queues: VirtioQueues) {
    info!(target: TARGET, "VirtIO NICs: {queues}");
    *VIRTIO_QUEUES.write().unwrap() = queues;
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
        create_tap(PROBE_TAP, true)?;
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
/// (otherwise the host would send router solicitations and MLD reports into the experiment).
/// `multi_queue` must match how QEMU opens it: with `queues` > 1, and only then.
fn create_tap(name: &str, multi_queue: bool) -> anyhow::Result<()> {
    let user = current_user()?;

    let mut args = vec!["tuntap", "add", "dev", name, "mode", "tap", "user", &user];
    if multi_queue {
        args.push("multi_queue");
    }

    run_privileged("ip", &args)?;

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

/// The two taps of physical link number `link_index`, wired to each other. `queues_a` and `queues_b`
/// are the number of queues each VM opens its tap with.
pub fn create_tap_pair(link_index: usize, queues_a: u32, queues_b: u32) -> anyhow::Result<(String, String)> {
    let tap_a = format!("{TAP_PREFIX}{link_index:04x}a");
    let tap_b = format!("{TAP_PREFIX}{link_index:04x}b");

    for (tap, queues) in [(&tap_a, queues_a), (&tap_b, queues_b)] {
        create_tap(tap, queues > 1)?;
        run_privileged("tc", &["qdisc", "add", "dev", tap, "ingress"])?;
    }

    redirect(&tap_a, &tap_b)?;
    redirect(&tap_b, &tap_a)?;

    debug!(target: TARGET, "Created taps {tap_a} ({queues_a} queues) <-> {tap_b} ({queues_b} queues)");

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
