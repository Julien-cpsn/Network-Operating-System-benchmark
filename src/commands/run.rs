use std::fs;
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use anyhow::anyhow;
use indexmap::IndexMap;
use tracing::{debug, error, info, warn};
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::util::SubscriberInitExt;
use crate::args::run::RunCommand;
use crate::GUEST_IMAGE_PATH;
use crate::models::experiment::Experiment;
use crate::models::failure_event::FailureEvent;
use crate::models::network_stack::NetworkStack;
use crate::models::nodes::node::{NodeType};
use crate::models::operating_system::OperatingSystem;
use crate::models::os_command::{CommandContext, OsCommand};
use crate::models::routes::route_config::RouteConfig;
use crate::models::routing_stack::RoutingStack;
use crate::utils::env::{harvest_env_variables, work_dir};
use crate::utils::files::experiments::parse_experiments_files;
use crate::utils::files::network_stacks::parse_network_stack_list_file;
use crate::utils::files::oses::parse_os_list_file;
use crate::utils::files::results_dir::RESULT_DIR_PATH;
use crate::utils::files::routing_stacks::parse_routing_stack_list_file;
use crate::utils::files::shared_dir::SHARED_DIR_PATH;
use crate::utils::link::{create_link, link_failure};
use crate::utils::qemu::host_monitor::{host_monitor_task, MonitoredVm};
use crate::utils::qemu::image::resolve_images;
use crate::utils::qemu::ports::reset_port_allocator;
use crate::utils::qemu::network::{delete_all_taps, link_backend};
use crate::utils::qemu::process::{stop_all_vms, stop_and_delete_all_vms};
use crate::utils::qemu::vm::{sanitize_name, QemuVm};
use crate::utils::log::{find_and_delete_log_files, setup_experiment_logger, EXPERIMENT_LOG_FILE_NAME, find_and_delete_experiment_log_file};
use crate::utils::monitor::monitor_task;
use crate::utils::os_commands::execute::{execute_commands_from_node};
use crate::utils::os_commands::guest::{guest_add_route_commands, guest_config_commands, GUEST_INPUT_READY, guest_test_commands, guest_test_batch_start_commands, guest_test_batch_end_commands};
use crate::utils::os_commands::router::{router_add_ip_address_commands, router_login_commands, router_start_network_stack_commands, router_start_routing_stack_commands, router_stop_network_stack_commands, router_stop_routing_stack_commands};
use crate::utils::os_commands::routing::bpg_config::router_configure_bgp_commands;
use crate::utils::os_commands::routing::ospf_config::router_configure_ospf_commands;
use crate::utils::os_commands::routing::rip_config::router_configure_rip_commands;
use crate::utils::os_commands::routing::static_route::router_configure_static_commands;
use crate::utils::route::generate_distant_network_from_test;
use crate::utils::test::{clear_shared_dir, harvest_results, test_task};
use crate::utils::utils::{filter_guests, filter_guests_mut, filter_routers, filter_routers_mut};

pub const INTER_XP_WAIT: u64 = 1;
pub const INTRA_XP_WAIT: u64 = 60;
pub const POST_XP_WAIT: u64 = 10;

