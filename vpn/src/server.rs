use std::hash::{Hash, Hasher};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::auth::UserStore;
use crate::config::ServerConfig;
use crate::ctrl::{self, deny_reason_from};
use crate::data::{DownlinkDispatcher, QuinnDatagram, Tun, downlink_pump, dst_ipv4_addr, forward};
use crate::ipam::IpPool;
use crate::route::SessionRegistry;
use crate::tun_setup::gateway_addr;
use crate::vpn::control_message::Msg;
use crate::vpn::{AuthDenied, AuthOk, ControlMessage, Disconnect, Heartbeat};
use msgx::channel::{Receiver, Sender};
use msgx::{Channel, KEEPALIVE_INTERVAL, KeepaliveTracker};
use shutdown::Shutdown;
use shutdown::ShutdownHandle;

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

const FIRST_MSG_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn handle_conn(
    conn: quinn::Connection,
    state: SharedState,
    shutdown: ShutdownHandle,
) -> anyhow::Result<()> {
    let Some((channel, ip)) = setup_session(&conn, &state).await? else {
        return Ok(());
    };
    let (sender, receiver) = channel.split();
    let conn_for_hb = conn.clone();
    let shutdown_for_hb = shutdown.clone();
    let ctrl_task = tokio::spawn(async move {
        run_ctrl_loop(conn_for_hb, sender, receiver, shutdown_for_hb).await;
    });

    let uplink_task = spawn_uplink(&state, &conn, shutdown);
    let _ = ctrl_task.await;
    if let Some(t) = uplink_task {
        let _ = t.await;
    }
    cleanup_session(&state, ip);
    Ok(())
}

async fn setup_session(
    conn: &quinn::Connection,
    state: &SharedState,
) -> anyhow::Result<Option<(Channel<ControlMessage>, Ipv4Addr)>> {
    let mut channel = crate::quinn_stream::accept_bi::<ControlMessage>(conn)
        .await
        .map_err(|e| anyhow::anyhow!("failed to accept control stream: {e}"))?;
    let Some(req) = recv_auth_request(&mut channel, conn).await? else {
        return Ok(None);
    };
    let ip = match resolve_auth(state, &req) {
        AuthResolution::Denied(reason) => {
            finish_denied(channel, conn, reason).await;
            return Ok(None);
        }
        AuthResolution::Ok(ip) => ip,
    };
    if !register_and_evict(state, &req.username, ip, conn) {
        return Ok(None);
    }
    send_auth_ok(&mut channel, state, ip).await?;
    Ok(Some((channel, ip)))
}

async fn recv_auth_request(
    channel: &mut Channel<ControlMessage>,
    conn: &quinn::Connection,
) -> anyhow::Result<Option<crate::vpn::AuthRequest>> {
    let first = channel
        .recv_timeout(FIRST_MSG_TIMEOUT)
        .await
        .map_err(|e| anyhow::anyhow!("failed to receive first message: {e}"))?;
    if let Some(Msg::AuthRequest(req)) = first.msg {
        Ok(Some(req))
    } else {
        conn.close(0u32.into(), b"protocol-error");
        Ok(None)
    }
}

enum AuthResolution {
    Ok(Ipv4Addr),
    Denied(crate::vpn::DenyReason),
}

fn resolve_auth(state: &SharedState, req: &crate::vpn::AuthRequest) -> AuthResolution {
    let result = {
        let mut pool = state
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ctrl::authenticate(&state.users, &mut pool, req)
    };
    match result {
        Ok(ip) => AuthResolution::Ok(ip),
        Err(e) => AuthResolution::Denied(deny_reason_from(&e)),
    }
}

async fn finish_denied(
    mut channel: Channel<ControlMessage>,
    conn: &quinn::Connection,
    reason: crate::vpn::DenyReason,
) {
    let deny = ControlMessage {
        msg: Some(Msg::AuthDenied(AuthDenied {
            reason: reason as i32,
        })),
    };
    let _ = channel.send(deny).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    conn.close(0u32.into(), b"auth-denied");
}

fn register_and_evict(
    state: &SharedState,
    username: &str,
    ip: Ipv4Addr,
    conn: &quinn::Connection,
) -> bool {
    let handle = ConnectionHandle::new(conn.clone(), ip);
    let evicted = {
        let mut reg = state
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.insert(username, ip, handle)
    };
    match evicted {
        Ok(Some(evicted)) => {
            free_ip(state, evicted.ip);
            evicted.handle.conn.close(0u32.into(), b"superseded");
            true
        }
        Ok(None) => true,
        Err(_) => {
            conn.close(0u32.into(), b"internal-error");
            false
        }
    }
}

fn free_ip(state: &SharedState, ip: Ipv4Addr) {
    let mut pool = state
        .pool
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = pool.free(ip);
}

async fn send_auth_ok(
    channel: &mut Channel<ControlMessage>,
    state: &SharedState,
    ip: Ipv4Addr,
) -> anyhow::Result<()> {
    channel
        .send(build_auth_ok(state, ip))
        .await
        .map_err(|e| anyhow::anyhow!("failed to send AuthOk: {e}"))
}

fn build_auth_ok(state: &SharedState, ip: Ipv4Addr) -> ControlMessage {
    let gateway = gateway_addr(state.config.tun_subnet);
    ControlMessage {
        msg: Some(Msg::AuthOk(AuthOk {
            assigned_ip: ip.to_string(),
            subnet: state.config.tun_subnet.to_string(),
            gateway: gateway.to_string(),
            mtu: u32::from(state.config.mtu),
            routes: state
                .config
                .routes
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        })),
    }
}

