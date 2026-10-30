use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::exit;
use anyhow::{anyhow, bail};
use indexmap::IndexMap;
use tracing::{error, info, warn};
use strum::VariantArray;
use crate::args::generate::{GenerateCommand, GenerateSubcommand};
use crate::commands::run::{INTER_XP_WAIT, XP_INTER_NODE_CONNECTION_WAIT_ESTIMATION, POST_XP_WAIT};
use crate::models::experiment::Experiment;
use crate::models::hardware_resources::HardwareResources;
use crate::models::network::Network;
use crate::models::nic::{Nic, NicType};
use crate::models::nodes::guest::Guest;
use crate::models::nodes::node::{GenericNodeType, Node, NodeType};
use crate::models::nodes::router::Router;
use crate::models::operating_system::OperatingSystem;
use crate::models::protocol::RoutingProtocol;
use crate::models::test::Test;
use crate::models::topology::Topology;
use crate::utils::args::experiment_selection_args_to_string;
use crate::utils::files::dir::create_dir_if_does_not_exist;
use crate::utils::files::experiments::EXPERIMENTS_PATH;
use crate::utils::files::network_stacks::parse_network_stack_list_file;
use crate::utils::files::oses::{parse_os_list_file, OS_LIST_PATH};
use crate::utils::files::routing_stacks::parse_routing_stack_list_file;
use crate::utils::files::test_batches::{parse_test_batch_list_file, TEST_BATCH_LIST_PATH};
use crate::utils::files::topologies::{parse_topologies_list_file, TOPOLOGIES_LIST_PATH};

const TARGET: &str = "generate";

