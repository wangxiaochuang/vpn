use std::time::Duration;

use async_trait::async_trait;
use sysinfo::InterfaceOperationalState;
use sysinfo::Networks;

use crate::collector::CollectError;
use crate::collector::Collector;
use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;
use crate::proto::NetifList;
use crate::proto::info_snapshot::Payload;

const NETIF_CADENCE: Duration = Duration::from_mins(10);

pub struct NetifCollector;

impl Default for NetifCollector {
    fn default() -> Self {
        Self
    }
}

impl NetifCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for NetifCollector {
    fn kind(&self) -> InfoKind {
        InfoKind::NetifList
    }

    fn cadence(&self) -> Option<Duration> {
        Some(NETIF_CADENCE)
    }

    async fn collect(&self) -> Result<InfoSnapshot, CollectError> {
        let list = tokio::task::spawn_blocking(collect_netif_blocking)
            .await
            .map_err(|e| CollectError::System(format!("join error: {e}")))?;
        Ok(InfoSnapshot {
            kind: InfoKind::NetifList as i32,
            payload: Some(Payload::Interfaces(list)),
        })
    }
}

fn collect_netif_blocking() -> NetifList {
    let nets = Networks::new_with_refreshed_list();
    let interfaces = nets
        .list()
        .iter()
        .map(|(name, data)| crate::proto::NetifEntry {
            name: name.clone(),
            mac: data.mac_address().to_string(),
            ipv4_addrs: ipv4_addrs(data.ip_networks()),
            ipv6_addrs: ipv6_addrs(data.ip_networks()),
            is_up: matches!(data.operational_state(), InterfaceOperationalState::Up),
            mtu: u32::try_from(data.mtu()).unwrap_or(0),
        })
        .collect();
    NetifList { interfaces }
}

fn ipv4_addrs(networks: &[sysinfo::IpNetwork]) -> Vec<String> {
    networks
        .iter()
        .filter(|n| n.addr.is_ipv4())
        .map(|n| n.addr.to_string())
        .collect()
}

fn ipv6_addrs(networks: &[sysinfo::IpNetwork]) -> Vec<String> {
    networks
        .iter()
        .filter(|n| n.addr.is_ipv6())
        .map(|n| n.addr.to_string())
        .collect()
}
