use std::time::Duration;

use async_trait::async_trait;

use crate::collector::CollectError;
use crate::collector::Collector;
use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;
use crate::proto::PortEntry;
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
    for (path, proto) in [
        ("/proc/net/tcp", "tcp"),
        ("/proc/net/tcp6", "tcp"),
        ("/proc/net/udp", "udp"),
        ("/proc/net/udp6", "udp"),
    ] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        ports.extend(parse_proc_net(&content, proto));
    }
    PortList { ports }
}

#[cfg(not(target_os = "linux"))]
fn collect_ports_blocking() -> PortList {
    PortList { ports: vec![] }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_net(content: &str, proto: &str) -> Vec<PortEntry> {
    content
        .lines()
        .skip(1)
        .filter_map(|l| parse_proc_line(l, proto))
        .collect()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_line(line: &str, proto: &str) -> Option<PortEntry> {
    let cols: Vec<&str> = line.split_whitespace().collect();
    let local = cols.get(1)?;
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_addr_port(s: &str) -> Option<(String, u32)> {
    let (addr_hex, port_hex) = s.split_once(':')?;
    let port = u32::from_str_radix(port_hex, 16).ok()?;
    let addr = if addr_hex.len() == 8 {
        format_ipv4(addr_hex)
    } else {
        format!("::{addr_hex}")
    };
    Some((addr, port))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn tcp_state_name(code: &str) -> String {
    match code {
        "01" => "ESTABLISHED".into(),
        "0A" => "LISTEN".into(),
        "06" => "TIME_WAIT".into(),
        _ => code.to_lowercase(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_format_ipv4_normal_returns_dotted_decimal() {
        assert_eq!(format_ipv4("0100007F"), "127.0.0.1");
    }

    #[test]
    fn test_format_ipv4_empty_returns_empty() {
        assert_eq!(format_ipv4(""), "");
    }

    #[test]
    fn test_format_ipv4_invalid_hex_returns_original() {
        assert_eq!(format_ipv4("GG"), "GG");
    }

    #[test]
    fn test_parse_addr_port_normal_returns_addr_port() {
        assert_eq!(
            parse_addr_port("0100007F:0050"),
            Some(("127.0.0.1".to_string(), 80))
        );
    }

    #[test]
    fn test_parse_addr_port_no_colon_returns_none() {
        assert_eq!(parse_addr_port("0100007F0050"), None);
    }

    #[test]
    fn test_parse_addr_port_invalid_port_hex_returns_none() {
        assert_eq!(parse_addr_port("0100007F:ZZ"), None);
    }

    #[test]
    fn test_tcp_state_name_listen_returns_listen() {
        assert_eq!(tcp_state_name("0A"), "LISTEN");
    }

    #[test]
    fn test_tcp_state_name_established_returns_established() {
        assert_eq!(tcp_state_name("01"), "ESTABLISHED");
    }

    #[test]
    fn test_tcp_state_name_unknown_returns_lowercase() {
        assert_eq!(tcp_state_name("ZZ"), "zz");
    }

    #[test]
    fn test_parse_proc_line_listen_returns_entry() {
        let line = "  0: 0100007F:1F90 00000000:0000 0A 00000000:00000000";
        let entry = parse_proc_line(line, "tcp").expect("entry");
        assert_eq!(entry.proto, "tcp");
        assert_eq!(entry.local_addr, "127.0.0.1");
        assert_eq!(entry.local_port, 8080);
        assert_eq!(entry.state, "LISTEN");
    }

    #[test]
    fn test_parse_proc_line_empty_returns_none() {
        assert_eq!(parse_proc_line("", "tcp"), None);
    }

    #[test]
    fn test_parse_proc_line_single_column_returns_none() {
        assert_eq!(parse_proc_line("foo", "tcp"), None);
    }

    #[test]
    fn test_parse_proc_net_listen_row_returns_one_entry() {
        let content = "  sl  local_address rem_address\n  0: 0100007F:1F90 00000000:0000 0A\n";
        let entries = parse_proc_net(content, "tcp");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].proto, "tcp");
        assert_eq!(entries[0].local_addr, "127.0.0.1");
        assert_eq!(entries[0].local_port, 8080);
        assert_eq!(entries[0].state, "LISTEN");
    }

    #[test]
    fn test_parse_proc_net_empty_returns_empty_vec() {
        assert!(parse_proc_net("", "tcp").is_empty());
    }

    #[test]
    fn test_parse_proc_net_header_only_returns_empty_vec() {
        let content = "  sl  local_address rem_address\n";
        assert!(parse_proc_net(content, "tcp").is_empty());
    }
}
