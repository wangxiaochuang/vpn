use std::hash::{Hash, Hasher};
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::auth::UserStore;
use crate::config::ServerConfig;
use crate::ctrl::{self, HEARTBEAT_INTERVAL, HeartbeatTracker, deny_reason_from};
use crate::data::{
    DownlinkDispatcher, PacketSink, PacketSource, QuinnDatagram, downlink_pump, dst_ipv4_addr,
    forward,
};
use crate::framing::ControlCodec;
use crate::ipam::IpPool;
use crate::route::SessionRegistry;
use crate::tun_setup::gateway_addr;
use crate::vpn::control_message::Msg;
use crate::vpn::{AuthDenied, AuthOk, ControlMessage, Heartbeat};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::codec::{Framed, FramedParts};

#[derive(Debug)]
pub struct ConnectionHandle {
    id: usize,
    pub conn: quinn::Connection,
    pub ip: Ipv4Addr,
}

impl ConnectionHandle {
    pub fn new(conn: quinn::Connection, ip: Ipv4Addr) -> Self {
        Self {
            id: conn.stable_id(),
            conn,
            ip,
        }
    }

    pub fn id(&self) -> usize {
        self.id
    }
}

impl Clone for ConnectionHandle {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            conn: self.conn.clone(),
            ip: self.ip,
        }
    }
}

impl PartialEq for ConnectionHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ConnectionHandle {}

impl Hash for ConnectionHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

pub struct ServerState {
    pub users: UserStore,
    pub pool: std::sync::Mutex<IpPool>,
    pub registry: std::sync::Mutex<SessionRegistry<ConnectionHandle>>,
    pub tun: Option<Arc<tun_rs::AsyncDevice>>,
    pub config: Arc<ServerConfig>,
}

pub type SharedState = Arc<ServerState>;

pub struct TunSource(pub Arc<tun_rs::AsyncDevice>);

impl PacketSource for TunSource {
    fn recv(&mut self) -> impl std::future::Future<Output = std::io::Result<bytes::Bytes>> + Send {
        async move {
            let mut buf = vec![0u8; 1280];
            let n = tun_rs::AsyncDevice::recv(&self.0, &mut buf).await?;
            buf.truncate(n);
            Ok(bytes::Bytes::from(buf))
        }
    }
}

pub struct TunSink(pub Arc<tun_rs::AsyncDevice>);

impl PacketSink for TunSink {
    fn send(
        &mut self,
        pkt: bytes::Bytes,
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send {
        async move {
            tun_rs::AsyncDevice::send(&self.0, &pkt).await?;
            Ok(())
        }
    }
}

pub struct ControlStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl ControlStream {
    pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self { send, recv }
    }

    pub fn into_parts(self) -> (quinn::SendStream, quinn::RecvStream) {
        (self.send, self.recv)
    }
}

impl AsyncRead for ControlStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buf)
    }
}

impl AsyncWrite for ControlStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
    }
}

pub struct RegistryDispatcher {
    pub state: SharedState,
}

