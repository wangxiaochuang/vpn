use std::hash::{Hash, Hasher};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use crate::auth::UserStore;
use crate::config::ServerConfig;
use crate::ctrl::{self, deny_reason_from};
use crate::data::{DownlinkDispatcher, Tun, downlink_pump, dst_ipv4_addr};
use crate::ledger::{ConnectionLedger, Evicted, ReservedIp};
use crate::telemetry::TelemetryPlane;
use crate::telemetry::TelemetryTxSlot;
use crate::telemetry::make_telemetry_tx_slot;
use crate::tun_setup::gateway_addr;
use crate::vpn::control_message::Msg;
use crate::vpn::{AuthDenied, AuthOk, ControlMessage, Disconnect, Heartbeat};
use bytes::Bytes;
use msgx::Channel;
use msgx::channel::{Receiver, Sender};
use quic_link::{
    KeepaliveConfig, LoopControl, PacketSink, Server, Session, forward, keepalive_loop,
};
use shutdown::Shutdown;
use shutdown::ShutdownHandle;
use sysprobe::proto::TelemetryMessage;
use sysprobe::sink::ConsoleSink;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;

const TELEMETRY_ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);

/// 每连接 supervisor 的退出原因（"遗言"契约）。纯枚举，不携带错误信息。
///
/// 与客户端 `ExitCause` 的差异：服务端没有 `Downlink`（下行是全局泵，非 per-conn）、
/// 没有 `HeartbeatEnded`/`ServerDisconnect`（用 `keepalive_loop` 的归并 `CtrlEnded` 表达）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnExitCause {
    ServerShutdown,
    CtrlEnded,
    UplinkEnded,
    TelemetryEnded,
    TaskPanicked,
}

impl ConnExitCause {
    pub const ALL: [Self; 5] = [
        Self::ServerShutdown,
        Self::CtrlEnded,
        Self::UplinkEnded,
        Self::TelemetryEnded,
        Self::TaskPanicked,
    ];

    pub fn code(self) -> u64 {
        match self {
            Self::UplinkEnded | Self::CtrlEnded => 0x1,
            Self::TaskPanicked => 0x2,
            Self::ServerShutdown | Self::TelemetryEnded => 0,
        }
    }

    pub fn reason(self) -> &'static [u8] {
        match self {
            Self::ServerShutdown => b"server-shutdown",
            Self::CtrlEnded => b"ctrl-ended",
            Self::UplinkEnded => b"uplink-ended",
            Self::TelemetryEnded => b"telemetry-ended",
            Self::TaskPanicked => b"conn-panic",
        }
    }
}

impl std::fmt::Display for ConnExitCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ServerShutdown => write!(f, "server-shutdown"),
            Self::CtrlEnded => write!(f, "ctrl-ended"),
            Self::UplinkEnded => write!(f, "uplink-ended"),
            Self::TelemetryEnded => write!(f, "telemetry-ended"),
            Self::TaskPanicked => write!(f, "conn-panic"),
        }
    }
}

pub struct ConnectionHandle {
    id: usize,
    pub session: Session,
    pub ip: Ipv4Addr,
    pub telemetry_tx: TelemetryTxSlot,
    pub(crate) retire_slot: Arc<std::sync::Mutex<Option<ReservedIp>>>,
}

impl std::fmt::Debug for ConnectionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionHandle")
            .field("id", &self.id)
            .field("ip", &self.ip)
            .finish_non_exhaustive()
    }
}

