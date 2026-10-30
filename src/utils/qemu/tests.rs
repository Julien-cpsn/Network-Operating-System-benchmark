//! Integration test of the QEMU backend. Needs qemu-system-x86_64 and qemu-img, so it is ignored by default:
//! `cargo test qemu -- --ignored --nocapture`

use std::fs;
use std::fs::OpenOptions;
use std::path::Path;
use std::process::Command;
use indexmap::IndexMap;
use serde_json::{json, Value};
use crate::{QEMU_BINARY, QEMU_ENABLE_KVM, QEMU_IMG_BINARY};
use crate::models::link::Link;
use crate::models::nic::{Nic, NicIndex, NicType};
use crate::models::nodes::guest::Guest;
use crate::models::nodes::node::{Node, NodeType};
use crate::models::nodes::router::Router;
use crate::models::routes::route_config::RouteConfig;
use crate::utils::files::shared_dir::SHARED_DIR_PATH;
use crate::utils::link::{create_link, link_failure};
use crate::utils::qemu::network::{link_backend, set_link_backend, LinkBackend};
use crate::utils::qemu::process::{is_qemu_alive, stop_and_delete_all_vms, PID_FILE_NAME};
use crate::utils::qemu::qmp::qmp_execute;
use crate::utils::qemu::vm::QemuVm;

fn node(node_type: NodeType) -> Node {
    Node { vcpu: 1, ram: 256, x: 0, y: 0, node_type, vm: None, distant_networks: vec![] }
}

fn blank_image(path: &Path) {
    let status = Command::new("qemu-img").args(["create", "-q", "-f", "qcow2"]).arg(path).arg("64M").status().unwrap();
    assert!(status.success());
}

/// (slot, vendor id, device id) of every device on PCI bus 0
async fn pci_devices(qmp_port: u16) -> Vec<(u64, u64, u64)> {
    let result = qmp_execute(qmp_port, "query-pci", None).await.unwrap();
    result[0]["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["slot"].as_u64().unwrap(), d["id"]["vendor"].as_u64().unwrap(), d["id"]["device"].as_u64().unwrap()))
        .collect()
}

async fn hmp(qmp_port: u16, command: &str) -> String {
    let result: Value = qmp_execute(qmp_port, "human-monitor-command", Some(json!({ "command-line": command }))).await.unwrap();
    result.as_str().unwrap().to_string()
}

/// The link backend and the port allocator are global: scenarios must not run concurrently
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn qemu_backend_udp_links() {
    let _guard = SERIAL.lock().await;
    set_link_backend(LinkBackend::Udp, true);
    scenario().await;
}

/// Needs root or passwordless sudo. vhost-net is used only if /dev/vhost-net and KVM are usable.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn qemu_backend_tap_links() {
    let _guard = SERIAL.lock().await;
    let vhost = Path::new("/dev/vhost-net").exists() && Path::new("/dev/kvm").exists();
    set_link_backend(LinkBackend::Tap, vhost);
    scenario().await;
    set_link_backend(LinkBackend::Udp, true);
}

