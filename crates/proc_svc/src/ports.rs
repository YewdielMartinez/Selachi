//! Listening sockets and their owning pid (`GetExtendedTcpTable` / `GetExtendedUdpTable`).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
    MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID, MIB_UDP6TABLE_OWNER_PID,
    MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    UDP_TABLE_OWNER_PID,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

/// Transport protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Proto {
    /// TCP (IPv4 or IPv6, see [`PortInfo::local_addr`]).
    Tcp,
    /// UDP.
    Udp,
}

/// Socket state. TCP rows are always `Listen` (we only ask for listeners); UDP
/// has no connection state, so bound sockets are reported as `Bound`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortState {
    /// TCP listener.
    Listen,
    /// UDP socket bound to a local port.
    Bound,
}

/// A listening / bound local socket.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortInfo {
    /// Owning process.
    pub pid: u32,
    /// Protocol.
    pub proto: Proto,
    /// Local address (`0.0.0.0` / `::` = all interfaces).
    pub local_addr: IpAddr,
    /// Local port.
    pub port: u16,
    /// State.
    pub state: PortState,
}

impl PortInfo {
    /// True when reachable beyond loopback (bound to a wildcard or non-loopback address).
    pub fn is_exposed(&self) -> bool {
        !self.local_addr.is_loopback()
    }
}

/// All TCP listeners and UDP bound sockets (IPv4 + IPv6), sorted by pid then port.
/// Failures of one table are logged and skipped.
pub fn listening_ports() -> Vec<PortInfo> {
    let mut v = Vec::new();
    v.extend(tcp4());
    v.extend(tcp6());
    v.extend(udp4());
    v.extend(udp6());
    v.sort_by_key(|p| (p.pid, p.port));
    v.dedup();
    v
}

/// Groups ports by owning pid.
pub fn ports_by_pid(ports: &[PortInfo]) -> HashMap<u32, Vec<PortInfo>> {
    let mut m: HashMap<u32, Vec<PortInfo>> = HashMap::new();
    for p in ports {
        m.entry(p.pid).or_default().push(p.clone());
    }
    m
}

fn port_from_dw(dw: u32) -> u16 {
    // The port is in network byte order in the low 16 bits.
    u16::from_be((dw & 0xFFFF) as u16)
}

/// Calls a `Get*Table` style API with a growing buffer.
fn fetch(mut call: impl FnMut(*mut core::ffi::c_void, &mut u32) -> u32) -> Option<Vec<u32>> {
    let mut size = 16 * 1024u32;
    for _ in 0..8 {
        let mut buf = vec![0u32; (size as usize).div_ceil(4)];
        let mut sz = size;
        let r = call(buf.as_mut_ptr().cast(), &mut sz);
        if r == NO_ERROR.0 {
            return Some(buf);
        }
        if r == ERROR_INSUFFICIENT_BUFFER.0 {
            size = sz.max(size) + 4096;
            continue;
        }
        log::warn!("ip table query failed with error {r}");
        return None;
    }
    None
}

macro_rules! table_rows {
    ($buf:expr, $table:ty, $row:ty) => {{
        let base = $buf.as_ptr().cast::<$table>();
        // SAFETY: the API filled a table header followed by `dwNumEntries`
        // rows inside `$buf`; rows are read unaligned for safety.
        let (n, first) = unsafe {
            (
                std::ptr::addr_of!((*base).dwNumEntries).read_unaligned() as usize,
                std::ptr::addr_of!((*base).table).cast::<$row>(),
            )
        };
        let mut rows: Vec<$row> = Vec::with_capacity(n);
        for i in 0..n {
            // SAFETY: i < dwNumEntries, all inside the buffer.
            rows.push(unsafe { first.add(i).read_unaligned() });
        }
        rows
    }};
}

