use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use anyhow::{anyhow, bail, Context};
use tokio::process::Command;
use tracing::{debug, info, warn};
use crate::{QEMU_BINARY, QEMU_ENABLE_KVM};
use crate::models::nic::NicType;
use crate::models::nodes::node::Node;
use crate::utils::files::shared_dir::SHARED_DIR_PATH;
use crate::utils::qemu::image::create_linked_clone;
use crate::utils::qemu::network::tap_vhost;
use crate::utils::qemu::ports::allocate_tcp_port;
use crate::utils::qemu::process::{is_qemu_alive, read_pid_file, terminate_qemu, wait_for_exit, PID_FILE_NAME};
use crate::utils::qemu::qmp::qmp_execute;

const TARGET: &str = "vm";

/// QEMU supports at most 4 IDE disks (hda to hdd)
const MAX_DISKS: usize = 4;
const CONSOLE_HOST: &str = "127.0.0.1";

#[derive(Debug, Clone, PartialEq)]
pub enum DiskInterface {
    Ide,
    /// virtio-scsi controller + scsi-hd
    Scsi,
}

/// What an adapter is plugged into
#[derive(Debug, Clone)]
pub enum Connection {
    /// One end of a point-to-point UDP tunnel
    Udp { local_port: u16, remote_port: u16 },
    /// A tap wired to the tap of the other end, with in-kernel vhost-net for VirtIO NICs
    Tap { ifname: String, vhost: bool },
}

#[derive(Debug, Clone)]
pub struct Adapter {
    /// QEMU device model (e1000, rtl8139, virtio-net-pci)
    pub model: NicType,
    pub mac: String,
    pub connection: Option<Connection>,
}

#[derive(Debug, Clone)]
pub struct QemuVm {
    pub name: String,
    pub dir: PathBuf,
    pub ram: u32,
    pub vcpu: u32,
    pub uuid: String,
    /// Linked clones of the images, in hda..hdd order
    pub disks: Vec<PathBuf>,
    pub disk_interface: DiskInterface,
    pub adapters: Vec<Adapter>,
    /// Appended at the end of the command line
    pub extra_options: Vec<String>,
    pub console_host: String,
    pub console_port: u16,
    pub qmp_port: u16,
    pub pid: Option<u32>,
}

/// Turns an experiment or node name into a safe directory name
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect()
}

/// Commas are option separators for QEMU: they are escaped by doubling them
fn escape_qemu_value(value: &Path) -> String {
    value.to_string_lossy().replace(',', ",,")
}

fn hash_of(values: &[&str]) -> u64 {
    let mut hasher = DefaultHasher::new();
    values.hash(&mut hasher);
    hasher.finish()
}

pub fn netdev_id(adapter: u32) -> String {
    format!("net{adapter}")
}

impl QemuVm {
    async fn new(
        experiment_dir: &Path,
        node_index: usize,
        name: &str,
        node: &Node,
        images: &[PathBuf],
        disk_interface: DiskInterface,
        adapter_model: NicType,
        number_adapters: u32,
        extra_options: Vec<String>,
    ) -> anyhow::Result<QemuVm> {
        if images.len() > MAX_DISKS {
            bail!("{name}: QEMU supports only max {MAX_DISKS} images per VM");
        }

        let dir = experiment_dir.join(sanitize_name(name));
        fs::create_dir_all(&dir)?;

        let mut disks = Vec::with_capacity(images.len());

        for (index, image) in images.iter().enumerate() {
            let overlay = dir.join(format!("hd{}_disk.qcow2", (b'a' + index as u8) as char));
            create_linked_clone(image, &overlay).await?;
            disks.push(overlay);
        }

        // Deterministic and unique within an experiment: 0c:42:<node index>:00:<adapter>
        let adapters = (0..number_adapters)
            .map(|adapter| Adapter {
                model: adapter_model.clone(),
                mac: format!("0c:42:{:02x}:{:02x}:00:{:02x}", (node_index >> 8) & 0xff, node_index & 0xff, adapter & 0xff),
                connection: None,
            })
            .collect();

        let experiment_name = experiment_dir.file_name().unwrap_or_default().to_string_lossy();
        let high = hash_of(&[&experiment_name, name, "high"]);
        let low = hash_of(&[&experiment_name, name, "low"]);
        let uuid = format!(
            "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
            high >> 32,
            (high >> 16) & 0xffff,
            high & 0xfff,
            (low >> 48) & 0xfff,
            low & 0xffff_ffff_ffff
        );

        let vm = QemuVm {
            name: name.to_string(),
            dir,
            ram: node.ram,
            vcpu: node.vcpu,
            uuid,
            disks,
            disk_interface,
            adapters,
            extra_options,
            console_host: CONSOLE_HOST.to_string(),
            console_port: allocate_tcp_port()?,
            qmp_port: allocate_tcp_port()?,
            pid: None,
        };

        debug!(target: TARGET, "Generated VM: {} ({} vCPU, {} MB RAM, {} adapters)", name, vm.vcpu, vm.ram, vm.adapters.len());

        Ok(vm)
    }