impl ConnectionHandle {
    pub fn new(session: Session, ip: Ipv4Addr) -> Self {
        let id = session.id();
        Self {
            id,
            session,
            ip,
            telemetry_tx: make_telemetry_tx_slot(),
            retire_slot: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub async fn request_collect(
        &self,
        kinds: Vec<sysprobe::proto::InfoKind>,
    ) -> Result<(), crate::telemetry::TelemetryError> {
        crate::telemetry::request_collect(&self.telemetry_tx, kinds).await
    }
}

impl Clone for ConnectionHandle {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            session: self.session.clone(),
            ip: self.ip,
            telemetry_tx: self.telemetry_tx.clone(),
            retire_slot: self.retire_slot.clone(),
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

/// 启动参数（只读快照）。按需 clone 给每个连接。
pub struct BootParams {
    pub config: Arc<ServerConfig>,
}

/// 认证存储（只读共享）。
pub struct AuthStore {
    pub users: UserStore,
}

pub struct ServerRuntime {
    pub ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    pub auth: Arc<AuthStore>,
    pub boot: Arc<BootParams>,
    pub telemetry: Arc<TelemetryPlane>,
}

pub struct RegistryDispatcher {
    pub ledger: Arc<ConnectionLedger<ConnectionHandle>>,
}

impl DownlinkDispatcher for RegistryDispatcher {
    fn dispatch(&self, pkt: Bytes) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let Some(dst) = dst_ipv4_addr(&pkt) else {
                return;
            };
            let Some(handle) = self.ledger.lookup_by_ip(dst) else {
                return;
            };
            let mut tx = handle.session.datagram_tx();
            let _ = tx.send(pkt).await;
        }
    }
}

const FIRST_MSG_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn handle_conn<S: PacketSink + Unpin + Send + 'static>(
    session: Session,
    runtime: Arc<ServerRuntime>,
    uplink_sink: S,
    shutdown: ShutdownHandle,
) -> anyhow::Result<ConnExitCause> {
    let Some((channel, handle, username)) = setup_session(&session, &runtime).await? else {
        return Ok(ConnExitCause::CtrlEnded);
    };
    let supervisor = ConnectionSupervisor::spawn(
        handle,
        runtime.ledger.clone(),
        runtime.telemetry.clone(),
        uplink_sink,
        channel,
        username,
        &shutdown,
    );
    let cause = supervisor.run(&shutdown).await;
    tracing::info!("connection exited: {cause}");
    Ok(cause)
}

/// 每连接 supervisor：集中 spawn ctrl/uplink/telemetry 三个 task，
/// 统一"等待结束信号 → 决定退出原因 → close → drain → cleanup"。
pub struct ConnectionSupervisor {
    session: Session,
    handle: ConnectionHandle,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    tasks: tokio::task::JoinSet<ConnExitCause>,
    drain_sd: Shutdown,
}

impl ConnectionSupervisor {
    pub fn spawn<S: PacketSink + Unpin + Send + 'static>(
        handle: ConnectionHandle,
        ledger: Arc<ConnectionLedger<ConnectionHandle>>,
        telemetry: Arc<TelemetryPlane>,
        uplink_sink: S,
        channel: Channel<ControlMessage>,
        username: String,
        sd: &ShutdownHandle,
    ) -> Self {
        let mut tasks: tokio::task::JoinSet<ConnExitCause> = tokio::task::JoinSet::new();
        let (sender, receiver) = channel.split();
        let session = handle.session.clone();
        let telemetry_tx = handle.telemetry_tx.clone();
        spawn_ctrl_task(&mut tasks, session.clone(), sender, receiver, sd);
        spawn_uplink_task(&mut tasks, uplink_sink, session.clone(), sd);
        spawn_telemetry_task(&mut tasks, session, telemetry, username, telemetry_tx, sd);
        Self {
            session: handle.session.clone(),
            handle,
            ledger,
            tasks,
            drain_sd: Shutdown::new(Duration::from_secs(5)),
        }
    }

    pub async fn run(mut self, global_sd: &ShutdownHandle) -> ConnExitCause {
        let cause = self.await_cause(global_sd).await;
        let close_after_drain = cause == ConnExitCause::ServerShutdown;
        if !close_after_drain {
            self.session.close(cause.code(), cause.reason());
        }
        self.drain_sd.drain(&mut self.tasks, "conn").await;
        if close_after_drain {
            self.session.close(cause.code(), cause.reason());
        }
        let reserved = self
            .handle
            .retire_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.ledger.retire(&self.handle, reserved);
        cause
    }

    async fn await_cause(&mut self, global_sd: &ShutdownHandle) -> ConnExitCause {
        loop {
            // cancel-safety: global_sd.cancelled() 和 tasks.join_next() 均 cancel-safe（tokio 文档）。
            let cause = tokio::select! {
                biased;
                () = global_sd.cancelled() => ConnExitCause::ServerShutdown,
                Some(r) = self.tasks.join_next() => match r {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("conn task panicked: {e}");
                        ConnExitCause::TaskPanicked
                    }
                },
            };
            if cause != ConnExitCause::TelemetryEnded {
                return cause;
            }
        }
    }
}

