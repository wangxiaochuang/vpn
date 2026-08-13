#![allow(clippy::manual_async_fn)]

use std::future::Future;
use std::io;

use bytes::Bytes;
use tokio::sync::mpsc;
use vpn_core::data::PacketSink;
use vpn_core::data::PacketSource;
use vpn_core::data::TUN_RECV_BUF_SIZE;
use vpn_core::data::forward;

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
                None => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source closed",
                )),
            }
        }
    }
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

fn ipv4_packet(total_len: usize) -> Bytes {
    let mut pkt = vec![0u8; total_len];
    pkt[0] = 0x45;
    pkt[16..20].copy_from_slice(&[10, 0, 0, 5]);
    Bytes::from(pkt)
}

#[test]
fn test_recv_buf_size_covers_typical_ethernet_mtu() {
    const { assert!(TUN_RECV_BUF_SIZE >= 1500) };
}

#[tokio::test]
async fn test_forward_1400_byte_ipv4_packet_arrives_intact() {
    const PACKET_LEN: usize = 1400;
    let pkt = ipv4_packet(PACKET_LEN);
    let pkt_copy = pkt.clone();

    let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
    let (sink_tx, mut sink_rx) = mpsc::channel::<Bytes>(8);

    src_tx.send(pkt).await.unwrap();
    drop(src_tx);

    let result = {
        let mut source = ChannelSource { rx: src_rx };
        let mut sink = ChannelSink { tx: sink_tx };
        forward(&mut source, &mut sink, &sd_handle()).await
    };
    assert!(result.is_err(), "forward should exit after source closes");

    let received = sink_rx
        .recv()
        .await
        .expect("should receive 1400-byte packet");
    assert_eq!(received.len(), PACKET_LEN);
    assert_eq!(received, pkt_copy);
}
