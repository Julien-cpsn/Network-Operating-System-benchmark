use std::{env, fs};
use std::path::PathBuf;
use std::process::exit;
use indexmap::IndexMap;
use tracing::{debug, error};
use once_cell::sync::Lazy;
use crate::models::operating_system::OperatingSystem;
use crate::utils::files::dir::subtract_project_root;
use crate::utils::files::network_stacks::NETWORK_STACK_LIST_PATH;
use crate::utils::files::routing_stacks::ROUTING_STACK_LIST_PATH;

const TARGET: &str = "files";

pub static OS_LIST_PATH: Lazy<PathBuf> = Lazy::new(|| env::current_dir().unwrap().join("experimentation").join("operating_systems.toml"));


pub fn parse_os_list_file(network_stack_list: Vec<&String>, routing_stack_list: Vec<&String>) -> anyhow::Result<IndexMap<String, OperatingSystem>> {
    if !OS_LIST_PATH.exists() {
        error!(target: TARGET, "Could not find file \"{}\"", subtract_project_root(&OS_LIST_PATH));
        exit(1);
    }

    debug!(target: TARGET, "Parsing operating system list");
    
    let os_list_content = fs::read_to_string(&*OS_LIST_PATH)?;
    let os_list: IndexMap<String, OperatingSystem> = toml::from_str(&os_list_content)?;

    if os_list.is_empty() {
        debug!(target: TARGET, "Operating system list is empty");
    }
    else {
        debug!(target: TARGET, "Found operating system list:");

        for (key, os) in &os_list {
            debug!(target: TARGET, "- {}", key);

            if !network_stack_list.contains(&&os.network_stack) {
                error!(target: TARGET, "{} network stack \"{}\" does not exist in \"{}\"", key, os.network_stack, subtract_project_root(&NETWORK_STACK_LIST_PATH));
                exit(1);
            }

            if let Some(routing_stack) = &os.routing_stack {
                if !routing_stack_list.contains(&routing_stack) {
                    error!(target: TARGET, "{} routing stack \"{}\" does not exist in \"{}\"", key, routing_stack, subtract_project_root(&ROUTING_STACK_LIST_PATH));
                    exit(1);
                }
            }
        }
    }

    Ok(os_list)
}