fn spawn_ctrl_task(
    tasks: &mut tokio::task::JoinSet<ConnExitCause>,
    session: Session,
    sender: Sender<ControlMessage>,
    receiver: Receiver<ControlMessage>,
    sd: &ShutdownHandle,
) {
    let sd = sd.clone();
    tasks.spawn(async move { ctrl_task(session, sender, receiver, sd).await });
}

pub fn spawn_uplink_task<S: PacketSink + Unpin + Send + 'static>(
    tasks: &mut tokio::task::JoinSet<ConnExitCause>,
    sink: S,
    session: Session,
    sd: &ShutdownHandle,
) {
    let sd = sd.clone();
    tasks.spawn(async move { uplink_task(sink, session, sd).await });
}

fn spawn_telemetry_task(
    tasks: &mut tokio::task::JoinSet<ConnExitCause>,
    session: Session,
    telemetry: Arc<TelemetryPlane>,
    username: String,
    telemetry_tx: TelemetryTxSlot,
    sd: &ShutdownHandle,
) {
    let sd = sd.clone();
    tasks
        .spawn(async move { telemetry_task(session, telemetry, username, telemetry_tx, sd).await });
}

async fn ctrl_task(
    session: Session,
    mut writer: Sender<ControlMessage>,
    mut reader: Receiver<ControlMessage>,
    shutdown: ShutdownHandle,
) -> ConnExitCause {
    let hb = || ControlMessage {
        msg: Some(Msg::Heartbeat(Heartbeat {})),
    };
    keepalive_loop(
        &session,
        &mut writer,
        &mut reader,
        &shutdown,
        KeepaliveConfig::default(),
        hb,
        |_| LoopControl::Continue,
    )
    .await;
    send_disconnect_on_shutdown(&shutdown, &mut writer).await;
    ConnExitCause::CtrlEnded
}

async fn send_disconnect_on_shutdown(
    shutdown: &ShutdownHandle,
    writer: &mut Sender<ControlMessage>,
) {
    if shutdown.is_cancelled() {
        let _ = writer.send(server_disconnect_msg()).await;
    }
}

async fn uplink_task<S: PacketSink + Unpin + Send>(
    mut sink: S,
    session: Session,
    shutdown: ShutdownHandle,
) -> ConnExitCause {
    let mut source = session.datagram_rx();
    match forward(&mut source, &mut sink, &shutdown).await {
        Ok(()) => {}
        Err(e) => tracing::warn!("uplink ended with error: {e}"),
    }
    ConnExitCause::UplinkEnded
}

async fn telemetry_task(
    session: Session,
    telemetry: Arc<TelemetryPlane>,
    username: String,
    telemetry_tx: TelemetryTxSlot,
    shutdown: ShutdownHandle,
) -> ConnExitCause {
    let Some(channel) = accept_telemetry_channel(&session, &shutdown).await else {
        return ConnExitCause::TelemetryEnded;
    };
    let (writer, reader) = channel.split();
    set_telemetry_sender(&telemetry_tx, writer).await;
    let source = build_sink_source(&session, &username);
    crate::telemetry::server_telemetry_loop(reader, telemetry, source, shutdown).await;
    ConnExitCause::TelemetryEnded
}

async fn accept_telemetry_channel(
    session: &Session,
    shutdown: &ShutdownHandle,
) -> Option<Channel<TelemetryMessage>> {
    // cancel-safety: shutdown.cancelled() 与 timeout+accept_stream 均 cancel-safe。
    tokio::select! {
        biased;
        () = shutdown.cancelled() => None,
        result = tokio::time::timeout(
            TELEMETRY_ACCEPT_TIMEOUT,
            session.accept_stream::<TelemetryMessage>(),
        ) => if let Ok(Ok(ch)) = result {
            Some(ch)
        } else {
            tracing::debug!("telemetry stream not opened within timeout, skipping");
            None
        },
    }
}

async fn setup_session(
    session: &Session,
    runtime: &ServerRuntime,
) -> anyhow::Result<Option<(Channel<ControlMessage>, ConnectionHandle, String)>> {
    let mut channel = session
        .accept_stream::<ControlMessage>()
        .await
        .map_err(|e| anyhow::anyhow!("failed to accept control stream: {e}"))?;
    let Some(req) = recv_auth_request(&mut channel, session).await? else {
        return Ok(None);
    };
    match resolve_auth(runtime, &req) {
        AuthResolution::Denied(reason) => {
            finish_denied(channel, session, reason).await;
            Ok(None)
        }
        AuthResolution::Ok(ip) => {
            finalize_session(runtime, &req.username, ip, session, channel).await
        }
    }
}

