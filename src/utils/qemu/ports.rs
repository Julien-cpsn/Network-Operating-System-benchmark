use std::collections::HashSet;
use std::net::{TcpListener, UdpSocket};
use std::process;
use std::sync::Mutex;
use anyhow::bail;
use once_cell::sync::Lazy;

/// Outside the Linux ephemeral range (32768-60999), so that
/// the source port of a telnet client can never steal a port that a VM is about to bind.
const TCP_PORT_RANGE: (u16, u16) = (5000, 10000);
const UDP_PORT_RANGE: (u16, u16) = (10000, 20000);

/// A port is free when it can be bound, but it is only bound by QEMU later. Each benchmark process starts
/// at its own offset in the ranges (derived from its PID), so that several processes on the same host do not
/// probe the same ports at the same time and then all hand the same one to their VMs.
struct PortAllocator {
    /// Ports tried since the last reset
    tried_tcp: u32,
    tried_udp: u32,
    allocated: HashSet<u16>,
}

static PORT_ALLOCATOR: Lazy<Mutex<PortAllocator>> = Lazy::new(|| Mutex::new(PortAllocator {
    tried_tcp: 0,
    tried_udp: 0,
    allocated: HashSet::new(),
}));

fn range_len(range: (u16, u16)) -> u32 {
    (range.1 - range.0) as u32
}

/// Offset of this process in a range: spread over the range, the same for the whole life of the process
fn start_offset(range: (u16, u16)) -> u32 {
    ((process::id() as u64).wrapping_mul(2_654_435_761) % range_len(range) as u64) as u32
}

/// `tried`-th port of the range for this process, wrapping around
fn nth_port(range: (u16, u16), tried: u32) -> u16 {
    range.0 + ((start_offset(range) + tried) % range_len(range)) as u16
}

/// Forget every allocated port. Must only be called once every VM has been stopped.
pub fn reset_port_allocator() {
    let mut allocator = PORT_ALLOCATOR.lock().unwrap();
    allocator.tried_tcp = 0;
    allocator.tried_udp = 0;
    allocator.allocated.clear();
}

/// TCP port for a console (telnet) or QMP server, listening on 127.0.0.1
pub fn allocate_tcp_port() -> anyhow::Result<u16> {
    let mut allocator = PORT_ALLOCATOR.lock().unwrap();

    while allocator.tried_tcp < range_len(TCP_PORT_RANGE) {
        let port = nth_port(TCP_PORT_RANGE, allocator.tried_tcp);
        allocator.tried_tcp += 1;

        if !allocator.allocated.contains(&port) && TcpListener::bind(("127.0.0.1", port)).is_ok() {
            allocator.allocated.insert(port);
            return Ok(port);
        }
    }

    bail!("No free TCP port left in range {}-{}", TCP_PORT_RANGE.0, TCP_PORT_RANGE.1)
}

/// UDP port for one end of a link tunnel, bound on 127.0.0.1
pub fn allocate_udp_port() -> anyhow::Result<u16> {
    let mut allocator = PORT_ALLOCATOR.lock().unwrap();

    while allocator.tried_udp < range_len(UDP_PORT_RANGE) {
        let port = nth_port(UDP_PORT_RANGE, allocator.tried_udp);
        allocator.tried_udp += 1;

        if !allocator.allocated.contains(&port) && UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            allocator.allocated.insert(port);
            return Ok(port);
        }
    }

    bail!("No free UDP port left in range {}-{}", UDP_PORT_RANGE.0, UDP_PORT_RANGE.1)
}
