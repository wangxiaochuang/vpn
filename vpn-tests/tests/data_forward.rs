#![allow(clippy::manual_async_fn)]

use std::future::Future;
use std::io;

use bytes::Bytes;
use tokio::sync::mpsc;
use vpn_core::data::PacketSink;
use vpn_core::data::PacketSource;
use vpn_core::data::forward;

fn sd_handle() -> shutdown::ShutdownHandle {
    shutdown::Shutdown::default().handle()
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

#[tokio::test]
async fn test_forward_relays_packets_in_order_until_source_closes() {
    let p1 = Bytes::from_static(b"packet-1");
    let p2 = Bytes::from_static(b"packet-2");
    let p1_copy = p1.clone();
    let p2_copy = p2.clone();

    let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
    let (sink_tx, mut sink_rx) = mpsc::channel::<Bytes>(8);

    src_tx.send(p1).await.unwrap();
    src_tx.send(p2).await.unwrap();
    drop(src_tx);

    let result = {
        let mut source = ChannelSource { rx: src_rx };
        let mut sink = ChannelSink { tx: sink_tx };
        forward(&mut source, &mut sink, &sd_handle()).await
    };
    assert!(result.is_err());

    let received_first = sink_rx.recv().await.unwrap();
    let received_second = sink_rx.recv().await.unwrap();
    assert_eq!(received_first, p1_copy);
    assert_eq!(received_second, p2_copy);
    assert!(sink_rx.recv().await.is_none());
}

#[tokio::test]
async fn test_forward_source_first_error_forwards_no_packets() {
    let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
    drop(src_tx);
    let (sink_tx, mut sink_rx) = mpsc::channel::<Bytes>(8);

    let result = {
        let mut source = ChannelSource { rx: src_rx };
        let mut sink = ChannelSink { tx: sink_tx };
        forward(&mut source, &mut sink, &sd_handle()).await
    };
    assert!(result.is_err());
    assert!(sink_rx.recv().await.is_none());
}
