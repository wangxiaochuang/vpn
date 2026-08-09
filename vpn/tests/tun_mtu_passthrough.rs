//! Q2 场景：MTU > 1280 配置下的大包转发不丢失字节。
//!
//! 绑定 spec scenario「Tun 适配后 recv 返回 TUN 读到的完整包」与
//! 「TUN_RECV_BUF_SIZE 覆盖最大 IPv4 包长度」。
//!
//! 旧实现 `TUN_RECV_BUF_SIZE = 1280` 会在 MTU > 1280 时静默截断 IP 包；
//! 现修正为 65535（`u16::MAX`）。本测试验证：
//! 1. 常量足以覆盖典型以太网 MTU（1500）及更大包；
//! 2. `forward` 数据面能完整转发 1400 字节的 IP 包。
//!
//! 真机 TUN 设备收发验证见 `doc/release-test-checklist.md`（Q3）。

#![allow(clippy::manual_async_fn)]

use std::future::Future;
use std::io;

use bytes::Bytes;
use tokio::sync::mpsc;
use vpn::data::{PacketSink, PacketSource, TUN_RECV_BUF_SIZE, forward};

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