fn tcp4() -> Vec<PortInfo> {
    let buf = fetch(|p, sz| {
        // SAFETY: `p` points to a buffer of `*sz` bytes.
        unsafe {
            GetExtendedTcpTable(Some(p), sz, false, u32::from(AF_INET.0), TCP_TABLE_OWNER_PID_LISTENER, 0)
        }
    });
    let Some(buf) = buf else { return Vec::new() };
    let rows = table_rows!(buf, MIB_TCPTABLE_OWNER_PID, MIB_TCPROW_OWNER_PID);
    rows.into_iter()
        .map(|r| PortInfo {
            pid: r.dwOwningPid,
            proto: Proto::Tcp,
            local_addr: IpAddr::V4(Ipv4Addr::from(u32::from_be(r.dwLocalAddr))),
            port: port_from_dw(r.dwLocalPort),
            state: PortState::Listen,
        })
        .collect()
}

fn tcp6() -> Vec<PortInfo> {
    let buf = fetch(|p, sz| {
        // SAFETY: `p` points to a buffer of `*sz` bytes.
        unsafe {
            GetExtendedTcpTable(Some(p), sz, false, u32::from(AF_INET6.0), TCP_TABLE_OWNER_PID_LISTENER, 0)
        }
    });
    let Some(buf) = buf else { return Vec::new() };
    let rows = table_rows!(buf, MIB_TCP6TABLE_OWNER_PID, MIB_TCP6ROW_OWNER_PID);
    rows.into_iter()
        .map(|r| PortInfo {
            pid: r.dwOwningPid,
            proto: Proto::Tcp,
            local_addr: IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)),
            port: port_from_dw(r.dwLocalPort),
            state: PortState::Listen,
        })
        .collect()
}

fn udp4() -> Vec<PortInfo> {
    let buf = fetch(|p, sz| {
        // SAFETY: `p` points to a buffer of `*sz` bytes.
        unsafe { GetExtendedUdpTable(Some(p), sz, false, u32::from(AF_INET.0), UDP_TABLE_OWNER_PID, 0) }
    });
    let Some(buf) = buf else { return Vec::new() };
    let rows = table_rows!(buf, MIB_UDPTABLE_OWNER_PID, MIB_UDPROW_OWNER_PID);
    rows.into_iter()
        .map(|r| PortInfo {
            pid: r.dwOwningPid,
            proto: Proto::Udp,
            local_addr: IpAddr::V4(Ipv4Addr::from(u32::from_be(r.dwLocalAddr))),
            port: port_from_dw(r.dwLocalPort),
            state: PortState::Bound,
        })
        .collect()
}

fn udp6() -> Vec<PortInfo> {
    let buf = fetch(|p, sz| {
        // SAFETY: `p` points to a buffer of `*sz` bytes.
        unsafe { GetExtendedUdpTable(Some(p), sz, false, u32::from(AF_INET6.0), UDP_TABLE_OWNER_PID, 0) }
    });
    let Some(buf) = buf else { return Vec::new() };
    let rows = table_rows!(buf, MIB_UDP6TABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID);
    rows.into_iter()
        .map(|r| PortInfo {
            pid: r.dwOwningPid,
            proto: Proto::Udp,
            local_addr: IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)),
            port: port_from_dw(r.dwLocalPort),
            state: PortState::Bound,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_own_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().expect("addr").port();
        let me = std::process::id();
        let ports = listening_ports();
        assert!(
            ports.iter().any(|p| p.pid == me
                && p.port == port
                && p.proto == Proto::Tcp
                && p.state == PortState::Listen
                && p.local_addr == IpAddr::V4(Ipv4Addr::LOCALHOST)),
            "own listener on {port} not found"
        );
        assert_eq!(ports_by_pid(&ports)[&me].iter().filter(|p| p.port == port).count(), 1);
    }

    #[test]
    fn sees_own_ipv6_and_udp() {
        let l6 = std::net::TcpListener::bind("[::1]:0");
        let u = std::net::UdpSocket::bind("127.0.0.1:0").expect("udp");
        let uport = u.local_addr().expect("addr").port();
        let me = std::process::id();
        let ports = listening_ports();
        assert!(ports.iter().any(|p| p.pid == me && p.proto == Proto::Udp && p.port == uport));
        if let Ok(l6) = l6 {
            let p6 = l6.local_addr().expect("addr").port();
            assert!(ports.iter().any(|p| p.pid == me && p.port == p6 && p.local_addr.is_ipv6()));
        }
    }
}
