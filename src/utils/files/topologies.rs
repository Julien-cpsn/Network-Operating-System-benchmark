use std::{env, fs};
use std::path::PathBuf;
use std::process::exit;
use indexmap::IndexMap;
use tracing::{debug, error};
use once_cell::sync::Lazy;
use crate::models::topology::Topology;
use crate::utils::files::dir::subtract_project_root;

const TARGET: &str = "files";

pub static TOPOLOGIES_LIST_PATH: Lazy<PathBuf> = Lazy::new(|| env::current_dir().unwrap().join("experimentation").join("topologies.toml"));


pub fn parse_topologies_list_file() -> anyhow::Result<IndexMap<String, Topology>> {
    if !TOPOLOGIES_LIST_PATH.exists() {
        error!(target: TARGET, "Could not find file \"{}\"", subtract_project_root(&TOPOLOGIES_LIST_PATH));
        exit(1);
    }

    let topology_list_content = fs::read_to_string(&*TOPOLOGIES_LIST_PATH)?;
    let topology_list: IndexMap<String, Topology> = toml::from_str(&topology_list_content)?;

    if topology_list.is_empty() {
        debug!(target: TARGET, "Topology list is empty");
    }
    else {
        debug!(target: TARGET, "Found topology list:");

        for (key, topology) in &topology_list {
            let supported_protocol_list = topology.supported_routing_protocols
                .iter()
                .map(|protocol| protocol.to_string())
                .collect::<Vec<String>>();

            if supported_protocol_list.is_empty() {
                error!(target: TARGET, "No routing protocol supported by topology \"{}\"", key);
                exit(1);
            }

            debug!(target: TARGET, "- {} ({})", key, supported_protocol_list.join(", "));
        }
    }

    Ok(topology_list)
}