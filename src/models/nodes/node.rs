use crate::models::nodes::guest::{GenericGuest, Guest};
use crate::models::nodes::router::{GenericRouter, Router};
use cidr::Ipv4Cidr;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use crate::utils::qemu::vm::QemuVm;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub vcpu: u32,
    pub ram: u32,

    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,

    #[serde(flatten)]
    pub node_type: NodeType,

    #[serde(skip)]
    pub vm: Option<QemuVm>,

    #[serde(skip)]
    pub distant_networks: Vec<DistantNetwork>
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeType {
    Guest(Guest),
    Router(Router),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistantNetwork {
    pub network: Ipv4Cidr,
    pub gateway: Ipv4Addr,
    pub adapter_index: u32,
}

impl Node {
    pub fn unwrap_guest(&self) -> &Guest {
        match &self.node_type {
            NodeType::Guest(guest) => guest,
            NodeType::Router(_) => unreachable!()
        }
    }

    pub fn unwrap_router(&self) -> &Router {
        match &self.node_type {
            NodeType::Guest(_) => unreachable!(),
            NodeType::Router(router) => router
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericNode {
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,

    #[serde(flatten)]
    pub node_type: GenericNodeType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GenericNodeType {
    Guest(GenericGuest),
    Router(GenericRouter),
}

impl GenericNodeType {
    /*
    pub fn get_guest(&self) -> Option<&GenericGuest> {
        match &self {
            GenericNodeType::Guest(guest) => Some(guest),
            GenericNodeType::Router(_) => None
        }
    }*/

    pub fn get_router(&self) -> Option<&GenericRouter> {
        match &self {
            GenericNodeType::Guest(_) => None,
            GenericNodeType::Router(router) => Some(router)
        }
    }
}