pub fn generate(generate_command: GenerateCommand) -> anyhow::Result<()> {
    let experiment_selection = match &generate_command.command {
        GenerateSubcommand::Run(run_command) => &run_command.experiment_selection,
        GenerateSubcommand::Files { experiment_selection, .. } => &experiment_selection,
        GenerateSubcommand::Commands { experiment_selection, .. } => {
            &experiment_selection
        }
    };

    let network_stack_list = parse_network_stack_list_file()?;
    let routing_stack_list = parse_routing_stack_list_file()?;
    let os_list = parse_os_list_file(network_stack_list.keys().collect(), routing_stack_list.keys().collect())?;
    let oses_to_use = get_oses_to_use(&experiment_selection.os, os_list);

    let test_batch_list = parse_test_batch_list_file()?;
    let test_batches_to_use = get_test_batches_to_use(&experiment_selection.test_batch, test_batch_list);

    let resources_to_use = match &experiment_selection.resources {
        None => HardwareResources::VARIANTS.to_vec(),
        Some(resources) => vec![resources.to_owned()],
    };

    let nic_types_to_use = match &experiment_selection.nic {
        None => NicType::VARIANTS.to_vec(),
        Some(nic_type) => vec![nic_type.to_owned()],
    };

    let topology_list = parse_topologies_list_file()?;
    let topologies_to_use = get_topologies_to_use(&experiment_selection.topology, topology_list, &experiment_selection.protocol);

    if oses_to_use.is_empty() || test_batches_to_use.is_empty() || topologies_to_use.is_empty() {
        warn!(target: TARGET, "At least of the lists is empty, no experiment will be generated");
        warn!(target: TARGET, "Exiting");
        exit(0);
    }

    let override_ = match &generate_command.command {
        GenerateSubcommand::Files { override_, .. } => {
            if override_ == &true {
                warn!(target: TARGET, "Override is set, existing experiment will be overwritten");
            }
            else {
                warn!(target: TARGET, "Override is not set, existing experiment will NOT be overwritten");
            }

            *override_
        },
        _ => false
    };

    if let GenerateSubcommand::Commands { experiment_selection, runs } = &generate_command.command {
        println!("# cargo run -- generate commands {}", experiment_selection_args_to_string(&experiment_selection, *runs));
    }

    let mut total_xp_seconds = 0;

    let experiment_dir = create_dir_if_does_not_exist(EXPERIMENTS_PATH.clone())?;
    let mut experiment_count = 0usize;

    for (os_name, os) in &oses_to_use {
        info!(target: TARGET, "Experiments for {}", os_name);
        let mut os_xp_seconds = 0;

        let os_dir = create_dir_if_does_not_exist(experiment_dir.join(os_name))?;

        for (test_batch_name, test_batch) in &test_batches_to_use {
            let test_batch_dir = create_dir_if_does_not_exist(os_dir.join(test_batch_name))?;

            for resources in &resources_to_use {
                let resources_dir = create_dir_if_does_not_exist(test_batch_dir.join(resources.to_string()))?;

                for nic in &nic_types_to_use {
                    let nic_dir = create_dir_if_does_not_exist(resources_dir.join(nic.to_string()))?;

                    for (topology_name, topology) in &topologies_to_use {
                        let topology_dir = create_dir_if_does_not_exist(nic_dir.join(topology_name))?;

                        'routing_protocol: for routing_protocol in &topology.supported_routing_protocols {

                            if routing_protocol != &RoutingProtocol::Static {
                                if let Some(routing_stack_name) = &os.routing_stack {
                                    let routing_stack = routing_stack_list.get(routing_stack_name).ok_or_else(|| anyhow!("No routing stack found for {}", os.network_stack))?;

                                    // Skip experiment generation if the routing stack does not support the protocol
                                    match routing_protocol {
                                        RoutingProtocol::Rip if routing_stack.rip.is_none() => continue,
                                        RoutingProtocol::Ospf if routing_stack.ospf.is_none() => continue,
                                        RoutingProtocol::Bgp if routing_stack.bgp.is_none() => continue,
                                        RoutingProtocol::Mpls if routing_stack.mpls.is_none() => continue,
                                        RoutingProtocol::Mixed => {
                                            let protocols_to_use = topology.network.nodes
                                                .iter()
                                                .filter_map(|(_, n)| n.node_type.get_router())
                                                .filter_map(|r| r.routes_config.get(&RoutingProtocol::Mixed))
                                                .map(|r| r.unwrap_mixed().keys().collect::<Vec<_>>())
                                                .flatten()
                                                .collect::<HashSet<_>>();

                                            for protocol in protocols_to_use {
                                                match protocol {
                                                    RoutingProtocol::Rip if routing_stack.rip.is_none() => continue 'routing_protocol,
                                                    RoutingProtocol::Ospf if routing_stack.ospf.is_none() => continue 'routing_protocol,
                                                    RoutingProtocol::Bgp if routing_stack.bgp.is_none() => continue 'routing_protocol,
                                                    RoutingProtocol::Mpls if routing_stack.mpls.is_none() => continue 'routing_protocol,
                                                    RoutingProtocol::Mixed => panic!("Mixed protocol recursion"),
                                                    _ => {}
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                } else {
                                    // Skip experiment generation if there is no routing stack and the protocol is no static
                                    continue;
                                }
                            }

                            let routing_protocol_dir = create_dir_if_does_not_exist(topology_dir.join(routing_protocol.to_string()))?;

                            let has_failures = !topology.failure_events.is_empty();
                            let with_failures_options = match experiment_selection.failures {
                                Some(failures) => match failures {
                                    true => match has_failures {
                                        true => vec![true],
                                        false => bail!("No failures are listed in the \"{topology_name}\" topology")
                                    }
                                    false => match has_failures {
                                        true => vec![false],
                                        false => vec![false]
                                    }
                                }
                                None => match has_failures {
                                    true => vec![false, true],
                                    false => vec![false]
                                }
                            };

                            for with_failures in with_failures_options {
                                let with_failures_string = match with_failures {
                                    true => String::from("failures"),
                                    false => String::from("no-failures"),
                                };

                                let with_failures_dir = create_dir_if_does_not_exist(routing_protocol_dir.join(&with_failures_string))?;

                                let experiment_path = with_failures_dir.join("experiment.json");

                                experiment_count += 1;

                                let mut longest_xp = 0;

                                for test in test_batch {
                                    let total_duration = test.fire_at + test.duration;

                                    if total_duration > longest_xp {
                                        longest_xp = total_duration
                                    }
                                }

                                let runs = match &generate_command.command {
                                    GenerateSubcommand::Run(run_command) => run_command.run_command.runs,
                                    GenerateSubcommand::Files { .. } => 1,
                                    GenerateSubcommand::Commands { runs, .. } => *runs
                                };

                                os_xp_seconds += (
                                    180 * 1
                                    + XP_INTER_NODE_CONNECTION_WAIT_ESTIMATION as usize * topology.network.nodes.len()
                                    + longest_xp as usize
                                    + POST_XP_WAIT as usize
                                    + INTER_XP_WAIT as usize
                                ) * runs as usize;

                                match &generate_command.command {
                                    GenerateSubcommand::Run(_) | GenerateSubcommand::Files { .. } => {
                                        info!(
                                            target: TARGET,
                                            "{}, {}, {}, {}, {}, {}",
                                            os_name,
                                            test_batch_name,
                                            resources,
                                            topology_name,
                                            routing_protocol,
                                            with_failures_string
                                        );
                                    }
                                    GenerateSubcommand::Commands { runs, .. } => {
                                        println!(
                                            "cargo run -- run --os \"{}\" --test-batch \"{}\" --resources \"{}\" --topology \"{}\" --nic \"{}\" --protocol \"{}\" --failures {} --runs {}",
                                            os_name,
                                            test_batch_name,
                                            resources.to_command(),
                                            topology_name,
                                            nic.to_command(),
                                            routing_protocol,
                                            with_failures,
                                            runs
                                        );
                                        continue;
                                    }
                                }

                                if experiment_path.exists() && override_ == false {
                                    continue;
                                }

                                let mut router_id: u8 = 1;

                                let mut nodes = IndexMap::new();

                                for (key, generic_node) in &topology.network.nodes {
                                    let (vcpu, ram, node_type) = match &generic_node.node_type {
                                        GenericNodeType::Guest(guest) => {
                                            let node_type = NodeType::Guest(Guest {
                                                ip: guest.ip,
                                            });

                                            (guest.vcpu, guest.ram, node_type)
                                        },
                                        GenericNodeType::Router(router) => {
                                            let (vcpu, ram) = resources.to_vcpu_and_ram();
                                            let routes = match router.routes_config.get(routing_protocol) {
                                                Some(routes) => routes.clone(),
                                                None => return Err(anyhow!("Routing protocol \"{}\" has no route in router \"{}\"", routing_protocol, key)),
                                            };

                                            let mut nics = IndexMap::new();

                                            for (index, ip) in &router.ips {
                                                let router_nic = Nic {
                                                    nic_type: nic.clone(),
                                                    ip_address: *ip,
                                                };

                                                nics.insert(index.clone(), router_nic);
                                            }

                                            let node_type = NodeType::Router(Router {
                                                id: format!("{router_id}.{router_id}.{router_id}.{router_id}"),
                                                os_name: os_name.to_owned(),
                                                number_nics: router.number_nics,
                                                nics,
                                                routes_config: routes,
                                            });

                                            (vcpu, ram, node_type)
                                        }
                                    };

                                    let node = Node {
                                        vcpu,
                                        ram,
                                        x: generic_node.x,
                                        y: generic_node.y,
                                        node_type,
                                        vm: None,
                                        distant_networks: vec![],
                                    };

                                    nodes.insert(key.to_owned(), node);

                                    router_id += 1;
                                }

                                for (index, physical_link) in topology.network.physical_links.iter().enumerate() {
                                    if !nodes.contains_key(&physical_link.node_a) {
                                        return Err(anyhow!("Node \"{}\" does not exist in topology \"{}\" for link {}", physical_link.node_a, topology_name, index));
                                    }

                                    if !nodes.contains_key(&physical_link.node_b) {
                                        return Err(anyhow!("Node \"{}\" does not exist in topology \"{}\" for link {}", physical_link.node_b, topology_name, index));
                                    }
                                }

                                let experiment = Experiment {
                                    experiment_name: format!("{topology_name},{os_name},{test_batch_name},{resources},{nic},{routing_protocol},{with_failures_string}"),
                                    network: Network {
                                        nodes,
                                        physical_links: topology.network.physical_links.clone(),
                                        failure_events: match with_failures {
                                            true => topology.failure_events.clone(),
                                            false => Vec::new()
                                        },
                                    },
                                    test_batch: test_batch.clone(),
                                };

                                let experiment_json = serde_json::to_string_pretty(&experiment)?;

                                let mut experiment_file = OpenOptions::new()
                                    .create(true)
                                    .write(true)
                                    .truncate(true)
                                    .open(experiment_path)?;

                                experiment_file.write_all(experiment_json.as_bytes())?;
                            }
                        }
                    }
                }
            }
        }

        total_xp_seconds += os_xp_seconds;
        let minutes = os_xp_seconds / 60;
        let hours = minutes / 60;
        let days = hours / 24;
        info!(target: TARGET, "Max total experiment duration for {} is {:0>3}:{:0>2}:{:0>2}:{:0>2} (D:H:M:S)", os_name, days, hours % 24, minutes % 60, os_xp_seconds % 60);
    }

    if matches!(&generate_command.command, GenerateSubcommand::Commands { .. }) {
        println!()
    }

    // Experiment count
    info!(target: TARGET, "Total of {} individual experiment",  experiment_count);

    let minutes = total_xp_seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    info!(target: TARGET, "Max total experiment duration is {:0>3}:{:0>2}:{:0>2}:{:0>2} (D:H:M:S)", days, hours % 24, minutes % 60, total_xp_seconds % 60);

    Ok(())
}

fn get_oses_to_use(command_os: &Option<String>, mut os_list: IndexMap<String, OperatingSystem>) -> IndexMap<String, OperatingSystem> {
    if let Some(os_name) = &command_os {
        if !os_list.contains_key(os_name) {
            error!(target: TARGET, "{} was not found in \"{}\"", os_name, OS_LIST_PATH.display());
            exit(1);
        }

        os_list.retain(|o, _| o == os_name);
    }

    os_list
}

fn get_test_batches_to_use(command_test: &Option<String>, mut test_batch_list: IndexMap<String, Vec<Test>>) -> IndexMap<String, Vec<Test>> {
   if let Some(test) = &command_test {
       if !test_batch_list.contains_key(test) {
           error!(target: TARGET, "{} was not found in \"{}\"", test, TEST_BATCH_LIST_PATH.display());
           exit(1);
       }

       test_batch_list.retain(|t, _| t == test);
   }

    test_batch_list
}

fn get_topologies_to_use(command_topology: &Option<String>, topology_list: IndexMap<String, Topology>, command_routing_protocol: &Option<RoutingProtocol>) -> IndexMap<String, Topology> {
    let mut topology_list = match &command_topology {
        None => topology_list,
        Some(topology) => match topology_list.get_key_value(topology) {
            Some(topology) => IndexMap::from([(topology.0.to_owned(), topology.1.to_owned())]),
            None => {
                error!(target: TARGET, "{} was not found in \"{}\"", topology, TOPOLOGIES_LIST_PATH.display());
                exit(1);
            }
        }
    };

    if let Some(routing_protocol) = command_routing_protocol {
        for (key, topology) in topology_list.iter_mut() {
            if !topology.supported_routing_protocols.contains(&routing_protocol) {
                error!(target: TARGET, "{} virtual topology was not found in the topology \"{}\"", routing_protocol, key);
                exit(1);
            }

            topology.supported_routing_protocols.retain(|r| r == routing_protocol);
        }
    }

    topology_list
}