    /// Client/server VM (flent, iperf3, ...): 1 VirtIO NIC, SCSI disk, shared folder through 9p
    pub async fn new_guest(experiment_dir: &Path, node_index: usize, name: &str, node: &Node, guest_image: &Path) -> anyhow::Result<QemuVm> {
        let extra_options = vec![
            String::from("-mem-prealloc"),
            String::from("-virtfs"),
            format!("local,path={},mount_tag=shared_folder,id=shared_folder,security_model=mapped-file", escape_qemu_value(&SHARED_DIR_PATH)),
        ];

        Self::new(
            experiment_dir,
            node_index,
            name,
            node,
            &[guest_image.to_path_buf()],
            DiskInterface::Scsi,
            NicType::VirtIO,
            1,
            extra_options
        ).await
    }

    /// Router VM: `number_nics` NICs of the router NIC type, IDE disks, host CPU model
    pub async fn new_router(experiment_dir: &Path, node_index: usize, name: &str, node: &Node, images: &[PathBuf]) -> anyhow::Result<QemuVm> {
        let router = node.unwrap_router();

        let nic_type = router.nics
            .values()
            .next()
            .ok_or_else(|| anyhow!("Router {name} has no NIC"))?
            .nic_type
            .clone();

        // "-cpu host" is only available with KVM
        let cpu_model = if *QEMU_ENABLE_KVM.get().unwrap() { "host" } else { "max" };

        let extra_options = vec![
            //String::from("-mem-prealloc"),
            String::from("-cpu"),
            String::from(cpu_model),
        ];

        Self::new(
            experiment_dir,
            node_index,
            name,
            node,
            images,
            DiskInterface::Ide,
            nic_type,
            router.number_nics,
            extra_options
        ).await
    }

    fn pid_file(&self) -> PathBuf {
        self.dir.join(PID_FILE_NAME)
    }

    /// Connects `adapter` to one end of a UDP tunnel. Must be called before `start`.
    pub fn attach_udp_tunnel(&mut self, adapter: u32, local_port: u16, remote_port: u16) -> anyhow::Result<()> {
        self.attach(adapter, Connection::Udp { local_port, remote_port })
    }

    /// Connects `adapter` to a tap. VirtIO NICs use vhost-net. Must be called before `start`.
    pub fn attach_tap(&mut self, adapter: u32, ifname: String) -> anyhow::Result<()> {
        let vhost = self.adapters
            .get(adapter as usize)
            .is_some_and(|vm_adapter| vm_adapter.model == NicType::VirtIO)
            && tap_vhost();

        self.attach(adapter, Connection::Tap { ifname, vhost })
    }

    fn attach(&mut self, adapter: u32, connection: Connection) -> anyhow::Result<()> {
        let nb_adapters = self.adapters.len();
        let vm_adapter = self.adapters
            .get_mut(adapter as usize)
            .ok_or_else(|| anyhow!("{} has no adapter {} (only {} adapters)", self.name, adapter, nb_adapters))?;

        if vm_adapter.connection.is_some() {
            bail!("Adapter {} of {} is already connected", adapter, self.name);
        }

        vm_adapter.connection = Some(connection);

        Ok(())
    }

    /// The QEMU command line
    /// The guest sees the same PCI slots and therefore the same interface names (e.g. ens4 for the guests).
    pub fn build_args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();

        let mut push = |values: &[&str]| args.extend(values.iter().map(|v| v.to_string()));

        push(&["-name", &format!("{},debug-threads=on", self.name)]);
        push(&["-m", &format!("{}M", self.ram)]);
        push(&["-smp", &format!("cpus={0},maxcpus={0},sockets=1", self.vcpu)]);

        if *QEMU_ENABLE_KVM.get().unwrap() {
            push(&["-enable-kvm", "-machine", "smm=off"]);
        }

        push(&["-boot", "order=c"]);

