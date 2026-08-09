#![allow(clippy::unwrap_used, clippy::expect_used, clippy::manual_async_fn)]

mod common;

use std::future::Future;
use std::io;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use vpn::data::{PacketSink, QuinnDatagram, forward};

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

fn make_ipv4_packet(dst: [u8; 4]) -> Vec<u8> {
    let mut pkt = vec![0u8; 20];
    pkt[0] = 0x45;
    pkt[16..20].copy_from_slice(&dst);
    pkt
}

#[tokio::test]
async fn test_client_datagram_forwarded_to_sink() {
    let pair = common::make_connected_pair().await;

    let (tx, mut rx) = mpsc::channel::<Bytes>(8);
    let mut sink = ChannelSink { tx };
    let mut source = QuinnDatagram::new(pair.server.clone());

    let forward_task = tokio::spawn(async move {
        let _ = forward(&mut source, &mut sink, &sd_handle()).await;
    });

    let pkt = make_ipv4_packet([10, 0, 0, 2]);
    pair.client
        .send_datagram(Bytes::from(pkt.clone()))
        .expect("send datagram");

    let received = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("timeout waiting for packet")
        .expect("packet received");
    assert_eq!(received.as_ref(), &pkt[..]);

    drop(pair.client);
    let _ = tokio::time::timeout(Duration::from_secs(3), forward_task).await;
}

#[tokio::test]
async fn test_uplink_exits_on_connection_close() {
    let pair = common::make_connected_pair().await;

    let (tx, _rx) = mpsc::channel::<Bytes>(8);
    let mut sink = ChannelSink { tx };
    let mut source = QuinnDatagram::new(pair.server.clone());

    let forward_task =
        tokio::spawn(async move { forward(&mut source, &mut sink, &sd_handle()).await });

    pair.client.close(0u32.into(), b"bye");

    let result = tokio::time::timeout(Duration::from_secs(3), forward_task).await;
    assert!(
        result.is_ok(),
        "forward task should exit after connection close"
    );
}
