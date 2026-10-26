use std::collections::HashSet;
use std::net::{TcpListener, UdpSocket};
use std::sync::Mutex;
use anyhow::bail;
use once_cell::sync::Lazy;

/// Outside the Linux ephemeral range (32768-60999), so that
/// the source port of a telnet client can never steal a port that a VM is about to bind.
const TCP_PORT_RANGE: (u16, u16) = (5000, 10000);
const UDP_PORT_RANGE: (u16, u16) = (10000, 20000);

struct PortAllocator {
    next_tcp: u16,
    next_udp: u16,
    allocated: HashSet<u16>,
}

static PORT_ALLOCATOR: Lazy<Mutex<PortAllocator>> = Lazy::new(|| Mutex::new(PortAllocator {
    next_tcp: TCP_PORT_RANGE.0,
    next_udp: UDP_PORT_RANGE.0,
    allocated: HashSet::new(),
}));

/// Forget every allocated port. Must only be called once every VM has been stopped.
pub fn reset_port_allocator() {
    let mut allocator = PORT_ALLOCATOR.lock().unwrap();
    allocator.next_tcp = TCP_PORT_RANGE.0;
    allocator.next_udp = UDP_PORT_RANGE.0;
    allocator.allocated.clear();
}

/// TCP port for a console (telnet) or QMP server, listening on 127.0.0.1
pub fn allocate_tcp_port() -> anyhow::Result<u16> {
    let mut allocator = PORT_ALLOCATOR.lock().unwrap();

    while allocator.next_tcp < TCP_PORT_RANGE.1 {
        let port = allocator.next_tcp;
        allocator.next_tcp += 1;

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

    while allocator.next_udp < UDP_PORT_RANGE.1 {
        let port = allocator.next_udp;
        allocator.next_udp += 1;

        if !allocator.allocated.contains(&port) && UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            allocator.allocated.insert(port);
            return Ok(port);
        }
    }

    bail!("No free UDP port left in range {}-{}", UDP_PORT_RANGE.0, UDP_PORT_RANGE.1)
}