async fn scenario() {
    QEMU_BINARY.get_or_init(|| String::from("qemu-system-x86_64"));
    QEMU_IMG_BINARY.get_or_init(|| String::from("qemu-img"));
    QEMU_ENABLE_KVM.get_or_init(|| OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok());

    fs::create_dir_all(&*SHARED_DIR_PATH).unwrap();

    let root = tempfile::tempdir().unwrap();
    let work_dir = root.path().join("vms");
    stop_and_delete_all_vms(&work_dir).unwrap();

    let guest_image = root.path().join("guest.qcow2");
    let router_image = root.path().join("router.qcow2");
    blank_image(&guest_image);
    blank_image(&router_image);

    let mut nics = IndexMap::new();
    nics.insert(NicIndex(String::from("1")), Nic { nic_type: NicType::E1000, ip_address: "10.0.1.1/24".parse().unwrap() });

    let mut nodes = IndexMap::new();
    // 2 vCPUs: the client's VirtIO NIC gets 2 queue pairs (QEMU_VIRTIO_QUEUES=auto)
    let mut client_node = node(NodeType::Guest(Guest { ip: "10.0.1.2/24".parse().unwrap() }));
    client_node.vcpu = 2;
    nodes.insert(String::from("Client"), client_node);
    nodes.insert(String::from("Router"), node(NodeType::Router(Router {
        id: String::from("1"),
        os_name: String::from("Test"),
        number_nics: 3,
        nics,
        routes_config: RouteConfig::Mpls,
    })));

    let experiment_dir = work_dir.join("Simple_Test_xp");

    let client = QemuVm::new_guest(&experiment_dir, 0, "Client", &nodes["Client"], &guest_image).await.unwrap();
    let router = QemuVm::new_router(&experiment_dir, 1, "Router", &nodes["Router"], &[router_image.clone()]).await.unwrap();
    nodes["Client"].vm = Some(client);
    nodes["Router"].vm = Some(router);

    let link = Link { node_a: String::from("Client"), adapter_a: 0, node_b: String::from("Router"), adapter_b: 1 };
    let qemu_link = create_link(&mut nodes, &link, 0).unwrap();

    for vm in nodes.values_mut().map(|n| n.vm.as_mut().unwrap()) {
        vm.start().await.unwrap();
        println!("{}", fs::read_to_string(vm.dir.join("qemu_command.sh")).unwrap());
    }

    // Guest: virtio-scsi in slot 3, virtio-net in slot 4 (=> ens4), virtio-9p in slot 5
    let client_pci = pci_devices(nodes["Client"].vm.as_ref().unwrap().qmp_port).await;
    println!("Client PCI: {client_pci:?}");
    assert!(client_pci.contains(&(3, 0x1af4, 0x1004)));
    assert!(client_pci.contains(&(4, 0x1af4, 0x1000)));
    assert!(client_pci.contains(&(5, 0x1af4, 0x1009)));

    // Router: IDE disk on the PIIX, 3 e1000 NICs in slots 3, 4, 5
    let router_pci = pci_devices(nodes["Router"].vm.as_ref().unwrap().qmp_port).await;
    println!("Router PCI: {router_pci:?}");
    for slot in 3..6 {
        assert!(router_pci.contains(&(slot, 0x8086, 0x100e)));
    }

    // Overlay disk backed by the untouched image
    let block = hmp(nodes["Router"].vm.as_ref().unwrap().qmp_port, "info block").await;
    println!("{block}");
    assert!(block.contains("hda_disk.qcow2"));
    assert!(block.contains("router.qcow2"));

    let network = hmp(nodes["Router"].vm.as_ref().unwrap().qmp_port, "info network").await;
    println!("{network}");
    assert!(network.contains("net1"));

    if link_backend() == LinkBackend::Tap {
        // Both taps exist, are attached to QEMU, and are wired to each other
        for tap in ["nosb0000a", "nosb0000b"] {
            assert!(Path::new(&format!("/sys/class/net/{tap}")).exists());
        }
        assert!(network.contains("ifname=nosb0000b"));

        // Multiqueue: the client's tap is multi_queue and opened twice, the router's e1000 tap is not
        let tap_flags = |tap: &str| String::from_utf8_lossy(&Command::new("ip").args(["-d", "link", "show", tap]).output().unwrap().stdout).to_string();
        assert!(tap_flags("nosb0000a").contains("multi_queue"), "{}", tap_flags("nosb0000a"));
        assert!(!tap_flags("nosb0000b").contains("multi_queue"), "{}", tap_flags("nosb0000b"));
        let client_network = hmp(nodes["Client"].vm.as_ref().unwrap().qmp_port, "info network").await;
        println!("{client_network}");
        assert_eq!(client_network.matches("ifname=nosb0000a").count(), 2);
        let filters = Command::new("tc").args(["filter", "show", "dev", "nosb0000a", "ingress"]).output().unwrap();
        assert!(String::from_utf8_lossy(&filters.stdout).contains("nosb0000b"));
    }

    // set_link resolves the netdev ids (an unknown id is an error), then cuts both ends
    let unknown = qmp_execute(nodes["Router"].vm.as_ref().unwrap().qmp_port, "set_link", Some(json!({ "name": "net9", "up": false }))).await;
    assert!(unknown.is_err());
    link_failure(link, qemu_link, 0).await.unwrap();

    for vm in nodes.values_mut().map(|n| n.vm.as_mut().unwrap()) {
        let pid = vm.pid.unwrap();
        vm.stop().await.unwrap();
        // The process may linger as a zombie if nothing reaps it, but it must no longer run QEMU
        assert!(!is_qemu_alive(pid, &vm.dir.join(PID_FILE_NAME)));
    }

    stop_and_delete_all_vms(&work_dir).unwrap();
    assert!(!Path::new("/sys/class/net/nosb0000a").exists());
}