async fn finalize_session(
    runtime: &ServerRuntime,
    username: &str,
    ip: Ipv4Addr,
    session: &Session,
    mut channel: Channel<ControlMessage>,
) -> anyhow::Result<Option<(Channel<ControlMessage>, ConnectionHandle, String)>> {
    let Some(handle) = register_session(runtime, username, ip, session) else {
        return Ok(None);
    };
    send_auth_ok(&mut channel, &runtime.boot, ip).await?;
    Ok(Some((channel, handle, username.to_string())))
}

fn register_session(
    runtime: &ServerRuntime,
    username: &str,
    ip: Ipv4Addr,
    session: &Session,
) -> Option<ConnectionHandle> {
    let handle = ConnectionHandle::new(session.clone(), ip);
    match runtime.ledger.register(username, ip, handle.clone()) {
        Ok(None) => Some(handle),
        Ok(Some(evicted)) => {
            deliver_reserved_and_close(evicted);
            Some(handle)
        }
        Err(_) => {
            session.close(0, b"internal-error");
            None
        }
    }
}

fn deliver_reserved_and_close(evicted: Evicted<ConnectionHandle>) {
    install_retire_guard(&evicted.handle, evicted.reserved);
    evicted.handle.session.close(0, b"superseded");
}

fn install_retire_guard(handle: &ConnectionHandle, reserved: ReservedIp) {
    *handle
        .retire_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reserved);
}

async fn recv_auth_request(
    channel: &mut Channel<ControlMessage>,
    session: &Session,
) -> anyhow::Result<Option<crate::vpn::AuthRequest>> {
    let first = channel
        .recv_timeout(FIRST_MSG_TIMEOUT)
        .await
        .map_err(|e| anyhow::anyhow!("failed to receive first message: {e}"))?;
    if let Some(Msg::AuthRequest(req)) = first.msg {
        Ok(Some(req))
    } else {
        session.close(0, b"protocol-error");
        Ok(None)
    }
}

enum AuthResolution {
    Ok(Ipv4Addr),
    Denied(crate::vpn::DenyReason),
}

fn resolve_auth(runtime: &ServerRuntime, req: &crate::vpn::AuthRequest) -> AuthResolution {
    let result = ctrl::authenticate(&runtime.auth.users, req, || runtime.ledger.alloc());
    match result {
        Ok(ip) => AuthResolution::Ok(ip),
        Err(e) => AuthResolution::Denied(deny_reason_from(&e)),
    }
}

const AUTH_DENY_CONFIRM: Duration = Duration::from_secs(1);

async fn finish_denied(
    mut channel: Channel<ControlMessage>,
    session: &Session,
    reason: crate::vpn::DenyReason,
) {
    let deny = ControlMessage {
        msg: Some(Msg::AuthDenied(AuthDenied {
            reason: reason as i32,
        })),
    };
    let _ = channel.send(deny).await;
    drop(channel);
    let _ = tokio::time::timeout(AUTH_DENY_CONFIRM, session.closed()).await;
    session.close(0, b"auth-denied");
}

async fn send_auth_ok(
    channel: &mut Channel<ControlMessage>,
    boot: &BootParams,
    ip: Ipv4Addr,
) -> anyhow::Result<()> {
    channel
        .send(build_auth_ok(boot, ip))
        .await
        .map_err(|e| anyhow::anyhow!("failed to send AuthOk: {e}"))
}

fn build_auth_ok(boot: &BootParams, ip: Ipv4Addr) -> ControlMessage {
    let config = &boot.config;
    let gateway = gateway_addr(config.tun_subnet);
    ControlMessage {
        msg: Some(Msg::AuthOk(AuthOk {
            assigned_ip: ip.to_string(),
            subnet: config.tun_subnet.to_string(),
            gateway: gateway.to_string(),
            mtu: u32::from(config.mtu),
            routes: config
                .routes
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        })),
    }
}

fn build_sink_source(session: &Session, username: &str) -> SinkSource {
    SinkSource {
        session_id: session.id() as u64,
        username: username.to_string(),
        virtual_ip: None,
    }
}

