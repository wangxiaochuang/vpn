#![allow(clippy::unwrap_used, clippy::expect_used, clippy::manual_async_fn)]

mod common;

use std::future::Future;
use std::io;
use std::net::Ipv4Addr;

use bytes::Bytes;
use tokio::sync::mpsc;
use vpn_server::data::{DownlinkDispatcher, PacketSource, downlink_pump};
use vpn_server::server::RegistryDispatcher;

fn sd_handle() -> shutdown::ShutdownHandle {
    shutdown::Shutdown::new(std::time::Duration::from_secs(5)).handle()
}

struct ChannelSource {
    rx: mpsc::Receiver<Bytes>,
}

impl PacketSource for ChannelSource {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send {
        async move {
            match self.rx.recv().await {
                Some(b) => Ok(b),
                None => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "tun closed")),
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

async fn assert_no_datagram(conn: &quinn::Connection) {
    let r = tokio::time::timeout(std::time::Duration::from_millis(100), conn.read_datagram()).await;
    assert!(r.is_err(), "should timeout - no datagram expected");
}

#[tokio::test]
async fn test_dispatcher_delivers_packet_to_registered_conn() {
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

    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    let pkt = ipv4_packet([10, 0, 0, 2]);
    dispatcher.dispatch(pkt.clone()).await;

    let received = pair.client.read_datagram().await.expect("should receive");
    assert_eq!(received, pkt);
}

#[tokio::test]
async fn test_dispatcher_miss_silently_drops_no_datagram() {
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

    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    dispatcher.dispatch(ipv4_packet([10, 0, 0, 9])).await;

    assert_no_datagram(&pair.client).await;
}

#[tokio::test]
async fn test_dispatcher_malformed_short_packet_silently_dropped() {
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

    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    dispatcher.dispatch(Bytes::from_static(b"abc")).await;

    assert_no_datagram(&pair.client).await;
}

#[tokio::test]
async fn test_dispatcher_non_ipv4_packet_silently_dropped() {
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

    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    let mut pkt = vec![0u8; 40];
    pkt[0] = 0x60;
    dispatcher.dispatch(Bytes::from(pkt)).await;

    assert_no_datagram(&pair.client).await;
}

#[tokio::test]
async fn test_downlink_pump_relays_multiple_packets_in_order_and_exits_on_tun_close() {
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

    let p1 = ipv4_packet([10, 0, 0, 2]);
    let p2 = ipv4_packet([10, 0, 0, 2]);
    let p1_copy = p1.clone();
    let p2_copy = p2.clone();

    let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
    src_tx.send(p1).await.unwrap();
    src_tx.send(p2).await.unwrap();
    drop(src_tx);

    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    let pump_result = {
        let mut tun = ChannelSource { rx: src_rx };
        downlink_pump(&mut tun, &dispatcher, &sd_handle()).await
    };
    assert!(pump_result.is_err());

    let r1 = pair.client.read_datagram().await.unwrap();
    let r2 = pair.client.read_datagram().await.unwrap();
    assert_eq!(r1, p1_copy);
    assert_eq!(r2, p2_copy);
}

#[tokio::test]
async fn test_downlink_pump_continues_after_miss() {
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

    let miss_pkt = ipv4_packet([10, 0, 0, 9]);
    let hit_pkt = ipv4_packet([10, 0, 0, 2]);
    let hit_copy = hit_pkt.clone();

    let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
    src_tx.send(miss_pkt).await.unwrap();
    src_tx.send(hit_pkt).await.unwrap();
    drop(src_tx);

    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    let _ = {
        let mut tun = ChannelSource { rx: src_rx };
        downlink_pump(&mut tun, &dispatcher, &sd_handle()).await
    };

    let r1 = pair.client.read_datagram().await.unwrap();
    assert_eq!(r1, hit_copy);
}