fn spawn_uplink(
    state: &SharedState,
    conn: &quinn::Connection,
    shutdown: ShutdownHandle,
) -> Option<tokio::task::JoinHandle<()>> {
    let tun = state.tun.clone()?;
    let conn_for_uplink = conn.clone();
    Some(tokio::spawn(async move {
        let mut source = QuinnDatagram::new(conn_for_uplink.clone());
        let mut sink = Tun(tun);
        let _ = forward(&mut source, &mut sink, &shutdown).await;
        conn_for_uplink.close(0x101u32.into(), b"uplink-ended");
    }))
}

#[allow(clippy::too_many_lines)]
async fn run_ctrl_loop(
    conn: quinn::Connection,
    mut writer: Sender<ControlMessage>,
    mut reader: Receiver<ControlMessage>,
    shutdown: ShutdownHandle,
) {
    let mut tracker = KeepaliveTracker::new(now());
    let mut send_tick = tokio::time::interval(KEEPALIVE_INTERVAL);
    let mut timeout_tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                let _ = writer
                    .send(ControlMessage {
                        msg: Some(Msg::Disconnect(Disconnect {
                            reason: "server-shutdown".to_string(),
                        })),
                    })
                    .await;
                break;
            }
            _ = timeout_tick.tick() => {
                if tracker.is_dead(now()) {
                    conn.close(0x100u32.into(), b"timeout");
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
            msg = reader.recv() => {
                match msg {
                    Ok(Some(_)) => tracker.observe(now()),
                    _ => break,
                }
            }
        }
    }
}

fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

fn cleanup_session(state: &SharedState, ip: Ipv4Addr) {
    let _ = state
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove_by_ip(ip);
    free_ip(state, ip);
}

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    let endpoint = build_server_endpoint(&config)?;
    let tun = Arc::new(crate::tun_setup::create_tun(config.tun_subnet, config.mtu)?);
    let state = build_server_state(config, tun.clone())?;
    let sd = Shutdown::new(Duration::from_secs(5));
    let ready = shutdown::spawn_signal_watchdog(sd.clone());
    let _ = ready.await;
    spawn_downlink(tun, state.clone(), sd.handle());
    let mut conn_set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    accept_connections(&endpoint, state, &sd, &mut conn_set).await;
    endpoint.close(0u32.into(), b"shutdown");
    sd.drain(&mut conn_set, "server").await;
    Ok(())
}

fn build_server_endpoint(config: &ServerConfig) -> anyhow::Result<quinn::Endpoint> {
    let quinn_cfg = crate::tls::build_quinn_server_config(&config.cert, &config.key)?;
    let endpoint = quinn::Endpoint::server(quinn_cfg, config.listen)?;
    tracing::info!(
        "listening on {}",
        endpoint.local_addr().unwrap_or(config.listen)
    );
    Ok(endpoint)
}

fn build_server_state(
    config: ServerConfig,
    tun: Arc<tun_rs::AsyncDevice>,
) -> anyhow::Result<SharedState> {
    let user_pairs: Vec<(String, String)> = config
        .users
        .iter()
        .map(|u| (u.username.clone(), u.password_hash.clone()))
        .collect();
    let users = UserStore::from_users(user_pairs)?;
    let pool = IpPool::new(config.tun_subnet)?;
    let registry = SessionRegistry::new();
    Ok(Arc::new(ServerState {
        users,
        pool: std::sync::Mutex::new(pool),
        registry: std::sync::Mutex::new(registry),
        tun: Some(tun),
        config: Arc::new(config),
    }))
}

fn spawn_downlink(tun: Arc<tun_rs::AsyncDevice>, state: SharedState, shutdown: ShutdownHandle) {
    let downlink_tun = Tun(tun);
    let dispatcher = RegistryDispatcher { state };
    tokio::spawn(async move {
        let mut src = downlink_tun;
        let _ = downlink_pump(&mut src, &dispatcher, &shutdown).await;
    });
}

async fn accept_connections(
    endpoint: &quinn::Endpoint,
    state: SharedState,
    sd: &Shutdown,
    conn_set: &mut tokio::task::JoinSet<()>,
) {
    let accept_endpoint = endpoint.clone();
    let handle = sd.handle();
    run_accept_loop(&accept_endpoint, &state, &handle, conn_set).await;
    tracing::info!("initiating graceful shutdown");
    sd.trigger();
}

async fn run_accept_loop(
    endpoint: &quinn::Endpoint,
    state: &SharedState,
    shutdown: &ShutdownHandle,
    conn_set: &mut tokio::task::JoinSet<()>,
) {
    loop {
        if !accept_one(endpoint, state, shutdown, conn_set).await {
            break;
        }
    }
}

async fn accept_one(
    endpoint: &quinn::Endpoint,
    state: &SharedState,
    shutdown: &ShutdownHandle,
    conn_set: &mut tokio::task::JoinSet<()>,
) -> bool {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => false,
        incoming = endpoint.accept() => {
            match incoming {
                Some(incoming) => match incoming.await {
                    Ok(conn) => spawn_handle_conn(conn, state, shutdown, conn_set),
                    Err(e) => tracing::warn!("connection accept error: {e}"),
                },
                None => return false,
            }
            true
        }
    }
}

fn spawn_handle_conn(
    conn: quinn::Connection,
    state: &SharedState,
    shutdown: &ShutdownHandle,
    conn_set: &mut tokio::task::JoinSet<()>,
) {
    let st = state.clone();
    let ct = shutdown.clone();
    conn_set.spawn(async move {
        let _ = handle_conn(conn, st, ct).await;
    });
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
        let server = build_test_server();
        let client_cfg = build_no_verify_client_config();
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

    fn build_test_server() -> quinn::Endpoint {
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
        server
    }

    fn build_no_verify_client_config() -> quinn::ClientConfig {
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
        quinn::ClientConfig::new(StdArc::new(quic_client))
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