        for (index, disk) in self.disks.iter().enumerate() {
            let disk = escape_qemu_value(disk);

            match self.disk_interface {
                DiskInterface::Ide => {
                    push(&["-drive", &format!("file={disk},if=ide,index={index},media=disk,id=drive{index},format=qcow2")]);
                },
                DiskInterface::Scsi => {
                    push(&["-device", &format!("virtio-scsi-pci,id=scsi{index}")]);
                    push(&["-drive", &format!("file={disk},if=none,index={index},media=disk,id=drive{index},format=qcow2")]);
                    push(&["-device", &format!("scsi-hd,drive=drive{index}")]);
                }
            }
        }

        push(&["-uuid", &self.uuid]);
        // No graphical output, but the default VGA device is kept
        push(&["-display", "none"]);
        push(&["-serial", &format!("telnet:{}:{},server=on,wait=off", self.console_host, self.console_port)]);
        push(&["-qmp", &format!("tcp:127.0.0.1:{},server=on,wait=off", self.qmp_port)]);
        push(&["-nic", "none"]);

        for (index, adapter) in self.adapters.iter().enumerate() {
            let device = format!("{},mac={},id=nic{}", adapter.model.to_qemu_name(), adapter.mac, index);

            let netdev = netdev_id(index as u32);

            match &adapter.connection {
                Some(Connection::Udp { local_port, remote_port }) => {
                    push(&["-device", &format!("{device},netdev={netdev}")]);
                    push(&["-netdev", &format!("socket,id={netdev},udp=127.0.0.1:{remote_port},localaddr=127.0.0.1:{local_port}")]);
                },
                // vnet_hdr=off: no checksum/TSO/GSO offload is exposed to the guest, frames stay MTU-sized
                Some(Connection::Tap { ifname, vhost }) => {
                    let vhost = if *vhost { "on" } else { "off" };
                    let additional_flags = if adapter.model == NicType::VirtIO { ",csum=off,gso=off,guest_tso4=off,guest_tso6=off,host_tso4=off,host_tso6=off" } else { "" };

                    push(&["-device", &format!("{device},netdev={netdev}{additional_flags}")]);
                    push(&["-netdev", &format!("tap,id={netdev},ifname={ifname},script=no,downscript=no,vnet_hdr=off,vhost={vhost}")]);
                },
                // Not connected, like an unplugged cable
                None => push(&["-device", &device]),
            }
        }

        push(&["-pidfile", &self.pid_file().to_string_lossy(), "-daemonize"]);

        args.extend(self.extra_options.iter().cloned());

        args
    }

    pub async fn start(&mut self) -> anyhow::Result<()> {
        let qemu_binary = QEMU_BINARY.get().unwrap();
        let args = self.build_args();

        let command_line = std::iter::once(qemu_binary.as_str())
            .chain(args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<String>>()
            .join(" ");

        debug!(target: TARGET, "Starting {}: {}", self.name, command_line);
        // Kept for reproducibility, the VM can be started again by hand with this script
        fs::write(self.dir.join("qemu_command.sh"), format!("#!/bin/sh\n{command_line}\n"))?;

        // With -daemonize, the parent process only returns once the VM is initialized (or has failed)
        let output = Command::new(qemu_binary)
            .args(&args)
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("Could not run {qemu_binary}"))?;

        let mut log = output.stdout.clone();
        log.extend(&output.stderr);
        fs::write(self.dir.join("qemu.log"), &log)?;

        if !output.status.success() {
            bail!("QEMU failed to start {}: {}", self.name, String::from_utf8_lossy(&output.stderr).trim());
        }

        if !output.stderr.is_empty() {
            debug!(target: TARGET, "QEMU stderr for {}: {}", self.name, String::from_utf8_lossy(&output.stderr).trim());
        }

        let pid = read_pid_file(&self.pid_file()).ok_or_else(|| anyhow!("QEMU started {} but wrote no pidfile", self.name))?;
        self.pid = Some(pid);

        debug!(target: TARGET, "Started {} (pid {})", self.name, pid);

        Ok(())
    }

    /// Powers the VM off (QMP "quit"), kills it if it does not exit
    pub async fn stop(&mut self) -> anyhow::Result<()> {
        let Some(pid) = self.pid else {
            return Ok(());
        };

        let pid_file = self.pid_file();

        if is_qemu_alive(pid, &pid_file) {
            if let Err(error) = qmp_execute(self.qmp_port, "quit", None).await {
                warn!(target: TARGET, "QMP quit failed for {}: {}", self.name, error);
            }

            if !wait_for_exit(pid, &pid_file, Duration::from_secs(10)) {
                terminate_qemu(pid, &pid_file);
            }
        }

        self.pid = None;
        info!(target: TARGET, "Stopped {}", self.name);

        Ok(())
    }
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+".contains(c)) {
        value.to_string()
    }
    else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}