pub async fn run(run_command: RunCommand) -> anyhow::Result<()> {
    const TARGET: &str = "run";

    harvest_env_variables(true);

    if !SHARED_DIR_PATH.exists() {
        fs::create_dir(&*SHARED_DIR_PATH)?;
    }

    if !RESULT_DIR_PATH.exists() {
        fs::create_dir(&*RESULT_DIR_PATH)?;
    }

    let mut experiments = parse_experiments_files(&run_command.experiment_selection)?;

    if experiments.is_empty() {
        warn!(target: TARGET, "No experiment found");
        exit(0);
    }

    let network_stack_list = parse_network_stack_list_file()?;
    let routing_stack_list = parse_routing_stack_list_file()?;
    let os_list = parse_os_list_file(network_stack_list.keys().collect(), routing_stack_list.keys().collect())?;

    let guest_image = resolve_images(&[GUEST_IMAGE_PATH.get().unwrap().clone()])?.remove(0);

    let cancellation_token = CancellationToken::new();
    let token = cancellation_token.clone();

    ctrlc::set_handler(move || {
        cancellation_token.cancel();

        // Console interactions are blocking, so the experiment may not notice the cancellation:
        // in that case, stop the VMs from here and exit
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(3));
            warn!(target: "run", "Experiment did not stop by itself, stopping VMs");
            let _ = stop_all_vms(&work_dir());
            let _ = delete_all_taps();
            exit(130);
        });
    }).expect("Error setting Ctrl-C handler");

    let experiments_start = Instant::now();

    let nb_experiments = experiments.len();
    let nb_runs = run_command.run_command.runs;

    'experiment_loop: for (experiment_index, experiment) in experiments.drain(..).enumerate() {
        let experiment_path = RESULT_DIR_PATH.join(&experiment.experiment_name);
        if experiment_path.exists() {
           fs::remove_dir_all(&experiment_path)?;
        }

        for run_index in 1..nb_runs + 1 {
            let experiment_run_path = experiment_path.join(format!("run_{run_index}"));

            fs::create_dir_all(&experiment_run_path)?;

            find_and_delete_experiment_log_file(&experiment_run_path)?;

            let (dispatcher, _file_guard) = setup_experiment_logger(&experiment_run_path, EXPERIMENT_LOG_FILE_NAME)?;
            let _log_guard = dispatcher.set_default();

            tokio::select! {
                result = run_experiment(
                    experiment_index + 1,
                    nb_experiments,
                    run_index,
                    nb_runs,
                    &run_command,
                    &os_list,
                    &network_stack_list,
                    &routing_stack_list,
                    &guest_image,
                    experiment.clone(),
                    &experiment_run_path
                ) => {
                    if let Err(error) = result {
                        error!(target: TARGET, "{}", error);
                        exit(1);
                    }
                },
                 _ = token.cancelled() => {
                    info!(target: TARGET, "Stopping experiment");
                    stop_all_vms(&work_dir())?;
                    delete_all_taps()?;
                    break 'experiment_loop;
                }
            }

            sleep(Duration::from_secs(INTER_XP_WAIT)).await;
        }
    }

    // The taps of the last experiment (their VMs must be stopped first)
    if !run_command.run_command.no_stop {
        delete_all_taps()?;
    }

    let experiments_duration = experiments_start.elapsed();

    info!(target: TARGET, "Finished");
    info!(target: TARGET, "Total duration: {:?}", experiments_duration);

    Ok(())
}

