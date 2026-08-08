#![allow(clippy::manual_async_fn)]

use std::future::Future;
use std::io;

use bytes::Bytes;
use tokio::sync::mpsc;
use vpn::data::{DownlinkDispatcher, PacketSource, downlink_pump};

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

struct RecordingDispatcher {
    tx: mpsc::UnboundedSender<Bytes>,
}

impl DownlinkDispatcher for RecordingDispatcher {
    fn dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send {
        let _ = self.tx.send(pkt);
        async {}
    }
}

#[tokio::test]
async fn test_downlink_pump_relays_packets_in_order_until_tun_closes() {
    let p1 = Bytes::from_static(b"tun-pkt-1");
    let p2 = Bytes::from_static(b"tun-pkt-2");
    let p1_copy = p1.clone();
    let p2_copy = p2.clone();

    let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
    let (disp_tx, mut disp_rx) = mpsc::unbounded_channel::<Bytes>();

    src_tx.send(p1).await.unwrap();
    src_tx.send(p2).await.unwrap();
    drop(src_tx);

    let result = {
        let mut tun = ChannelSource { rx: src_rx };
        let dispatcher = RecordingDispatcher { tx: disp_tx };
        downlink_pump(&mut tun, &dispatcher).await
    };
    assert!(result.is_err());

    let received_first = disp_rx.recv().await.unwrap();
    let received_second = disp_rx.recv().await.unwrap();
    assert_eq!(received_first, p1_copy);
    assert_eq!(received_second, p2_copy);
    assert!(disp_rx.recv().await.is_none());
}

#[tokio::test]
async fn test_downlink_pump_keeps_running_after_dispatch_returns() {
    let p1 = Bytes::from_static(b"pkt-a");
    let p2 = Bytes::from_static(b"pkt-b");
    let p1_copy = p1.clone();
    let p2_copy = p2.clone();

    let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
    let (disp_tx, mut disp_rx) = mpsc::unbounded_channel::<Bytes>();

    src_tx.send(p1).await.unwrap();
    src_tx.send(p2).await.unwrap();
    drop(src_tx);

    let result = {
        let mut tun = ChannelSource { rx: src_rx };
        let dispatcher = RecordingDispatcher { tx: disp_tx };
        downlink_pump(&mut tun, &dispatcher).await
    };
    assert!(result.is_err());

    assert_eq!(disp_rx.recv().await.unwrap(), p1_copy);
    assert_eq!(disp_rx.recv().await.unwrap(), p2_copy);
}
