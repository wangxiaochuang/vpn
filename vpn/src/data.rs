use std::future::Future;
use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;

use bytes::Bytes;
use shutdown::ShutdownHandle;

pub fn dst_ipv4_addr(pkt: &[u8]) -> Option<Ipv4Addr> {
    let first = *pkt.first()?;
    if pkt.len() < 20 || first >> 4 != 4 {
        return None;
    }
    let octets: [u8; 4] = pkt.get(16..20)?.try_into().ok()?;
    Some(Ipv4Addr::from(octets))
}

pub trait PacketSource {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send;
}

pub trait PacketSink {
    fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send;
}

pub trait DownlinkDispatcher {
    fn dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send;
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

pub async fn downlink_pump<S: PacketSource + Unpin, D: DownlinkDispatcher>(
    tun: &mut S,
    dispatcher: &D,
    cancel: &ShutdownHandle,
) -> io::Result<()> {
    loop {
        let pkt = tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(()),
            pkt = tun.recv() => pkt?,
        };
        dispatcher.dispatch(pkt).await;
    }
}

pub const TUN_RECV_BUF_SIZE: usize = 65_535;

#[derive(Clone)]
pub struct Tun(pub Arc<tun_rs::AsyncDevice>);

impl std::fmt::Debug for Tun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Tun").finish_non_exhaustive()
    }
}

impl PacketSource for Tun {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send {
        async move {
            let mut buf = vec![0u8; TUN_RECV_BUF_SIZE];
            let n = tun_rs::AsyncDevice::recv(&self.0, &mut buf).await?;
            buf.truncate(n);
            Ok(Bytes::from(buf))
        }
    }
}

impl PacketSink for Tun {
    fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send {
        async move {
            tun_rs::AsyncDevice::send(&self.0, &pkt).await?;
            Ok(())
        }
    }
}

pub struct QuinnDatagram {
    conn: quinn::Connection,
}

impl QuinnDatagram {
    pub fn new(conn: quinn::Connection) -> Self {
        Self { conn }
    }
}

impl PacketSource for QuinnDatagram {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send {
        async move {
            self.conn
                .read_datagram()
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::ConnectionAborted, e.to_string()))
        }
    }
}

impl PacketSink for QuinnDatagram {
    fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send {
        async move {
            self.conn
                .send_datagram(pkt)
                .map_err(|e| io::Error::other(e.to_string()))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use std::future::Future;
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::*;

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

    struct RecordingDispatcher {
        tx: mpsc::UnboundedSender<Bytes>,
    }

    impl DownlinkDispatcher for RecordingDispatcher {
        fn dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send {
            let _ = self.tx.send(pkt);
            async {}
        }
    }

    fn ip_header(version_ihl: u8, dst: [u8; 4], total_len: usize) -> Vec<u8> {
        let mut pkt = vec![0u8; total_len];
        pkt[0] = version_ihl;
        pkt[16..20].copy_from_slice(&dst);
        pkt
    }

    #[test]
    fn test_dst_ipv4_addr_standard_20_byte_packet_returns_some() {
        let pkt = ip_header(0x45, [10, 0, 0, 5], 20);
        assert_eq!(dst_ipv4_addr(&pkt), Some(Ipv4Addr::new(10, 0, 0, 5)));
    }

    #[test]
    fn test_dst_ipv4_addr_40_byte_packet_returns_some() {
        let pkt = ip_header(0x45, [192, 168, 1, 1], 40);
        assert_eq!(dst_ipv4_addr(&pkt), Some(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn test_dst_ipv4_addr_packet_shorter_than_20_bytes_returns_none() {
        let pkt = vec![0x45u8; 19];
        assert_eq!(dst_ipv4_addr(&pkt), None);
    }

    #[test]
    fn test_dst_ipv4_addr_non_ipv4_version_returns_none() {
        let pkt = ip_header(0x60, [10, 0, 0, 1], 40);
        assert_eq!(dst_ipv4_addr(&pkt), None);
    }

    #[test]
    fn test_dst_ipv4_addr_packet_with_options_returns_some() {
        let pkt = ip_header(0x46, [10, 0, 0, 2], 24);
        assert_eq!(dst_ipv4_addr(&pkt), Some(Ipv4Addr::new(10, 0, 0, 2)));
    }

    #[test]
    fn test_dst_ipv4_addr_empty_packet_returns_none() {
        assert_eq!(dst_ipv4_addr(&[]), None);
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

    fn spawn_downlink_pump(
        tun_rx: mpsc::Receiver<Bytes>,
        disp_tx: mpsc::UnboundedSender<Bytes>,
        cancel: &ShutdownHandle,
    ) -> tokio::task::JoinHandle<io::Result<()>> {
        let mut tun = ChannelSource { rx: tun_rx };
        let dispatcher = RecordingDispatcher { tx: disp_tx };
        let cancel_for_task = cancel.clone();
        tokio::spawn(async move { downlink_pump(&mut tun, &dispatcher, &cancel_for_task).await })
    }

    #[tokio::test]
    async fn test_downlink_pump_cancel_when_tun_hanging_returns_ok() {
        let cancel = sd_handle();
        let (tun_tx, tun_rx) = mpsc::channel::<Bytes>(8);
        let (disp_tx, mut disp_rx) = mpsc::unbounded_channel::<Bytes>();
        let task = spawn_downlink_pump(tun_rx, disp_tx, &cancel);
        tokio::task::yield_now().await;
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("downlink_pump should return promptly after cancel")
            .expect("task should not panic");
        assert!(result.is_ok(), "downlink_pump returns Ok(()) on cancel");
        assert!(
            disp_rx.recv().await.is_none(),
            "no packet should be dispatched after cancel"
        );
        drop(tun_tx);
    }

    #[tokio::test]
    async fn test_downlink_pump_uncancelled_relays_until_tun_error() {
        let cancel = sd_handle();
        let (tun_tx, tun_rx) = mpsc::channel::<Bytes>(8);
        let (disp_tx, mut disp_rx) = mpsc::unbounded_channel::<Bytes>();

        tun_tx.send(Bytes::from_static(b"p1")).await.unwrap();
        drop(tun_tx);

        let result = {
            let mut tun = ChannelSource { rx: tun_rx };
            let dispatcher = RecordingDispatcher { tx: disp_tx };
            downlink_pump(&mut tun, &dispatcher, &cancel).await
        };
        assert!(result.is_err());

        assert_eq!(disp_rx.recv().await.unwrap(), Bytes::from_static(b"p1"));
        assert!(disp_rx.recv().await.is_none());
    }

    #[test]
    fn test_tun_impls_packet_source_sink_and_clone() {
        fn assert_traits<T: PacketSource + PacketSink + Clone>() {}
        assert_traits::<Tun>();
    }

    #[test]
    fn test_tun_recv_buf_size_covers_max_ipv4_packet_length() {
        use crate::config::MIN_MTU;
        assert_eq!(TUN_RECV_BUF_SIZE, 65_535);
        assert!(TUN_RECV_BUF_SIZE >= usize::from(MIN_MTU));
    }

    /// Spec scenario: `TUN_RECV_BUF_SIZE 覆盖最大 IPv4 包长度` —
    /// 该常量 SHALL 等于 65535（`u16::MAX`），覆盖 IPv4 total length 字段最大值。
    #[test]
    fn test_tun_recv_buf_size_equals_u16_max() {
        assert_eq!(TUN_RECV_BUF_SIZE, u16::MAX as usize);
    }
}
