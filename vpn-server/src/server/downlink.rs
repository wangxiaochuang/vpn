use std::sync::Arc;

use super::conn::ConnectionHandle;
use crate::ledger::ConnectionLedger;
use bytes::Bytes;
use quic_link::PacketSink;
use shutdown::Shutdown;
use shutdown::ShutdownHandle;
use vpn_core::data::{DownlinkDispatcher, Tun, downlink_pump, dst_ipv4_addr};

pub struct RegistryDispatcher {
    pub ledger: Arc<ConnectionLedger<ConnectionHandle>>,
}

impl DownlinkDispatcher for RegistryDispatcher {
    fn dispatch(&self, pkt: Bytes) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let Some(dst) = dst_ipv4_addr(&pkt) else {
                return;
            };
            let Some(handle) = self.ledger.lookup_by_ip(dst) else {
                return;
            };
            let mut tx = handle.session.datagram_tx();
            let _ = tx.send(pkt).await;
        }
    }
}

pub struct DownlinkDaemon {
    tasks: tokio::task::JoinSet<()>,
}

impl DownlinkDaemon {
    pub fn spawn(
        mut tun: Tun,
        ledger: Arc<ConnectionLedger<ConnectionHandle>>,
        shutdown: ShutdownHandle,
    ) -> Self {
        let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        let dispatcher = RegistryDispatcher { ledger };
        tasks.spawn(async move {
            let _ = downlink_pump(&mut tun, &dispatcher, &shutdown).await;
        });
        Self { tasks }
    }

    pub async fn drain(&mut self, sd: &Shutdown) {
        sd.drain(&mut self.tasks, "daemon").await;
    }
}
