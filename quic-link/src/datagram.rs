use std::future::Future;
use std::io;

use bytes::Bytes;
use shutdown::ShutdownHandle;

pub trait PacketSource {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send;
}

pub trait PacketSink {
    fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send;
}

pub async fn forward<S: PacketSource + Unpin, K: PacketSink + Unpin>(
    source: &mut S,
    sink: &mut K,
    cancel: &ShutdownHandle,
) -> io::Result<()> {
    loop {
        let pkt = tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(()),
            pkt = source.recv() => pkt?,
        };
        sink.send(pkt).await?;
    }
}

#[derive(Clone)]
pub struct DatagramTx {
    conn: quinn::Connection,
}

impl DatagramTx {
    pub fn new(conn: quinn::Connection) -> Self {
        Self { conn }
    }
}

impl PacketSink for DatagramTx {
    fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send {
        async move {
            self.conn
                .send_datagram(pkt)
                .map_err(|e| io::Error::other(e.to_string()))
        }
    }
}

pub struct DatagramRx {
    conn: quinn::Connection,
}

impl DatagramRx {
    pub fn new(conn: quinn::Connection) -> Self {
        Self { conn }
    }
}

impl PacketSource for DatagramRx {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send {
        async move {
            self.conn
                .read_datagram()
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::ConnectionAborted, e.to_string()))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::time::Duration;

    use tokio::sync::mpsc;

    fn sd_handle() -> ShutdownHandle {
        shutdown::Shutdown::new(Duration::from_secs(5)).handle()
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
    async fn test_forward_cancel_when_source_hanging_returns_ok() {
        let cancel = sd_handle();
        let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
        let (sink_tx, mut sink_rx) = mpsc::channel::<Bytes>(8);
        let mut source = ChannelSource { rx: src_rx };
        let mut sink = ChannelSink { tx: sink_tx };

        let cancel_for_task = cancel.clone();
        let task =
            tokio::spawn(async move { forward(&mut source, &mut sink, &cancel_for_task).await });

        tokio::task::yield_now().await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("forward should return promptly after cancel")
            .expect("task should not panic");
        assert!(result.is_ok(), "forward returns Ok(()) on cancel");
        assert!(
            sink_rx.recv().await.is_none(),
            "no packet should be sent after cancel"
        );
        drop(src_tx);
    }

    #[tokio::test]
    async fn test_forward_cancel_biased_priority_over_ready_recv() {
        let cancel = sd_handle();
        let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
        let (sink_tx, mut sink_rx) = mpsc::channel::<Bytes>(8);

        src_tx.send(Bytes::from_static(b"P")).await.unwrap();
        cancel.cancel();

        let result = {
            let mut source = ChannelSource { rx: src_rx };
            let mut sink = ChannelSink { tx: sink_tx };
            forward(&mut source, &mut sink, &cancel).await
        };

        assert!(
            result.is_ok(),
            "biased cancel should win over a ready packet"
        );
        assert!(
            sink_rx.recv().await.is_none(),
            "the ready packet P should be dropped, not forwarded"
        );
        drop(src_tx);
    }

    #[tokio::test]
    async fn test_forward_uncancelled_relays_packets_until_source_error() {
        let cancel = sd_handle();
        let (src_tx, src_rx) = mpsc::channel::<Bytes>(8);
        let (sink_tx, mut sink_rx) = mpsc::channel::<Bytes>(8);

        src_tx.send(Bytes::from_static(b"p1")).await.unwrap();
        src_tx.send(Bytes::from_static(b"p2")).await.unwrap();
        drop(src_tx);

        let result = {
            let mut source = ChannelSource { rx: src_rx };
            let mut sink = ChannelSink { tx: sink_tx };
            forward(&mut source, &mut sink, &cancel).await
        };
        assert!(result.is_err());

        assert_eq!(sink_rx.recv().await.unwrap(), Bytes::from_static(b"p1"));
        assert_eq!(sink_rx.recv().await.unwrap(), Bytes::from_static(b"p2"));
        assert!(sink_rx.recv().await.is_none());
    }

    #[test]
    fn test_datagram_tx_impls_clone_and_packet_sink() {
        fn assert_traits<T: PacketSink + Clone>() {}
        assert_traits::<DatagramTx>();
    }

    #[test]
    fn test_datagram_rx_impls_packet_source() {
        fn assert_traits<T: PacketSource>() {}
        assert_traits::<DatagramRx>();
    }
}
