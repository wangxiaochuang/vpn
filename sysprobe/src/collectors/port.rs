#[cfg(target_os = "linux")]
use std::fs;
use std::time::Duration;

use async_trait::async_trait;

use crate::collector::CollectError;
use crate::collector::Collector;
use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;
use crate::proto::PortList;
use crate::proto::info_snapshot::Payload;

const PORT_CADENCE: Duration = Duration::from_mins(1);

pub struct PortCollector;

impl Default for PortCollector {
    fn default() -> Self {
        Self
    }
}

impl PortCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for PortCollector {
    fn kind(&self) -> InfoKind {
        InfoKind::PortList
    }

    fn cadence(&self) -> Option<Duration> {
        Some(PORT_CADENCE)
    }

    async fn collect(&self) -> Result<InfoSnapshot, CollectError> {
        let ports = tokio::task::spawn_blocking(collect_ports_blocking)
            .await
            .map_err(|e| CollectError::System(format!("join error: {e}")))?;
        Ok(InfoSnapshot {
            kind: InfoKind::PortList as i32,
            payload: Some(Payload::Ports(ports)),
        })
    }
}

#[cfg(target_os = "linux")]
fn collect_ports_blocking() -> PortList {
    let mut ports = Vec::new();
    ports.extend(parse_proc_net_tcp("/proc/net/tcp", "tcp"));
    ports.extend(parse_proc_net_tcp("/proc/net/tcp6", "tcp"));
    ports.extend(parse_proc_net_udp("/proc/net/udp", "udp"));
    ports.extend(parse_proc_net_udp("/proc/net/udp6", "udp"));
    PortList { ports }
}

#[cfg(not(target_os = "linux"))]
fn collect_ports_blocking() -> PortList {
    PortList { ports: vec![] }
}

#[cfg(target_os = "linux")]
fn parse_proc_net_tcp(path: &str, proto: &str) -> Vec<crate::proto::PortEntry> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .skip(1)
        .filter_map(|l| parse_proc_line(l, proto))
        .collect()
}

#[cfg(target_os = "linux")]
fn parse_proc_net_udp(path: &str, proto: &str) -> Vec<crate::proto::PortEntry> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .skip(1)
        .filter_map(|l| parse_proc_line(l, proto))
        .collect()
}

#[cfg(target_os = "linux")]
fn parse_proc_line(line: &str, proto: &str) -> Option<crate::proto::PortEntry> {
    use crate::proto::PortEntry;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 2 {
        return None;
    }
    let local = cols[1];
    let (addr, port) = parse_addr_port(local)?;
    let state = tcp_state_name(cols.get(3).copied().unwrap_or("00"));
    Some(PortEntry {
        proto: proto.to_string(),
        local_addr: addr,
        local_port: port,
        state,
        pid: 0,
    })
}

#[cfg(target_os = "linux")]
fn parse_addr_port(s: &str) -> Option<(String, u32)> {
    let (addr_hex, port_hex) = s.split_once(':')?;
    let port = u32::from_str_radix(port_hex, 16).ok()?;
    let addr = if addr_hex.len() == 8 {
        format_ipv4(addr_hex)
    } else {
        format!("::{}", addr_hex)
    };
    Some((addr, port))
}

#[cfg(target_os = "linux")]
fn format_ipv4(hex: &str) -> String {
    let bytes: Vec<Option<u32>> = (0..hex.len())
        .step_by(2)
        .map(|i| u32::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    match bytes.as_slice() {
        [Some(a), Some(b), Some(c), Some(d)] => format!("{d}.{c}.{b}.{a}"),
        _ => hex.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn tcp_state_name(code: &str) -> String {
    match code {
        "01" => "ESTABLISHED".into(),
        "0A" => "LISTEN".into(),
        "06" => "TIME_WAIT".into(),
        _ => code.to_lowercase(),
    }
}
