use std::time::Duration;
use anyhow::anyhow;
use indexmap::IndexMap;
use tokio::time::sleep;
use tracing::{debug, info};
use crate::models::link::Link;
use crate::models::nodes::node::Node;
use crate::utils::qemu::network::{create_tap_pair, link_backend, LinkBackend};
use crate::utils::qemu::ports::allocate_udp_port;
use crate::utils::qemu::qmp::qmp_execute;
use crate::utils::qemu::vm::netdev_id;

const TARGET: &str = "link";

/// A physical link: two QEMU netdevs, either sending UDP datagrams to each other or plugged into two wired taps
#[derive(Debug, Clone)]
pub struct QemuLink {
    pub qmp_port_a: u16,
    pub adapter_a: u32,
    pub qmp_port_b: u16,
    pub adapter_b: u32,
}

pub fn create_link(nodes: &mut IndexMap<String, Node>, link: &Link, link_index: usize) -> anyhow::Result<QemuLink> {
    match link_backend() {
        LinkBackend::Udp => create_udp_link(nodes, link),
        LinkBackend::Tap => create_tap_link(nodes, link, link_index),
    }
}

fn vm_of<'a>(nodes: &'a mut IndexMap<String, Node>, node_name: &str) -> anyhow::Result<&'a mut crate::utils::qemu::vm::QemuVm> {
    nodes
        .get_mut(node_name)
        .and_then(|node| node.vm.as_mut())
        .ok_or_else(|| anyhow!("No VM was attached to node \"{}\"", node_name))
}

fn create_tap_link(nodes: &mut IndexMap<String, Node>, link: &Link, link_index: usize) -> anyhow::Result<QemuLink> {
    let (tap_a, tap_b) = create_tap_pair(link_index)?;

    let vm_a = vm_of(nodes, &link.node_a)?;
    vm_a.attach_tap(link.adapter_a, tap_a.clone())?;
    let qmp_port_a = vm_a.qmp_port;

    let vm_b = vm_of(nodes, &link.node_b)?;
    vm_b.attach_tap(link.adapter_b, tap_b.clone())?;
    let qmp_port_b = vm_b.qmp_port;

    debug!(target: TARGET, "Linked {} (adapter {}, {}) and {} (adapter {}, {})", link.node_a, link.adapter_a, tap_a, link.node_b, link.adapter_b, tap_b);

    Ok(QemuLink {
        qmp_port_a,
        adapter_a: link.adapter_a,
        qmp_port_b,
        adapter_b: link.adapter_b,
    })
}

fn create_udp_link(nodes: &mut IndexMap<String, Node>, link: &Link) -> anyhow::Result<QemuLink> {
    let port_a = allocate_udp_port()?;
    let port_b = allocate_udp_port()?;

    let qmp_port_a = {
        let vm = nodes
            .get_mut(&link.node_a)
            .and_then(|node| node.vm.as_mut())
            .ok_or_else(|| anyhow!("No VM was attached to node \"{}\"", link.node_a))?;

        vm.attach_udp_tunnel(link.adapter_a, port_a, port_b)?;

        vm.qmp_port
    };

    let qmp_port_b = {
        let vm = nodes
            .get_mut(&link.node_b)
            .and_then(|node| node.vm.as_mut())
            .ok_or_else(|| anyhow!("No VM was attached to node \"{}\"", link.node_b))?;

        vm.attach_udp_tunnel(link.adapter_b, port_b, port_a)?;

        vm.qmp_port
    };

    debug!(target: TARGET, "Linked {} (adapter {}, udp {}) and {} (adapter {}, udp {})", link.node_a, link.adapter_a, port_a, link.node_b, link.adapter_b, port_b);

    Ok(QemuLink {
        qmp_port_a,
        adapter_a: link.adapter_a,
        qmp_port_b,
        adapter_b: link.adapter_b,
    })
}

pub async fn link_failure(link: Link, qemu_link: QemuLink, happens_at: u64) -> anyhow::Result<()> {
    info!(
        target: TARGET,
        "Waiting {} seconds before programmed link failure between {} (adapter {}) and {} (adapter {})",
        happens_at,
        link.node_a,
        link.adapter_a,
        link.node_b,
        link.adapter_b
    );

    sleep(Duration::from_secs(happens_at)).await;

    info!(
        target: TARGET,
        "Waited {} seconds, programmed link failure between {} (adapter {}) and {} (adapter {})",
        happens_at,
        link.node_a,
        link.adapter_a,
        link.node_b,
        link.adapter_b
    );

    // Like unplugging the cable: both NICs lose carrier and no frame crosses the link anymore
    let arguments_a = serde_json::json!({ "name": netdev_id(qemu_link.adapter_a), "up": false });
    let arguments_b = serde_json::json!({ "name": netdev_id(qemu_link.adapter_b), "up": false });

    let (result_a, result_b) = tokio::join!(
        qmp_execute(qemu_link.qmp_port_a, "set_link", Some(arguments_a)),
        qmp_execute(qemu_link.qmp_port_b, "set_link", Some(arguments_b))
    );

    result_a?;
    result_b?;

    Ok(())
}
