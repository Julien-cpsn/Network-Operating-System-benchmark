use std::{env, fs};
use std::path::{PathBuf};
use std::process::exit;
use indexmap::IndexMap;
use tracing::{debug, error};
use once_cell::sync::Lazy;
use crate::models::routing_stack::RoutingStack;
use crate::utils::files::dir::subtract_project_root;

const TARGET: &str = "files";

pub static ROUTING_STACK_LIST_PATH: Lazy<PathBuf> = Lazy::new(|| env::current_dir().unwrap().join("experimentation").join("routing_stacks.toml"));


pub fn parse_routing_stack_list_file() -> anyhow::Result<IndexMap<String, RoutingStack>> {
    if !ROUTING_STACK_LIST_PATH.exists() {
        error!(target: TARGET, "Could not find file \"{}\"", subtract_project_root(&ROUTING_STACK_LIST_PATH));
        exit(1);
    }

    debug!(target: TARGET, "Parsing routing stack list");

    let routing_stack_list_content = fs::read_to_string(&*ROUTING_STACK_LIST_PATH)?;
    let routing_stacks: IndexMap<String, RoutingStack> = toml::from_str(&routing_stack_list_content)?;

    if routing_stacks.is_empty() {
        debug!(target: TARGET, "Routing stack list is empty");
    }
    else {
        debug!(target: TARGET, "Found routing stack list:");

        for (key, routing_stack) in &routing_stacks {
            let mut supported_protocols = Vec::new();

            if routing_stack.rip.is_some() {
                supported_protocols.push("RIP");
            }

            if routing_stack.ospf.is_some() {
                supported_protocols.push("OSPF");
            }

            debug!(target: TARGET, "- {} ({})", key, supported_protocols.join(", "));
        }
    }

    Ok(routing_stacks)
}