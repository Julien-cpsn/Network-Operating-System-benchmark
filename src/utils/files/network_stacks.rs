use std::{env, fs};
use std::path::{PathBuf};
use std::process::exit;
use indexmap::IndexMap;
use tracing::{debug, error};
use once_cell::sync::Lazy;
use crate::models::network_stack::NetworkStack;
use crate::utils::files::dir::subtract_project_root;

const TARGET: &str = "files";

pub static NETWORK_STACK_LIST_PATH: Lazy<PathBuf> = Lazy::new(|| env::current_dir().unwrap().join("experimentation").join("network_stacks.toml"));


pub fn parse_network_stack_list_file() -> anyhow::Result<IndexMap<String, NetworkStack>> {
    if !NETWORK_STACK_LIST_PATH.exists() {
        error!(target: TARGET, "Could not find file \"{}\"", subtract_project_root(&NETWORK_STACK_LIST_PATH));
        exit(1);
    }

    debug!(target: TARGET, "Parsing network stack list");

    let network_stack_list_content = fs::read_to_string(&*NETWORK_STACK_LIST_PATH)?;
    let network_stacks: IndexMap<String, NetworkStack> = toml::from_str(&network_stack_list_content)?;

    if network_stacks.is_empty() {
        debug!(target: TARGET, "Network stack list is empty");
    }
    else {
        debug!(target: TARGET, "Found network stack list:");

        for key in network_stacks.keys() {
            debug!(target: TARGET, "- {}", key);
        }
    }

    Ok(network_stacks)
}