pub async fn run_experiment(
    experiment_index: usize,
    nb_experiments: usize,
    run_index: u8,
    nb_runs: u8,
    run_command: &RunCommand,
    oses: &IndexMap<String, OperatingSystem>,
    network_stacks: &IndexMap<String, NetworkStack>,
    routing_stacks: &IndexMap<String, RoutingStack>,
    guest_image: &PathBuf,
    mut experiment: Experiment,
    experiment_run_path: &PathBuf
) -> anyhow::Result<()> {
    const TARGET: &str = "experiment";

    info!(target: TARGET, "----- Running experiment {}/{}, run {}/{}: {} -----", experiment_index, nb_experiments, run_index, nb_runs, experiment.experiment_name);

    /* INITIALIZATION */

    let experiment_start = Instant::now();

    info!(target: TARGET, "Link backend: {}", link_backend());

    // Stops the VMs left by a previous experiment and deletes their disks
    let work_dir = work_dir();
    stop_and_delete_all_vms(&work_dir)?;
    reset_port_allocator();
    find_and_delete_log_files(&experiment_run_path, &experiment.network.nodes)?;

    let mut router_images = IndexMap::new();

    for (router_name, node) in filter_routers(&experiment.network.nodes) {
        let router = node.unwrap_router();
        debug!(target: TARGET, "Ensuring \"{}\" operating system image ({}) exists", router_name, router.os_name);

        let operating_system = oses.get(&router.os_name).ok_or_else(|| anyhow!("No operating system found for {}", router.os_name))?;
        router_images.insert(router_name.clone(), resolve_images(&operating_system.images_path)?);
    }

    /* SETUP */

    let experiment_dir = work_dir.join(sanitize_name(&experiment.experiment_name));
    fs::create_dir_all(&experiment_dir)?;

    for (node_index, (node_name, node)) in experiment.network.nodes.iter_mut().enumerate() {
        let vm = match &node.node_type {
            NodeType::Router(_) => QemuVm::new_router(&experiment_dir, node_index, node_name, node, &router_images[node_name]).await?,
            NodeType::Guest(_) => QemuVm::new_guest(&experiment_dir, node_index, node_name, node, guest_image).await?,
        };

        node.vm = Some(vm);
    }

    let mut links = Vec::new();

    for (index, link) in experiment.network.physical_links.iter().enumerate() {
        if !experiment.network.nodes.contains_key(&link.node_a) {
            return Err(anyhow!("Node a \"{}\" not found in link {}", &link.node_a, index));
        }
        if !experiment.network.nodes.contains_key(&link.node_b) {
            return Err(anyhow!("Node b \"{}\" not found in link {}", &link.node_b, index));
        }

        let qemu_link = create_link(&mut experiment.network.nodes, link, index)?;

        links.push((link.clone(), qemu_link));
    }

    for test in &experiment.test_batch {
        let to_node = experiment.network.nodes.get(&test.to).ok_or_else(|| anyhow!("Node \"{}\" not found in test {}", &test.to, &test.name))?;
        let to_node_network = to_node.unwrap_guest().ip.network();

        let from_node = experiment.network.nodes.get(&test.from).ok_or_else(|| anyhow!("Node \"{}\" not found in test {}", test.from, &test.name))?;
        let from_node_network = from_node.unwrap_guest().ip.network();

        if matches!(from_node.node_type, NodeType::Router(..)) || matches!(to_node.node_type, NodeType::Router(..)) {
            return Err(anyhow!("Cannot test from/to router nodes, only guests"))
        }
        
        generate_distant_network_from_test(&test.from, experiment.network.nodes.get_mut(&test.from).unwrap(), to_node_network, from_node_network)?;
        generate_distant_network_from_test(&test.to, experiment.network.nodes.get_mut(&test.to).unwrap(), from_node_network, to_node_network)?;
    }

    /* START */

    for (guest_name, node) in filter_guests_mut(&mut experiment.network.nodes) {
        info!(target: TARGET, "Starting guest: {}", guest_name);
        let vm = node.vm.as_mut().ok_or_else(|| anyhow!("No VM was attached to the guest"))?;
        vm.start().await?;
        info!(target: TARGET, "Telnet console: telnet {} {}", vm.console_host, vm.console_port);
    }

    /* CONFIG */

    for (guest_name, node) in filter_guests(&experiment.network.nodes) {
        let guest = node.unwrap_guest();
        let mut config_commands = guest_config_commands(guest.ip.address());

        for distant_network in &node.distant_networks {
            let add_route_command = guest_add_route_commands(&distant_network);
            config_commands.extend(add_route_command);
        }

        config_commands.push(OsCommand::new_text(GUEST_INPUT_READY, "netserver", true, false));
        config_commands.push(OsCommand::new_line(GUEST_INPUT_READY));

        execute_commands_from_node(
            &experiment_run_path,
            &guest_name,
            node.vm.as_ref().unwrap(),
            config_commands,
            Some(4_000),
            None
        )?;
    }

    for (router_name, node) in filter_routers_mut(&mut experiment.network.nodes) {
        info!(target: TARGET, "Starting router: {}", router_name);
        // Login
        let router = node.unwrap_router();
        let os = oses.get(&router.os_name).ok_or_else(|| anyhow!("No operating system {} found for {}", router.os_name, router_name))?;

        let command_context = CommandContext {
            router_name,
            router: &router,
            os,
            network_stack: None,
            routing_stack: None,
        };

        let login_commands = router_login_commands(&command_context)?;

        let vm = node.vm.as_mut().ok_or_else(|| anyhow!("No VM was attached to the router"))?;
        vm.start().await?;

        info!(target: TARGET, "Telnet console: telnet {} {}", vm.console_host, vm.console_port);

        execute_commands_from_node(
            &experiment_run_path,
            &router_name,
            &vm,
            login_commands,
            Some(180_000),
            None
        )?;
    }

    for (router_name, node) in filter_routers(&experiment.network.nodes) {
        let vm = node.vm.as_ref().ok_or_else(|| anyhow!("No VM was attached to the router"))?;
        let router = node.unwrap_router();
        let os = oses.get(&router.os_name).ok_or_else(|| anyhow!("No operating system {} found for {}", router.os_name, router_name))?;
        let network_stack = network_stacks.get(&os.network_stack).ok_or_else(|| anyhow!("No network stack found for {}", os.network_stack))?;
        let routing_stack = match &os.routing_stack {
            None => None,
            Some(routing_stack) => Some(routing_stacks.get(routing_stack).ok_or_else(|| anyhow!("No routing stack found for {}", os.network_stack))?)
        };

        let command_context = CommandContext {
            router_name,
            router: &router,
            os,
            network_stack: Some(network_stack),
            routing_stack,
        };

        // Skip all commands
        if run_command.run_command.os_setup {
            continue;
        }

        // Network stack setup

        let start_network_stack_commands = router_start_network_stack_commands(&command_context, &network_stack)?;
        let stop_network_stack_commands = router_stop_network_stack_commands(&command_context, &network_stack)?;
        let mut add_ip_address_commands = Vec::new();
        let mut static_routes_commands = Vec::new();

        for (index, nic) in &router.nics {
            add_ip_address_commands.extend(router_add_ip_address_commands(&command_context, &index, &nic.nic_type, &nic.ip_address)?);
        }

        if let RouteConfig::Static(static_routes) = &router.routes_config {
            static_routes_commands.extend(router_configure_static_commands(&command_context, &static_routes)?);
        }

        // Routing stack setup

        let (start_routing_stack_commands, stop_routing_stack_commands) = match &os.routing_stack {
            Some(routing_stack) => {
                let routing_stack = routing_stacks.get(routing_stack.as_str()).ok_or_else(|| anyhow!("No routing stack found for {}", os.network_stack))?;

                (
                    router_start_routing_stack_commands(&command_context, &routing_stack)?,
                    router_stop_routing_stack_commands(&command_context, &routing_stack)?,
                )
            }
            None => (Vec::new(), Vec::new()),
        };

        // Routing protocols (static, RIP, OSPF, BGP, MPLS)

        let routing_commands = match routing_stack {
            None => Vec::new(),
            Some(_) => match &router.routes_config {
                RouteConfig::Rip(rip_config) => router_configure_rip_commands(&command_context, &rip_config)?,
                RouteConfig::Ospf(ospf_config) => router_configure_ospf_commands(&command_context, &ospf_config)?,
                RouteConfig::Bgp(bgp_config) => router_configure_bgp_commands(&command_context, &bgp_config)?,
                RouteConfig::Mpls => Vec::new(),
                RouteConfig::Mixed(mixed_config) => {
                    let mut commands = Vec::new();

                    for config in mixed_config.values() {
                        let c = match config {
                            RouteConfig::Static(static_config) => router_configure_static_commands(&command_context, &static_config)?,
                            RouteConfig::Rip(rip_config) => router_configure_rip_commands(&command_context, &rip_config)?,
                            RouteConfig::Ospf(ospf_config) => router_configure_ospf_commands(&command_context, &ospf_config)?,
                            RouteConfig::Bgp(bgp_config) => router_configure_bgp_commands(&command_context, &bgp_config)?,
                            RouteConfig::Mpls => Vec::new(),
                            RouteConfig::Mixed(_) => panic!("Mixed protocol recursion"),
                        };

                        commands.extend(c);
                    }

                    commands
                }
                // Handled before
                RouteConfig::Static(_) => Vec::new(),
            }
        };

        // Send commands

        let commands = vec![
            start_network_stack_commands,
            add_ip_address_commands,
            static_routes_commands,
            start_routing_stack_commands,
            routing_commands,
            stop_routing_stack_commands,
            stop_network_stack_commands
        ]
            .concat();

        execute_commands_from_node(
            &experiment_run_path,
            &router_name,
            &vm,
            commands,
            Some(4_000),
            None
        )?;
    }

    /* MONITOR ROUTER */

    let stop_monitoring = Arc::new(AtomicBool::new(false));
    let mut monitor_threads = JoinSet::new();

    // Host side: every VM (routers, but also clients and servers, to locate bottlenecks)
    let monitored_vms: Vec<MonitoredVm> = filter_routers(&experiment.network.nodes)
        .values()
        .filter_map(|node| node.vm.as_ref().and_then(MonitoredVm::from_vm))
        .collect();

    let host_monitor = {
        let (run_path, stop) = (experiment_run_path.clone(), stop_monitoring.clone());
        tokio::task::spawn_blocking(move || host_monitor_task(run_path, monitored_vms, stop))
    };

    for (router_name, node) in filter_routers(&experiment.network.nodes) {
        let router  = node.unwrap_router();
        let os = oses.get(&router.os_name).ok_or_else(|| anyhow!("No operating system {} found for {}", router.os_name, router_name))?;

        let command_context = CommandContext {
            router_name,
            router: &router,
            os,
            network_stack: None,
            routing_stack: None,
        };

        if let Some(monitor) = &os.resources_monitor_commands {
            let vm = node.vm.as_ref().unwrap();

            let monitor_commands = monitor
                .iter()
                .filter_map(|c| c.to_os_command(&command_context, None))
                .collect();

            monitor_threads.spawn(monitor_task(
                experiment_run_path.clone(),
                router_name.clone(),
                vm.console_host.clone(),
                vm.console_port,
                os.input_ready.clone(),
                monitor_commands,
                stop_monitoring.clone()
            ));
        }
    }

    if !run_command.run_command.no_sleep {
        info!(target: TARGET, "Waiting 1 minute");
        // Sleep 1 min so that the network can discuss
        sleep(Duration::from_secs(INTRA_XP_WAIT)).await;
    }

    /* FAILURE EVENTS */

    let mut failure_events_threads = JoinSet::new();

    for failure_event in &experiment.network.failure_events {
        match failure_event {
            FailureEvent::Link { link_index, happens_at } => {
                let (link, qemu_link) = links.get(*link_index).cloned().ok_or_else(|| anyhow!("Link {} not found for failure event", link_index))?;

                failure_events_threads.spawn(link_failure(link, qemu_link, *happens_at));
            }
        }
    }

    /* RUN */

    if !run_command.run_command.no_test {
        info!(target: TARGET, "Experiment start");

        let mut test_threads = JoinSet::new();

        for (from, tests) in experiment.test_batch_by_node() {
            let (from_node_name, from_node) = experiment.network.nodes.get_key_value(&from).unwrap();
            let from_vm = from_node.vm.as_ref().unwrap();

            let mut test_batch_commands = guest_test_batch_start_commands();
            
            for test in tests {
                info!(target: TARGET, "Running test in {} seconds : {} (duration: {} seconds)", &test.fire_at, &test.name, &test.duration);

                let to_node = experiment.network.nodes.get(&test.to).unwrap();
                let to_node_ip = to_node.unwrap_guest().ip.address();
                let test_commands = guest_test_commands(&experiment.experiment_name, &test, to_node_ip);
                
                test_batch_commands.extend(test_commands);
            }
            
            test_batch_commands.extend(guest_test_batch_end_commands(&experiment.test_batch));
            
            test_threads.spawn(test_task(
                experiment_run_path.clone(),
                from_node_name.to_owned(),
                from_vm.console_host.clone(),
                from_vm.console_port,
                test_batch_commands
            ));
        }

        test_threads.join_all().await;

        info!(target: TARGET, "Experiment end");
    }

    info!(target: TARGET, "Waiting 10 seconds");
    // Sleep 10 seconds so we can have monitoring after the end of the experiment
    sleep(Duration::from_secs(POST_XP_WAIT)).await;

    stop_monitoring.store(true, Ordering::Relaxed);
    monitor_threads.join_all().await;

    match host_monitor.await {
        Ok(Ok(())) => {},
        Ok(Err(error)) => error!(target: TARGET, "Host monitoring failed: {:?}", error),
        Err(error) => error!(target: TARGET, "Host monitoring task failed: {}", error),
    }

    if !run_command.run_command.no_test {
        for test in &experiment.test_batch {
            harvest_results(&experiment_run_path, &test)?;
        }
    }

    /* STOP */

    clear_shared_dir()?;
    
    if !run_command.run_command.no_stop {
        for (node_name, node) in experiment.network.nodes.iter_mut() {
            debug!(target: TARGET, "Stopping node: {}", node_name);
            if let Some(vm) = node.vm.as_mut() {
                vm.stop().await?;
            }
        }
    }

    /* END */

    let experiment_duration = experiment_start.elapsed();

    info!(target: TARGET, "Experiment finished");
    info!(target: TARGET, "Duration: {:?}", experiment_duration);
    info!(target: TARGET, "");

    if run_command.run_command.first_only {
        exit(0);
    }

    Ok(())
}