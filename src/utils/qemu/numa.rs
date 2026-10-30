//! NUMA placement of the VMs (`QEMU_NUMA`).
//!
//! A VM whose vCPUs, vhost workers and memory are spread over several sockets pays for every packet
//! crossing the inter-socket link (measured on a 2-socket EPYC 9754: ~10-20 % of the forwarding rate).
//! Each VM is therefore started under `numactl --cpunodebind=N --membind=N`, so that all its threads
//! (vCPUs, vhost workers, QEMU main loop) and its RAM stay on one node.
//!
//! In `auto` mode, a topology that fits on one node is placed entirely on the first node: links between
//! VMs then never cross sockets either. A larger topology is split over the nodes in proportion to their
//! CPUs, following the order of the nodes in the topology so that neighbours tend to share a node.
//! The weight of a VM is its number of vCPUs plus its number of vhost workers (one per queue pair
//! of each VirtIO NIC): an upper bound of the threads it can keep busy.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::RwLock;
use anyhow::{bail, Context};
use tracing::{info, warn};
use crate::utils::qemu::host_monitor::HOST_RESOURCES_DIR_NAME;
use crate::utils::qemu::vm::{Connection, QemuVm};

const TARGET: &str = "numa";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumaMode {
    /// Place the VMs automatically when the host has several NUMA nodes and numactl is installed
    Auto,
    /// No placement: the host scheduler decides
    Off,
    /// Every VM on this node
    Node(u32),
}

impl NumaMode {
    pub fn parse(value: &str) -> anyhow::Result<NumaMode> {
        match value.to_lowercase().as_str() {
            "auto" => Ok(NumaMode::Auto),
            "off" | "no" | "false" => Ok(NumaMode::Off),
            number => match number.parse::<u32>() {
                Ok(node) => Ok(NumaMode::Node(node)),
                Err(_) => bail!("QEMU_NUMA must be auto, off or a NUMA node number, got \"{value}\""),
            },
        }
    }
}

static NUMA_MODE: RwLock<NumaMode> = RwLock::new(NumaMode::Auto);

pub fn set_numa_mode(mode: NumaMode) {
    *NUMA_MODE.write().unwrap() = mode;
}

/// A NUMA node and its online CPUs
#[derive(Debug, Clone, PartialEq)]
pub struct HostNode {
    pub id: u32,
    pub cpus: u32,
}

/// Parses a kernel CPU list such as "0-7,256-263"
fn parse_cpu_list(list: &str) -> Vec<u32> {
    list.trim()
        .split(',')
        .filter(|part| !part.is_empty())
        .flat_map(|part| match part.split_once('-') {
            Some((start, end)) => match (start.parse::<u32>(), end.parse::<u32>()) {
                (Ok(start), Ok(end)) => (start..=end).collect(),
                _ => vec![],
            },
            None => part.parse::<u32>().into_iter().collect(),
        })
        .collect()
}

/// The NUMA nodes that have online CPUs (CPUs disabled with SMT off are not counted)
pub fn host_nodes() -> Vec<HostNode> {
    let online = fs::read_to_string("/sys/devices/system/cpu/online").map(|list| parse_cpu_list(&list)).unwrap_or_default();

    let Ok(entries) = fs::read_dir("/sys/devices/system/node") else {
        return vec![];
    };

    let mut nodes: Vec<HostNode> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let id = entry.file_name().to_str()?.strip_prefix("node")?.parse::<u32>().ok()?;
            let cpus = parse_cpu_list(&fs::read_to_string(entry.path().join("cpulist")).ok()?);
            let cpus = cpus.iter().filter(|cpu| online.is_empty() || online.contains(cpu)).count() as u32;
            (cpus > 0).then_some(HostNode { id, cpus })
        })
        .collect();

    nodes.sort_by_key(|node| node.id);
    nodes
}

fn numactl_available() -> bool {
    Command::new("numactl").arg("--show").output().is_ok_and(|output| output.status.success())
}

/// Upper bound of the host threads a VM can keep busy: its vCPUs and its vhost workers
pub fn vm_weight(vm: &QemuVm) -> u32 {
    let vhost_workers: u32 = vm.adapters
        .iter()
        .filter_map(|adapter| match &adapter.connection {
            Some(Connection::Tap { vhost: true, queues, .. }) => Some(*queues),
            _ => None,
        })
        .sum();

    vm.vcpu + vhost_workers
}