async fn set_telemetry_sender(slot: &TelemetryTxSlot, sender: crate::telemetry::TelemetrySender) {
    *slot.lock().await = Some(sender);
}

fn server_disconnect_msg() -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::Disconnect(Disconnect {
            reason: "server-shutdown".to_string(),
        })),
    }
}

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    let server = build_server(&config)?;
    let (runtime, tun) = build_runtime(config)?;
    let sd = Shutdown::new(Duration::from_secs(5));
    let ready = shutdown::spawn_signal_watchdog(sd.clone());
    let _ = ready.await;
    let mut daemon_set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    spawn_downlink(
        tun.clone(),
        runtime.ledger.clone(),
        sd.handle(),
        &mut daemon_set,
    );
    let mut conn_set: tokio::task::JoinSet<ConnExitCause> = tokio::task::JoinSet::new();
    accept_connections(&server, runtime, tun, &sd, &mut conn_set).await;
    server.close();
    sd.drain(&mut conn_set, "server").await;
    sd.drain(&mut daemon_set, "daemon").await;
    Ok(())
}

fn build_runtime(config: ServerConfig) -> anyhow::Result<(Arc<ServerRuntime>, Tun)> {
    let ledger = build_ledger(config.tun_subnet)?;
    let auth = build_auth_store(&config)?;
    let boot = build_boot_params(config);
    let telemetry = build_telemetry_plane();
    let runtime = Arc::new(ServerRuntime {
        ledger,
        auth,
        boot,
        telemetry,
    });
    let tun = Tun(Arc::new(crate::tun_setup::create_tun(
        runtime.boot.config.tun_subnet,
        runtime.boot.config.mtu,
    )?));
    Ok((runtime, tun))
}

fn build_server(config: &ServerConfig) -> anyhow::Result<Server> {
    let server = quic_link::Server::builder()
        .tls_from_files(config.cert.clone(), config.key.clone())
        .build(config.listen)?;
    tracing::info!(
        "listening on {}",
        server.local_addr().unwrap_or(config.listen)
    );
    Ok(server)
}

fn build_boot_params(config: ServerConfig) -> Arc<BootParams> {
    Arc::new(BootParams {
        config: Arc::new(config),
    })
}

fn build_auth_store(config: &ServerConfig) -> anyhow::Result<Arc<AuthStore>> {
    let user_pairs: Vec<(String, String)> = config
        .users
        .iter()
        .map(|u| (u.username.clone(), u.password_hash.clone()))
        .collect();
    let users = UserStore::from_users(user_pairs)?;
    Ok(Arc::new(AuthStore { users }))
}

fn build_ledger(subnet: ipnet::Ipv4Net) -> anyhow::Result<Arc<ConnectionLedger<ConnectionHandle>>> {
    Ok(Arc::new(ConnectionLedger::new(subnet)?))
}

fn build_telemetry_plane() -> Arc<TelemetryPlane> {
    Arc::new(TelemetryPlane::new(vec![
        Arc::new(ConsoleSink) as Arc<dyn TelemetrySink>
    ]))
}

fn spawn_downlink(
    mut tun: Tun,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    shutdown: ShutdownHandle,
    daemon_set: &mut tokio::task::JoinSet<()>,
) {
    let dispatcher = RegistryDispatcher { ledger };
    daemon_set.spawn(async move {
        let _ = downlink_pump(&mut tun, &dispatcher, &shutdown).await;
    });
}

async fn accept_connections(
    server: &Server,
    runtime: Arc<ServerRuntime>,
    tun: Tun,
    sd: &Shutdown,
    conn_set: &mut tokio::task::JoinSet<ConnExitCause>,
) {
    let handle = sd.handle();
    run_accept_loop(server, runtime, tun, &handle, conn_set).await;
    tracing::info!("initiating graceful shutdown");
    sd.trigger();
}

async fn run_accept_loop(
    server: &Server,
    runtime: Arc<ServerRuntime>,
    tun: Tun,
    shutdown: &ShutdownHandle,
    conn_set: &mut tokio::task::JoinSet<ConnExitCause>,
) {
    loop {
        if !accept_one(server, runtime.clone(), tun.clone(), shutdown, conn_set).await {
            break;
        }
    }
}