impl DownlinkDispatcher for RegistryDispatcher {
    fn dispatch(&self, pkt: bytes::Bytes) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let Some(dst) = dst_ipv4_addr(&pkt) else {
                return;
            };
            let handle = {
                let Ok(reg) = self.state.registry.lock() else {
                    return;
                };
                reg.lookup(dst).cloned()
            };
            if let Some(h) = handle {
                let _ = h.conn.send_datagram(pkt);
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
pub async fn handle_conn(conn: quinn::Connection, state: SharedState) -> anyhow::Result<()> {
    let (send, recv) = conn
        .accept_bi()
        .await
        .map_err(|e| anyhow::anyhow!("failed to accept control stream: {e}"))?;
    let mut framed = Framed::new(ControlStream::new(send, recv), ControlCodec::new());

    let first = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("control stream closed before first message"))?
        .map_err(|e| anyhow::anyhow!("failed to decode first message: {e}"))?;

    let Some(Msg::AuthRequest(req)) = first.msg else {
        conn.close(0u32.into(), b"protocol-error");
        return Ok(());
    };

    let auth_result = {
        let mut pool = state
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ctrl::authenticate(&state.users, &mut pool, &req)
    };

    let ip = match auth_result {
        Ok(ip) => ip,
        Err(e) => {
            let deny = ControlMessage {
                msg: Some(Msg::AuthDenied(AuthDenied {
                    reason: deny_reason_from(&e) as i32,
                })),
            };
            let _ = framed.send(deny).await;
            let _ = framed.close().await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            conn.close(0u32.into(), b"auth-denied");
            return Ok(());
        }
    };
    let handle = ConnectionHandle::new(conn.clone(), ip);
    let evicted = {
        let mut reg = state
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.insert(&req.username, ip, handle)
    };

    match evicted {
        Ok(Some(evicted)) => {
            {
                let mut pool = state
                    .pool
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = pool.free(evicted.ip);
            }
            evicted.handle.conn.close(0u32.into(), b"superseded");
        }
        Ok(None) => {}
        Err(_) => {
            conn.close(0u32.into(), b"internal-error");
            return Ok(());
        }
    }

    let gateway = gateway_addr(state.config.tun_subnet);
    let auth_ok = ControlMessage {
        msg: Some(Msg::AuthOk(AuthOk {
            assigned_ip: ip.to_string(),
            subnet: state.config.tun_subnet.to_string(),
            gateway: gateway.to_string(),
            mtu: u32::from(state.config.mtu),
        })),
    };
    framed
        .send(auth_ok)
        .await
        .map_err(|e| anyhow::anyhow!("failed to send AuthOk: {e}"))?;

    let parts = framed.into_parts();
    let control_stream = parts.io;
    let read_buf = parts.read_buf;
    let (send_stream, recv_stream) = control_stream.into_parts();

    let mut reader_parts = FramedParts::new(recv_stream, ControlCodec::new());
    reader_parts.read_buf = read_buf;
    let mut reader = Framed::from_parts(reader_parts);
    let mut writer = Framed::new(send_stream, ControlCodec::new());

    let conn_for_hb = conn.clone();
    let ctrl_task = tokio::spawn(async move {
        let mut tracker = HeartbeatTracker::new(tokio::time::Instant::now().into_std());
        let mut send_tick = tokio::time::interval(HEARTBEAT_INTERVAL);
        let mut timeout_tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                biased;
                _ = timeout_tick.tick() => {
                    if tracker.is_dead(tokio::time::Instant::now().into_std()) {
                        conn_for_hb.close(0x100u32.into(), b"timeout");
                        break;
                    }
                }
                _ = send_tick.tick() => {
                    let hb = ControlMessage {
                        msg: Some(Msg::Heartbeat(Heartbeat {})),
                    };
                    if writer.send(hb).await.is_err() {
                        break;
                    }
                }
                msg = reader.next() => {
                    match msg {
                        Some(Ok(ControlMessage { msg: Some(Msg::Heartbeat(_)) })) => {
                            tracker.observe(tokio::time::Instant::now().into_std());
                        }
                        Some(Ok(_)) => {}
                        _ => break,
                    }
                }
            }
        }
    });

    let uplink_task = if let Some(tun) = state.tun.clone() {
        let conn_for_uplink = conn.clone();
        Some(tokio::spawn(async move {
            let mut source = QuinnDatagram::new(conn_for_uplink.clone());
            let mut sink = TunSink(tun);
            let _ = forward(&mut source, &mut sink).await;
            conn_for_uplink.close(0x101u32.into(), b"uplink-ended");
        }))
    } else {
        None
    };

    let _ = ctrl_task.await;
    if let Some(t) = uplink_task {
        let _ = t.await;
    }

    let _ = state
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove_by_ip(ip);
    let _ = state
        .pool
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .free(ip);

    Ok(())
}

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    let quinn_cfg = crate::tls::build_quinn_server_config(&config.cert, &config.key)?;
    let endpoint = quinn::Endpoint::server(quinn_cfg, config.listen)?;
    tracing::info!(
        "listening on {}",
        endpoint.local_addr().unwrap_or(config.listen)
    );

    let tun_device = crate::tun_setup::create_tun(config.tun_subnet, config.mtu)?;
    let tun = Arc::new(tun_device);

    let user_pairs: Vec<(String, String)> = config
        .users
        .iter()
        .map(|u| (u.username.clone(), u.password_hash.clone()))
        .collect();
    let users = UserStore::from_users(user_pairs)?;
    let pool = IpPool::new(config.tun_subnet)?;
    let registry = SessionRegistry::new();

    let state: SharedState = Arc::new(ServerState {
        users,
        pool: std::sync::Mutex::new(pool),
        registry: std::sync::Mutex::new(registry),
        tun: Some(tun.clone()),
        config: Arc::new(config),
    });

    let downlink_tun = TunSource(tun);
    let dispatcher = RegistryDispatcher {
        state: state.clone(),
    };
    tokio::spawn(async move {
        let mut src = downlink_tun;
        let _ = downlink_pump(&mut src, &dispatcher).await;
    });

    let accept_endpoint = endpoint.clone();
    let accept_state = state.clone();
    let accept_loop = async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            match incoming.await {
                Ok(conn) => {
                    let st = accept_state.clone();
                    tokio::spawn(async move {
                        let _ = handle_conn(conn, st).await;
                    });
                }
                Err(e) => {
                    tracing::warn!("connection accept error: {e}");
                }
            }
        }
    };

    tokio::select! {
        () = accept_loop => {}
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received Ctrl+C, shutting down");
        }
    }

    endpoint.close(0u32.into(), b"shutdown");
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::mutable_key_type)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hash;
    use std::sync::Arc as StdArc;

    #[derive(Debug)]
    struct NoVerify;

    impl rustls::client::danger::ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls_pki_types::CertificateDer<'_>,
            _intermediates: &[rustls_pki_types::CertificateDer<'_>],
            _server_name: &rustls_pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls_pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls_pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls_pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ED25519,
                rustls::SignatureScheme::RSA_PSS_SHA256,
            ]
        }
    }

    fn repo(p: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("vpn crate nested under repo root")
            .join(p)
    }

    async fn make_client_conns(n: usize) -> Vec<quinn::Connection> {
        let cert = repo("cert.pem");
        let key = repo("key.pem");
        let server_cfg = crate::tls::build_quinn_server_config(&cert, &key).expect("server cfg");
        let server = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap())
            .expect("server endpoint");

        let server_for_accept = server.clone();
        tokio::spawn(async move {
            while let Some(incoming) = server_for_accept.accept().await {
                let _ = incoming.accept().map(|c| {
                    tokio::spawn(async move {
                        let _ = c.await;
                    });
                });
            }
        });

        let rustls_client = rustls::ClientConfig::builder_with_provider(StdArc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(StdArc::new(NoVerify))
        .with_no_client_auth();
        let quic_client =
            quinn::crypto::rustls::QuicClientConfig::try_from(StdArc::new(rustls_client)).unwrap();
        let client_cfg = quinn::ClientConfig::new(StdArc::new(quic_client));

        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = server.local_addr().unwrap();

        let mut conns = Vec::new();
        for _ in 0..n {
            let conn = client
                .connect_with(client_cfg.clone(), addr, "localhost")
                .expect("dial")
                .await
                .expect("connect");
            conns.push(conn);
        }
        conns
    }

    #[tokio::test]
    async fn test_connection_handle_eq_by_id_across_clones() {
        let mut conns = make_client_conns(1).await;
        let conn = conns.remove(0);
        let dup = conn.clone();
        let h1 = ConnectionHandle::new(conn, Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(dup, Ipv4Addr::new(10, 0, 0, 9));
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn test_connection_handle_neq_different_id() {
        let mut conns = make_client_conns(2).await;
        let h1 = ConnectionHandle::new(conns.remove(0), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(conns.remove(0), Ipv4Addr::new(10, 0, 0, 3));
        assert_ne!(h1.id(), h2.id());
        assert_ne!(h1, h2);
    }

    #[tokio::test]
    async fn test_connection_handle_hash_by_id() {
        let mut conns = make_client_conns(1).await;
        let conn = conns.remove(0);
        let h1 = ConnectionHandle::new(conn.clone(), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(conn, Ipv4Addr::new(10, 0, 0, 9));
        let mut s1 = DefaultHasher::new();
        let mut s2 = DefaultHasher::new();
        h1.hash(&mut s1);
        h2.hash(&mut s2);
        assert_eq!(s1.finish(), s2.finish());
    }

    #[tokio::test]
    async fn test_connection_handle_dedups_in_hashset() {
        let mut conns = make_client_conns(1).await;
        let conn = conns.remove(0);
        let h1 = ConnectionHandle::new(conn.clone(), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(conn, Ipv4Addr::new(10, 0, 0, 3));
        let mut set = HashSet::new();
        set.insert(h1);
        assert!(set.contains(&h2));
        set.insert(h2);
        assert_eq!(set.len(), 1);
    }
}
