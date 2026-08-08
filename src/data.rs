use std::future::Future;
use std::io;
use std::net::Ipv4Addr;

use bytes::Bytes;

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
) -> io::Result<()> {
    loop {
        let pkt = source.recv().await?;
        sink.send(pkt).await?;
    }
}

pub async fn downlink_pump<S: PacketSource + Unpin, D: DownlinkDispatcher>(
    tun: &mut S,
    dispatcher: &D,
) -> io::Result<()> {
    loop {
        let pkt = tun.recv().await?;
        dispatcher.dispatch(pkt).await;
    }
}

const TUN_RECV_BUF_SIZE: usize = 1280;

impl PacketSource for tun_rs::AsyncDevice {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send {
        async move {
            let mut buf = vec![0u8; TUN_RECV_BUF_SIZE];
            let n = tun_rs::AsyncDevice::recv(self, &mut buf).await?;
            buf.truncate(n);
            Ok(Bytes::from(buf))
        }
    }
}

impl PacketSink for tun_rs::AsyncDevice {
    fn send(&mut self, pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send {
        async move {
            tun_rs::AsyncDevice::send(self, &pkt).await?;
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
    use super::*;

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
}
