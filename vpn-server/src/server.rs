use std::hash::{Hash, Hasher};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use crate::auth::UserStore;
use crate::config::ServerConfig;
use crate::ctrl::{self, deny_reason_from};
use crate::ledger::{ConnectionLedger, Evicted, ReservedIp};
use crate::telemetry::TelemetryPlane;
use crate::telemetry::TelemetryTxSlot;
use crate::telemetry::make_telemetry_tx_slot;
use bytes::Bytes;
use ipnet::Ipv4Net;
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
use vpn_core::data::{DownlinkDispatcher, Tun, downlink_pump, dst_ipv4_addr};
use vpn_core::tun_setup::gateway_addr;
use vpn_core::vpn::control_message::Msg;
use vpn_core::vpn::{AuthDenied, AuthOk, ControlMessage, Disconnect, Heartbeat};

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

/// 客户端网络画像：认证成功后下发给客户端的 TUN 配置派生。
/// gateway 在 boot 时由 `gateway_addr(tun_subnet)` 预算一次，所有连接共享。
pub struct ClientNetProfile {
    pub subnet: Ipv4Net,
    pub gateway: Ipv4Addr,
    pub mtu: u16,
    pub routes: Vec<Ipv4Net>,
}

fn build_net_profile(config: ServerConfig) -> Arc<ClientNetProfile> {
    Arc::new(ClientNetProfile {
        subnet: config.tun_subnet,
        gateway: gateway_addr(config.tun_subnet),
        mtu: config.mtu,
        routes: config.routes,
    })
}

