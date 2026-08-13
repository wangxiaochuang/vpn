#![allow(clippy::unwrap_used, clippy::expect_used, clippy::manual_async_fn)]

mod common;

use std::future::Future;
use std::io;
use std::net::Ipv4Addr;
use std::time::Duration;

use bytes::Bytes;
use quic_link::{PacketSink, PacketSource};
use tokio::sync::mpsc;
use vpn_server::data::downlink_pump;
use vpn_server::server::RegistryDispatcher;
use vpn_server::server::spawn_uplink_task;

fn sd_handle() -> shutdown::ShutdownHandle {
    shutdown::Shutdown::new(Duration::from_secs(5)).handle()
}

struct ChannelSink {
    tx: mpsc::Sender<Bytes>,
}

impl PacketSink for ChannelSink {
    fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send {
        async move {
            self.tx
                .send(pkt)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "sink closed"))
        }
    }
}

struct ChannelSource {
    rx: mpsc::Receiver<Bytes>,
}

impl PacketSource for ChannelSource {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send {
        async move {
            match self.rx.recv().await {
                Some(b) => Ok(b),
                None => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source closed",
                )),
            }
        }
    }
}

fn ipv4_packet(dst: [u8; 4]) -> Bytes {
    let mut pkt = vec![0u8; 20];
    pkt[0] = 0x45;
    pkt[16..20].copy_from_slice(&dst);
    Bytes::from(pkt)
}

#[tokio::test]
async fn test_spawn_uplink_task_accepts_mock_packet_sink_without_tun() {
    let pair = common::make_connected_pair().await;
    let (tx, mut rx) = mpsc::channel::<Bytes>(8);
    let sink = ChannelSink { tx };
    let session = quic_link::Session::new(pair.server.clone());

    let mut tasks: tokio::task::JoinSet<vpn_server::server::ConnExitCause> =
        tokio::task::JoinSet::new();
    spawn_uplink_task(&mut tasks, sink, session, &sd_handle());

    let pkt = ipv4_packet([10, 0, 0, 2]);
    pair.client
        .send_datagram(pkt.clone())
        .expect("send datagram");

    let received = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("timeout waiting for uplink packet")
        .expect("packet received");
    assert_eq!(received, pkt);
}

#[tokio::test]
async fn test_downlink_pump_with_mock_source_and_registry_dispatcher_without_tun() {
    let state = common::make_test_state().await;
    let pair = common::make_connected_pair().await;
    let handle = vpn_server::server::ConnectionHandle::new(
        quic_link::Session::new(pair.server.clone()),
        Ipv4Addr::new(10, 0, 0, 2),
    );
    state
        .ledger
        .register("alice", Ipv4Addr::new(10, 0, 0, 2), handle)
        .unwrap();

    let pkt = ipv4_packet([10, 0, 0, 2]);
    let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
    src_tx.send(pkt.clone()).await.unwrap();
    drop(src_tx);

    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    let mut tun = ChannelSource { rx: src_rx };
    let _ = downlink_pump(&mut tun, &dispatcher, &sd_handle()).await;

    let received = pair.client.read_datagram().await.expect("should receive");
    assert_eq!(received, pkt);
}

#[tokio::test]
async fn test_uplink_via_spawn_task_exits_on_connection_close() {
    let pair = common::make_connected_pair().await;
    let (tx, _rx) = mpsc::channel::<Bytes>(8);
    let sink = ChannelSink { tx };
    let session = quic_link::Session::new(pair.server.clone());

    let mut tasks: tokio::task::JoinSet<vpn_server::server::ConnExitCause> =
        tokio::task::JoinSet::new();
    spawn_uplink_task(&mut tasks, sink, session, &sd_handle());

    pair.client.close(0u32.into(), b"bye");
    let cause = tokio::time::timeout(Duration::from_secs(3), tasks.join_next())
        .await
        .expect("timeout waiting for uplink task exit")
        .expect("task did not panic");
    assert!(matches!(
        cause,
        Ok(vpn_server::server::ConnExitCause::UplinkEnded)
    ));
}