/// Node of each VM (same order as `weights`). Everything on the first node when it fits there,
/// otherwise the VMs are dealt out in order, each node taking a share proportional to its CPUs.
pub fn plan(weights: &[u32], nodes: &[HostNode]) -> Vec<u32> {
    let Some(first) = nodes.first() else {
        return vec![];
    };

    let total: u32 = weights.iter().sum();

    if nodes.len() == 1 || total <= first.cpus {
        return vec![first.id; weights.len()];
    }

    let all_cpus: u32 = nodes.iter().map(|node| node.cpus).sum();
    let targets: Vec<f64> = nodes.iter().map(|node| total as f64 * node.cpus as f64 / all_cpus as f64).collect();

    let mut placement = Vec::with_capacity(weights.len());
    let mut current = 0;
    let mut used = 0u32;

    for &weight in weights {
        if used > 0 && (used + weight) as f64 > targets[current] + 0.5 && current + 1 < nodes.len() {
            current += 1;
            used = 0;
        }

        placement.push(nodes[current].id);
        used += weight;
    }

    placement
}

/// Sets the NUMA node of every VM according to `QEMU_NUMA`, and writes the placement to
/// `<experiment_run_path>/numa_placement.csv`. Must be called after the links are attached.
pub fn place_vms(vms: &mut [(&str, &mut QemuVm)], experiment_run_path: &Path) -> anyhow::Result<()> {
    let mode = *NUMA_MODE.read().unwrap();
    let nodes = host_nodes();

    let placement: Vec<Option<u32>> = match mode {
        NumaMode::Off => vec![None; vms.len()],
        NumaMode::Node(node) => {
            if !nodes.iter().any(|host_node| host_node.id == node) {
                bail!("QEMU_NUMA={node} but this host has no NUMA node {node} with online CPUs");
            }
            if !numactl_available() {
                bail!("QEMU_NUMA={node} needs numactl (apt install numactl)");
            }
            vec![Some(node); vms.len()]
        },
        NumaMode::Auto => {
            if nodes.len() <= 1 {
                vec![None; vms.len()]
            }
            else if !numactl_available() {
                warn!(target: TARGET, "This host has {} NUMA nodes but numactl is not installed: VMs are not placed (apt install numactl)", nodes.len());
                vec![None; vms.len()]
            }
            else {
                let weights: Vec<u32> = vms.iter().map(|(_, vm)| vm_weight(vm)).collect();
                plan(&weights, &nodes).into_iter().map(Some).collect()
            }
        },
    };

    let mut csv = String::from("vm,vcpu,weight,numa_node\n");

    for ((name, vm), node) in vms.iter_mut().zip(&placement) {
        vm.numa_node = *node;
        let weight = vm_weight(vm);
        csv.push_str(&format!("{},{},{},{}\n", name, vm.vcpu, weight, node.map_or_else(|| String::from("-"), |n| n.to_string())));
    }

    for host_node in &nodes {
        let weight: u32 = vms.iter().zip(&placement).filter(|(_, node)| **node == Some(host_node.id)).map(|((_, vm), _)| vm_weight(vm)).sum();

        if weight == 0 {
            continue;
        }

        info!(target: TARGET, "NUMA node {}: {} threads at most on {} CPUs", host_node.id, weight, host_node.cpus);

        if weight > host_node.cpus {
            warn!(target: TARGET, "NUMA node {} may be oversubscribed ({} vCPUs + vhost workers for {} CPUs)", host_node.id, weight, host_node.cpus);
        }
    }

    fs::write(experiment_run_path.join(HOST_RESOURCES_DIR_NAME).join("numa_placement.csv"), csv).context("Could not write numa_placement.csv")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Vec<HostNode> {
        vec![HostNode { id: 0, cpus: 128 }, HostNode { id: 1, cpus: 128 }]
    }

    #[test]
    fn parses_cpu_lists() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_cpu_list(""), Vec::<u32>::new());
    }

    #[test]
    fn small_topology_stays_on_first_node() {
        // Client, router (8 vCPU, 2 NICs x 8 queues), server
        assert_eq!(plan(&[4, 24, 4], &nodes()), vec![0, 0, 0]);
    }

    #[test]
    fn large_topology_is_split_in_order() {
        // 2 guests + 16 routers (8 vCPU, 4 NICs x 8 queues): 648 > 128
        let mut weights = vec![4];
        weights.extend([40; 16]);
        weights.push(4);

        let placement = plan(&weights, &nodes());

        let on_0 = placement.iter().filter(|&&node| node == 0).count();
        assert!((8..=10).contains(&on_0), "{placement:?}");
        // Contiguous: node 0 first, then node 1
        assert!(placement.windows(2).all(|pair| pair[0] <= pair[1]), "{placement:?}");
    }

    #[test]
    fn single_node_host() {
        assert_eq!(plan(&[300, 300], &[HostNode { id: 0, cpus: 16 }]), vec![0, 0]);
    }

    #[test]
    fn parses_modes() {
        assert_eq!(NumaMode::parse("auto").unwrap(), NumaMode::Auto);
        assert_eq!(NumaMode::parse("OFF").unwrap(), NumaMode::Off);
        assert_eq!(NumaMode::parse("1").unwrap(), NumaMode::Node(1));
        assert!(NumaMode::parse("both").is_err());
    }
}