/// 认证存储（只读共享）。
pub struct AuthStore {
    pub users: UserStore,
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

async fn try_authenticate(
    session: &Session,
    auth: &AuthStore,
    ledger: &ConnectionLedger<ConnectionHandle>,
    profile: &ClientNetProfile,
) -> Option<(ConnectionHandle, String, Channel<ControlMessage>)> {
    let channel = accept_control_stream(session).await?;
    authenticate(session, auth, ledger, profile, channel).await
}

pub async fn handle_conn<S: PacketSink + Unpin + Send + 'static>(
    session: Session,
    auth: Arc<AuthStore>,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    net_profile: Arc<ClientNetProfile>,
    telemetry: Arc<TelemetryPlane>,
    uplink_sink: S,
    sd: ShutdownHandle,
) -> ConnExitCause {
    let Some((handle, username, channel)) =
        try_authenticate(&session, &auth, &ledger, &net_profile).await
    else {
        return ConnExitCause::CtrlEnded;
    };
    let supervisor = ConnectionSupervisor::spawn(
        handle,
        ledger,
        telemetry,
        uplink_sink,
        channel,
        username,
        &sd,
    );
    let cause = supervisor.run(&sd).await;
    tracing::info!("connection exited: {cause}");
    cause
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
        spawn_ctrl_task(&mut tasks, session.clone(), sender, receiver, sd); // 控制面: 心跳保活
        spawn_uplink_task(&mut tasks, uplink_sink, session.clone(), sd); // 数据面上行: datagram → TUN
        spawn_telemetry_task(&mut tasks, session, telemetry, username, telemetry_tx, sd); // 采集面: telemetry
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

async fn accept_control_stream(session: &Session) -> Option<Channel<ControlMessage>> {
    match session.accept_stream::<ControlMessage>().await {
        Ok(ch) => Some(ch),
        Err(e) => {
            tracing::warn!("failed to accept control stream: {e}");
            None
        }
    }
}

async fn authenticate(
    session: &Session,
    auth: &AuthStore,
    ledger: &ConnectionLedger<ConnectionHandle>,
    profile: &ClientNetProfile,
    mut channel: Channel<ControlMessage>,
) -> Option<(ConnectionHandle, String, Channel<ControlMessage>)> {
    let req = recv_auth_request(&mut channel, session).await?;
    match resolve_auth(auth, ledger, &req) {
        AuthResolution::Denied(reason) => {
            finish_denied(channel, session, reason).await;
            None
        }
        AuthResolution::Ok(ip) => {
            establish_session(ledger, profile, &req.username, ip, session, &mut channel)
                .await
                .map(|(handle, username)| (handle, username, channel))
        }
    }
}

async fn establish_session(
    ledger: &ConnectionLedger<ConnectionHandle>,
    profile: &ClientNetProfile,
    username: &str,
    ip: Ipv4Addr,
    session: &Session,
    channel: &mut Channel<ControlMessage>,
) -> Option<(ConnectionHandle, String)> {
    let handle = register_session(ledger, username, ip, session)?;
    if let Err(e) = send_auth_ok(channel, profile, ip).await {
        tracing::warn!("failed to send AuthOk: {e}");
        return None;
    }
    Some((handle, username.to_string()))
}

fn register_session(
    ledger: &ConnectionLedger<ConnectionHandle>,
    username: &str,
    ip: Ipv4Addr,
    session: &Session,
) -> Option<ConnectionHandle> {
    let handle = ConnectionHandle::new(session.clone(), ip);
    match ledger.register(username, ip, handle.clone()) {
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
) -> Option<vpn_core::vpn::AuthRequest> {
    let first = match channel.recv_timeout(FIRST_MSG_TIMEOUT).await {
        Ok(msg) => msg,
        Err(e) => {
            tracing::warn!("failed to receive first message: {e}");
            return None;
        }
    };
    if let Some(Msg::AuthRequest(req)) = first.msg {
        Some(req)
    } else {
        session.close(0, b"protocol-error");
        None
    }
}

enum AuthResolution {
    Ok(Ipv4Addr),
    Denied(vpn_core::vpn::DenyReason),
}

fn resolve_auth(
    auth: &AuthStore,
    ledger: &ConnectionLedger<ConnectionHandle>,
    req: &vpn_core::vpn::AuthRequest,
) -> AuthResolution {
    let result = ctrl::authenticate(&auth.users, req, || ledger.alloc());
    match result {
        Ok(ip) => AuthResolution::Ok(ip),
        Err(e) => AuthResolution::Denied(deny_reason_from(&e)),
    }
}

const AUTH_DENY_CONFIRM: Duration = Duration::from_secs(1);

async fn finish_denied(
    mut channel: Channel<ControlMessage>,
    session: &Session,
    reason: vpn_core::vpn::DenyReason,
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
    profile: &ClientNetProfile,
    ip: Ipv4Addr,
) -> anyhow::Result<()> {
    channel
        .send(build_auth_ok(profile, ip))
        .await
        .map_err(|e| anyhow::anyhow!("failed to send AuthOk: {e}"))
}

fn build_auth_ok(profile: &ClientNetProfile, ip: Ipv4Addr) -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::AuthOk(AuthOk {
            assigned_ip: ip.to_string(),
            subnet: profile.subnet.to_string(),
            gateway: profile.gateway.to_string(),
            mtu: u32::from(profile.mtu),
            routes: profile
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

pub struct DownlinkDaemon {
    tasks: tokio::task::JoinSet<()>,
}

impl DownlinkDaemon {
    pub fn spawn(
        mut tun: Tun,
        ledger: Arc<ConnectionLedger<ConnectionHandle>>,
        shutdown: ShutdownHandle,
    ) -> Self {
        let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        let dispatcher = RegistryDispatcher { ledger };
        tasks.spawn(async move {
            let _ = downlink_pump(&mut tun, &dispatcher, &shutdown).await;
        });
        Self { tasks }
    }

    pub async fn drain(&mut self, sd: &Shutdown) {
        sd.drain(&mut self.tasks, "daemon").await;
    }
}

pub struct AcceptLoop {
    endpoint: Server,
    tun: Tun,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    auth: Arc<AuthStore>,
    net_profile: Arc<ClientNetProfile>,
    telemetry: Arc<TelemetryPlane>,
    conn_set: tokio::task::JoinSet<ConnExitCause>,
}

impl AcceptLoop {
    pub fn new(
        endpoint: Server,
        tun: Tun,
        ledger: Arc<ConnectionLedger<ConnectionHandle>>,
        auth: Arc<AuthStore>,
        net_profile: Arc<ClientNetProfile>,
        telemetry: Arc<TelemetryPlane>,
    ) -> Self {
        Self {
            endpoint,
            tun,
            ledger,
            auth,
            net_profile,
            telemetry,
            conn_set: tokio::task::JoinSet::new(),
        }
    }

    pub async fn serve(&mut self, sd: &ShutdownHandle) {
        loop {
            // cancel-safety: sd.cancelled() 与 endpoint.accept() 均 cancel-safe。
            tokio::select! {
                biased;
                () = sd.cancelled() => break,
                accepted = self.endpoint.accept() => match accepted {
                    None => break,
                    Some(Err(e)) => tracing::warn!("connection accept error: {e}"),
                    Some(Ok(session)) => self.spawn_conn(session, sd),
                }
            }
        }
    }

    fn spawn_conn(&mut self, session: Session, sd: &ShutdownHandle) {
        self.conn_set.spawn(handle_conn(
            session,
            self.auth.clone(),
            self.ledger.clone(),
            self.net_profile.clone(),
            self.telemetry.clone(),
            self.tun.clone(),
            sd.clone(),
        ));
    }

    pub fn close(&self) {
        self.endpoint.close();
    }

    pub async fn drain(&mut self, sd: &Shutdown) {
        sd.drain(&mut self.conn_set, "server").await;
    }
}

pub struct VpnServer {
    tun: Tun,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    accept: AcceptLoop,
    daemon: Option<DownlinkDaemon>,
    sd: Shutdown,
}

impl VpnServer {
    pub fn boot(config: ServerConfig) -> anyhow::Result<Self> {
        let (accept, tun, ledger) = build_accept(config)?;
        let sd = Shutdown::new(Duration::from_secs(5));
        Ok(Self {
            tun,
            ledger,
            accept,
            daemon: None,
            sd,
        })
    }
    pub async fn run(mut self) -> anyhow::Result<()> {
        let _ = shutdown::spawn_signal_watchdog(self.sd.clone()).await;
        let sd_handle = self.sd.handle();
        self.daemon = Some(DownlinkDaemon::spawn(
            self.tun.clone(),
            self.ledger.clone(),
            sd_handle.clone(),
        ));
        self.accept.serve(&sd_handle).await;
        self.graceful_stop().await;
        Ok(())
    }

    async fn graceful_stop(&mut self) {
        tracing::info!("initiating graceful shutdown");
        self.sd.trigger();
        self.accept.close();
        self.accept.drain(&self.sd).await;
        if let Some(daemon) = &mut self.daemon {
            daemon.drain(&self.sd).await;
        }
    }
}

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    VpnServer::boot(config)?.run().await
}

fn build_accept(
    config: ServerConfig,
) -> anyhow::Result<(AcceptLoop, Tun, Arc<ConnectionLedger<ConnectionHandle>>)> {
    let endpoint = build_server(&config)?;
    let auth = build_auth_store(&config)?;
    let ledger = build_ledger(config.tun_subnet)?;
    let tun = create_tun(config.tun_subnet, config.mtu)?;
    let net_profile = build_net_profile(config);
    let telemetry = build_telemetry_plane();
    let accept = AcceptLoop::new(
        endpoint,
        tun.clone(),
        ledger.clone(),
        auth,
        net_profile,
        telemetry,
    );
    Ok((accept, tun, ledger))
}

fn create_tun(tun_subnet: Ipv4Net, mtu: u16) -> anyhow::Result<Tun> {
    Ok(Tun(Arc::new(vpn_core::tun_setup::create_tun(
        tun_subnet, mtu,
    )?)))
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

    #[tokio::test]
    async fn test_graceful_stop_drains_conns_before_daemon() {
        let sd = Shutdown::new(Duration::from_secs(5));
        let mut conn_set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        let mut daemon_set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        let log: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        spawn_order_probe(&mut conn_set, sd.handle(), log.clone(), 1);
        spawn_order_probe(&mut daemon_set, sd.handle(), log.clone(), 2);
        sd.trigger();
        sd.drain(&mut conn_set, "conn").await;
        sd.drain(&mut daemon_set, "daemon").await;
        let recorded = log.lock().unwrap();
        assert_eq!(*recorded, vec![1u8, 2u8], "conns must drain before daemon");
    }

    fn spawn_order_probe(
        tasks: &mut tokio::task::JoinSet<()>,
        sd: ShutdownHandle,
        log: Arc<std::sync::Mutex<Vec<u8>>>,
        tag: u8,
    ) {
        tasks.spawn(async move {
            sd.cancelled().await;
            log.lock().unwrap().push(tag);
        });
    }

    fn server_config_for(subnet: Ipv4Net, mtu: u16, routes: Vec<Ipv4Net>) -> ServerConfig {
        ServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tun_subnet: subnet,
            mtu,
            cert: std::path::PathBuf::new(),
            key: std::path::PathBuf::new(),
            routes,
            users: vec![],
        }
    }

    fn net_profile_for(subnet: Ipv4Net, mtu: u16, routes: Vec<Ipv4Net>) -> ClientNetProfile {
        ClientNetProfile {
            subnet,
            gateway: gateway_addr(subnet),
            mtu,
            routes,
        }
    }

    #[test]
    fn test_build_net_profile_projects_config_and_precomputes_gateway() {
        let subnet = Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap();
        let routes = vec![Ipv4Net::new(Ipv4Addr::new(192, 168, 100, 0), 24).unwrap()];
        let config = server_config_for(subnet, 1280, routes.clone());
        let profile = build_net_profile(config);
        assert_eq!(profile.subnet, subnet);
        assert_eq!(profile.gateway, gateway_addr(subnet));
        assert_eq!(profile.gateway, Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(profile.mtu, 1280);
        assert_eq!(profile.routes, routes);
    }

    #[test]
    fn test_build_auth_ok_with_profile_projects_all_fields() {
        let subnet = Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap();
        let routes = vec![
            Ipv4Net::new(Ipv4Addr::new(192, 168, 100, 0), 24).unwrap(),
            Ipv4Net::new(Ipv4Addr::new(10, 88, 0, 0), 16).unwrap(),
        ];
        let profile = net_profile_for(subnet, 1280, routes.clone());
        let msg = build_auth_ok(&profile, Ipv4Addr::new(10, 0, 0, 5));
        let Msg::AuthOk(ok) = msg.msg.expect("AuthOk") else {
            panic!("expected AuthOk");
        };
        assert_eq!(ok.assigned_ip, "10.0.0.5");
        assert_eq!(ok.subnet, "10.0.0.0/24");
        assert_eq!(ok.gateway, "10.0.0.1");
        assert_eq!(ok.mtu, 1280);
        assert_eq!(ok.routes.len(), 2);
        assert_eq!(ok.routes[0], "192.168.100.0/24");
        assert_eq!(ok.routes[1], "10.88.0.0/16");
    }
}
