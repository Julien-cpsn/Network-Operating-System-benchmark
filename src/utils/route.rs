use std::net::Ipv4Addr;
use crate::models::nodes::node::{DistantNetwork, Node, NodeType};
use crate::models::nodes::router::Router;
use crate::utils::os_commands::guest::GUEST_NIC_INDEX;
use anyhow::anyhow;
use cidr::{Ipv4Cidr, Ipv4Inet};
use tracing::debug;

const TARGET: &str = "route";

pub fn generate_distant_network_from_test(from_node_name: &str, from_node: &mut Node, distant_ip: Ipv4Inet, gateway_network: Ipv4Cidr) -> anyhow::Result<()> {
    let distant_network = distant_ip.network();
    
    if matches!(from_node.node_type, NodeType::Router(Router { .. })) {
        return Err(anyhow!("Cannot test from/to router nodes, only guests"))
    }

    let mut gateway_octets = gateway_network.first_address().octets();
    gateway_octets[3] += 1;
    let gateway = Ipv4Addr::from_octets(gateway_octets);

    debug!(target: TARGET, "Registered {} network via {} on guest {}", &distant_network, &gateway, &from_node_name);

    from_node.distant_networks.push(DistantNetwork {
        network: distant_network,
        gateway,
        adapter_index: GUEST_NIC_INDEX,
        target_ip: distant_ip.address(),
    });

    Ok(())
}