async fn accept_one(
    server: &Server,
    runtime: Arc<ServerRuntime>,
    tun: Tun,
    shutdown: &ShutdownHandle,
    conn_set: &mut tokio::task::JoinSet<ConnExitCause>,
) -> bool {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => false,
        accepted = server.accept() => {
            match accepted {
                Some(Ok(session)) => spawn_handle_conn(session, runtime, tun, shutdown, conn_set),
                Some(Err(e)) => tracing::warn!("connection accept error: {e}"),
                None => return false,
            }
            true
        }
    }
}

fn spawn_handle_conn(
    session: Session,
    runtime: Arc<ServerRuntime>,
    tun: Tun,
    shutdown: &ShutdownHandle,
    conn_set: &mut tokio::task::JoinSet<ConnExitCause>,
) {
    let ct = shutdown.clone();
    conn_set.spawn(async move {
        match handle_conn(session, runtime, tun, ct).await {
            Ok(cause) => cause,
            Err(e) => {
                tracing::error!("connection error: {e}");
                ConnExitCause::CtrlEnded
            }
        }
    });
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::mutable_key_type,
    clippy::indexing_slicing
)]
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

    async fn make_client_sessions(n: usize) -> Vec<Session> {
        let server = build_test_server();
        let client_cfg = build_no_verify_client_config();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = server.local_addr().unwrap();
        let mut sessions = Vec::new();
        for _ in 0..n {
            let conn = client
                .connect_with(client_cfg.clone(), addr, "localhost")
                .expect("dial")
                .await
                .expect("connect");
            sessions.push(Session::new(conn));
        }
        std::mem::forget(client);
        sessions
    }

    fn build_test_server() -> quinn::Endpoint {
        let cert = repo("cert.pem");
        let key = repo("key.pem");
        let server_cfg = quic_link::build_quinn_server_config(&cert, &key).expect("server cfg");
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
        let mut sessions = make_client_sessions(1).await;
        let session = sessions.remove(0);
        let dup = session.clone();
        let h1 = ConnectionHandle::new(session, Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(dup, Ipv4Addr::new(10, 0, 0, 9));
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn test_connection_handle_neq_different_id() {
        let mut sessions = make_client_sessions(2).await;
        let h1 = ConnectionHandle::new(sessions.remove(0), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(sessions.remove(0), Ipv4Addr::new(10, 0, 0, 3));
        assert_ne!(h1.id(), h2.id());
        assert_ne!(h1, h2);
    }

    #[tokio::test]
    async fn test_connection_handle_hash_by_id() {
        let mut sessions = make_client_sessions(1).await;
        let session = sessions.remove(0);
        let h1 = ConnectionHandle::new(session.clone(), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(session, Ipv4Addr::new(10, 0, 0, 9));
        let mut s1 = DefaultHasher::new();
        let mut s2 = DefaultHasher::new();
        h1.hash(&mut s1);
        h2.hash(&mut s2);
        assert_eq!(s1.finish(), s2.finish());
    }

    #[tokio::test]
    async fn test_connection_handle_dedups_in_hashset() {
        let mut sessions = make_client_sessions(1).await;
        let session = sessions.remove(0);
        let h1 = ConnectionHandle::new(session.clone(), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(session, Ipv4Addr::new(10, 0, 0, 3));
        let mut set = HashSet::new();
        set.insert(h1);
        assert!(set.contains(&h2));
        set.insert(h2);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_conn_exit_cause_code_reason_mapping() {
        let cases = [
            (ConnExitCause::ServerShutdown, 0, "server-shutdown"),
            (ConnExitCause::CtrlEnded, 0x1, "ctrl-ended"),
            (ConnExitCause::UplinkEnded, 0x1, "uplink-ended"),
            (ConnExitCause::TelemetryEnded, 0, "telemetry-ended"),
            (ConnExitCause::TaskPanicked, 0x2, "conn-panic"),
        ];
        for (cause, code, reason) in cases {
            assert_eq!(cause.code(), code, "{cause:?}");
            assert_eq!(cause.reason(), reason.as_bytes(), "{cause:?}");
        }
    }

    #[test]
    fn test_conn_exit_cause_displays_are_distinct() {
        let all: Vec<String> = ConnExitCause::ALL.iter().map(ToString::to_string).collect();
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "duplicate at {i},{j}");
            }
        }
    }

    #[test]
    fn test_conn_exit_cause_is_copy_and_eq() {
        let a = ConnExitCause::CtrlEnded;
        let b = a;
        assert_eq!(a, b);
    }
}
