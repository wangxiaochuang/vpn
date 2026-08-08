#![allow(clippy::unwrap_used, clippy::expect_used, clippy::manual_async_fn)]

mod common;

use std::future::Future;
use std::io;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use vpn::data::{PacketSink, QuinnDatagram, forward};

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

fn make_ipv4_packet(dst: [u8; 4]) -> Bytes {
    let mut pkt = vec![0u8; 20];
    pkt[0] = 0x45;
    pkt[16..20].copy_from_slice(&dst);
    Bytes::from(pkt)
}

#[tokio::test]
async fn test_client_uplink_packet_reaches_server_side() {
    let pair = common::make_connected_pair().await;

    let (tx, mut rx) = mpsc::channel::<Bytes>(8);
    let mut sink = ChannelSink { tx };
    let mut source = QuinnDatagram::new(pair.server.clone());

    let forward_task = tokio::spawn(async move {
        let _ = forward(&mut source, &mut sink).await;
    });

    let pkt = make_ipv4_packet([10, 0, 0, 2]);
    pair.client
        .send_datagram(pkt.clone())
        .expect("send datagram from client");

    let received = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("timeout waiting for server-side receive")
        .expect("packet received");
    assert_eq!(
        received, pkt,
        "client uplink packet should reach server side"
    );

    drop(pair.client);
    let _ = tokio::time::timeout(Duration::from_secs(3), forward_task).await;
}

#[tokio::test]
async fn test_client_downlink_packet_reaches_client_side() {
    let pair = common::make_connected_pair().await;

    let (tx, mut rx) = mpsc::channel::<Bytes>(8);
    let mut sink = ChannelSink { tx };
    let mut source = QuinnDatagram::new(pair.client.clone());

    let forward_task = tokio::spawn(async move {
        let _ = forward(&mut source, &mut sink).await;
    });

    let pkt = make_ipv4_packet([10, 0, 0, 5]);
    pair.server
        .send_datagram(pkt.clone())
        .expect("send datagram from server");

    let received = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("timeout waiting for client-side receive")
        .expect("packet received");
    assert_eq!(
        received, pkt,
        "server downlink packet should reach client side"
    );

    drop(pair.server);
    let _ = tokio::time::timeout(Duration::from_secs(3), forward_task).await;
}

#[tokio::test]
async fn test_client_downlink_exits_when_connection_closed() {
    let pair = common::make_connected_pair().await;

    let (tx, _rx) = mpsc::channel::<Bytes>(8);
    let mut sink = ChannelSink { tx };
    let mut source = QuinnDatagram::new(pair.client.clone());

    let forward_task = tokio::spawn(async move { forward(&mut source, &mut sink).await });

    pair.server.close(0u32.into(), b"bye");
    let result = tokio::time::timeout(Duration::from_secs(3), forward_task).await;
    assert!(
        result.is_ok(),
        "client downlink forward should exit after connection close"
    );
}
