use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, anyhow};
use ipnet::Ipv4Net;
use thiserror::Error;

use crate::config::ClientConfig;
use crate::config::MIN_MTU;
use crate::data::{PacketSink, PacketSource, Tun, forward};
use crate::vpn::AuthOk;
use crate::vpn::ControlMessage;
use crate::vpn::control_message::Msg;
use msgx::Channel;
use msgx::channel::{Receiver, Sender};
use quic_link::{KeepaliveConfig, LoopControl, Session, keepalive_loop};
use shutdown::Shutdown;
use shutdown::ShutdownHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTunParams {
    pub assigned_ip: Ipv4Addr,
    pub subnet: Ipv4Net,
    pub gateway: Ipv4Addr,
    pub mtu: u16,
    pub routes: Vec<Ipv4Net>,
}

/// 数据面 task 的结束原因（"遗言"契约）。纯枚举，不携带错误信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCause {
    Interrupted,
    ServerDisconnect,
    HeartbeatEnded,
    UplinkEnded,
    DownlinkEnded,
    TelemetryEnded,
    TaskPanicked,
}

impl ExitCause {
    pub const ALL: [Self; 7] = [
        Self::Interrupted,
        Self::ServerDisconnect,
        Self::HeartbeatEnded,
        Self::UplinkEnded,
        Self::DownlinkEnded,
        Self::TelemetryEnded,
        Self::TaskPanicked,
    ];

    pub fn code(self) -> u64 {
        match self {
            Self::UplinkEnded | Self::DownlinkEnded => 0x1,
            Self::TaskPanicked => 0x2,
            Self::Interrupted
            | Self::ServerDisconnect
            | Self::HeartbeatEnded
            | Self::TelemetryEnded => 0,
        }
    }

    pub fn reason(self) -> &'static [u8] {
        match self {
            Self::Interrupted => b"client-shutdown",
            Self::ServerDisconnect => b"server-disconnect",
            Self::HeartbeatEnded => b"heartbeat-timeout",
            Self::UplinkEnded => b"uplink-ended",
            Self::DownlinkEnded => b"downlink-ended",
            Self::TelemetryEnded => b"telemetry-ended",
            Self::TaskPanicked => b"data-plane-panic",
        }
    }
}

impl std::fmt::Display for ExitCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupted => write!(f, "interrupted"),
            Self::ServerDisconnect => write!(f, "server-disconnect"),
            Self::HeartbeatEnded => write!(f, "heartbeat-ended"),
            Self::UplinkEnded => write!(f, "uplink-ended"),
            Self::DownlinkEnded => write!(f, "downlink-ended"),
            Self::TelemetryEnded => write!(f, "telemetry-ended"),
            Self::TaskPanicked => write!(f, "task-panicked"),
        }
    }
}

/// 已认证客户端，持有连接生命周期。
///
/// 字段按声明顺序析构：`session` 先、`endpoint` 最后，保证 Endpoint 活得比
/// 所有使用 Session 的 task 更久。
pub struct EstablishedClient {
    session: Session,
    channel: Channel<ControlMessage>,
    params: ClientTunParams,
    #[allow(dead_code)]
    endpoint: quic_link::Client,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("AuthOk contains invalid assigned_ip: {0}")]
    InvalidAssignedIp(String),
    #[error("AuthOk contains invalid gateway: {0}")]
    InvalidGateway(String),
    #[error("AuthOk contains invalid subnet: {0}")]
    InvalidSubnet(String),
    #[error("AuthOk mtu {0} is smaller than minimum {MIN_MTU}")]
    MtuTooSmall(u32),
    #[error("AuthOk mtu {0} exceeds maximum 65535")]
    MtuTooLarge(u32),
    #[error("AuthOk gateway {0} is not inside subnet {1}")]
    GatewayOutsideSubnet(Ipv4Addr, Ipv4Net),
    #[error("AuthOk gateway {0} equals the subnet network address")]
    GatewayIsNetworkAddr(Ipv4Addr),
    #[error("AuthOk contains invalid route CIDR: {0}")]
    InvalidRoute(String),
    #[error("authentication failed: {0}")]
    AuthDenied(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

pub fn parse_auth_ok(ok: &AuthOk) -> Result<ClientTunParams, ClientError> {
    let (assigned_ip, gateway, subnet) = parse_endpoint_addrs(ok)?;
    let mtu = validate_mtu(ok.mtu)?;
    validate_gateway(gateway, subnet)?;
    let routes = parse_routes(&ok.routes)?;
    Ok(ClientTunParams {
        assigned_ip,
        subnet,
        gateway,
        mtu,
        routes,
    })
}

fn parse_endpoint_addrs(ok: &AuthOk) -> Result<(Ipv4Addr, Ipv4Addr, Ipv4Net), ClientError> {
    let assigned_ip: Ipv4Addr = ok
        .assigned_ip
        .parse()
        .map_err(|_| ClientError::InvalidAssignedIp(ok.assigned_ip.clone()))?;
    let gateway: Ipv4Addr = ok
        .gateway
        .parse()
        .map_err(|_| ClientError::InvalidGateway(ok.gateway.clone()))?;
    let subnet: Ipv4Net = ok
        .subnet
        .parse()
        .map_err(|_| ClientError::InvalidSubnet(ok.subnet.clone()))?;
    Ok((assigned_ip, gateway, subnet))
}

fn validate_mtu(raw: u32) -> Result<u16, ClientError> {
    if raw < u32::from(MIN_MTU) {
        return Err(ClientError::MtuTooSmall(raw));
    }
    if raw > u32::from(u16::MAX) {
        return Err(ClientError::MtuTooLarge(raw));
    }
    u16::try_from(raw).map_err(|_| ClientError::MtuTooLarge(raw))
}

fn validate_gateway(gateway: Ipv4Addr, subnet: Ipv4Net) -> Result<(), ClientError> {
    if !subnet.contains(&gateway) {
        return Err(ClientError::GatewayOutsideSubnet(gateway, subnet));
    }
    if gateway == subnet.network() {
        return Err(ClientError::GatewayIsNetworkAddr(gateway));
    }
    Ok(())
}

fn parse_routes(raw: &[String]) -> Result<Vec<Ipv4Net>, ClientError> {
    let mut routes = Vec::with_capacity(raw.len());
    for r in raw {
        let net: Ipv4Net = r
            .parse()
            .map_err(|_| ClientError::InvalidRoute(r.clone()))?;
        routes.push(net);
    }
    Ok(routes)
}

fn deny_reason_text(reason: i32) -> &'static str {
    match reason {
        r if r == crate::vpn::DenyReason::AuthFailed as i32 => "认证失败（用户名或密码错误）",
        r if r == crate::vpn::DenyReason::ServerBusy as i32 => "服务端繁忙（IP 池耗尽）",
        _ => "未知拒绝原因",
    }
}

pub async fn run(config: ClientConfig) -> anyhow::Result<()> {
    let sd = Shutdown::new(Duration::from_secs(5));
    let ready = shutdown::spawn_signal_watchdog(sd.clone());
    let _ = ready.await;
    let username = read_username().await?;
    let password = read_password().await?;
    run_with_credentials(config, username, password, sd).await
}

async fn read_username() -> anyhow::Result<String> {
    let raw = tokio::task::spawn_blocking(|| rpassword::prompt_password("请输入用户名："))
        .await
        .context("username prompt task panicked")??;
    validate_username(&raw)
}

fn validate_username(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("用户名不能为空");
    }
    Ok(trimmed.to_string())
}

async fn read_password() -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || rpassword::prompt_password("请输入密码："))
        .await
        .context("password prompt task panicked")?
        .map_err(Into::into)
}

pub async fn run_with_credentials(
    config: ClientConfig,
    username: String,
    password: String,
    sd: Shutdown,
) -> anyhow::Result<()> {
    let est = connect_and_auth(&config, &username, password).await?;
    let tun = setup_tun(&est.params)?;
    tracing::info!(
        "authenticated as {}, assigned_ip={}, subnet={}, mtu={}",
        username,
        est.params.assigned_ip,
        est.params.subnet,
        est.params.mtu
    );
    let plane = DataPlane::spawn(est.session.clone(), Tun(tun), est.channel, &sd);
    let cause = plane.run(sd).await;
    tracing::info!("client exited: {cause}");
    Ok(())
}

async fn connect_and_auth(
    config: &ClientConfig,
    username: &str,
    password: String,
) -> anyhow::Result<EstablishedClient> {
    let client = quic_link::Client::builder()
        .trust_ca(config.ca_cert.clone())
        .server_name(config.server_name.clone())
        .build()
        .context("failed to build client")?;
    let session = client.connect(config.server).await?;
    tracing::info!("connected to {}", config.server);
    let mut channel = open_control_stream(&session).await?;
    let params = authenticate(&mut channel, username, password).await?;
    Ok(EstablishedClient {
        session,
        channel,
        params,
        endpoint: client,
    })
}

async fn open_control_stream(session: &Session) -> anyhow::Result<Channel<ControlMessage>> {
    session
        .open_stream::<ControlMessage>()
        .await
        .context("failed to open control stream")
}

async fn authenticate(
    channel: &mut Channel<ControlMessage>,
    username: &str,
    password: String,
) -> anyhow::Result<ClientTunParams> {
    send_auth_request(channel, username, password).await?;
    let first = channel
        .recv()
        .await
        .map_err(|e| anyhow!("failed to decode first response: {e}"))?
        .ok_or_else(|| anyhow!("control stream closed before AuthOk"))?;
    interpret_auth_response(first)
}

async fn send_auth_request(
    channel: &mut Channel<ControlMessage>,
    username: &str,
    password: String,
) -> anyhow::Result<()> {
    channel
        .send(ControlMessage {
            msg: Some(Msg::AuthRequest(crate::vpn::AuthRequest {
                username: username.to_string(),
                password,
            })),
        })
        .await
        .context("failed to send AuthRequest")
}

fn interpret_auth_response(first: ControlMessage) -> anyhow::Result<ClientTunParams> {
    match first.msg {
        Some(Msg::AuthOk(ok)) => parse_auth_ok(&ok).map_err(Into::into),
        Some(Msg::AuthDenied(denied)) => {
            let reason = deny_reason_text(denied.reason);
            tracing::error!("{reason}");
            Err(ClientError::AuthDenied(reason.to_string()).into())
        }
        _ => Err(
            ClientError::Protocol("expected AuthOk but got an unexpected message".into()).into(),
        ),
    }
}

fn setup_tun(params: &ClientTunParams) -> anyhow::Result<std::sync::Arc<tun_rs::AsyncDevice>> {
    let tun = crate::tun_setup::create_client_tun(params.assigned_ip, params.subnet, params.mtu)
        .context("failed to create client TUN device")?;
    let dev_name = tun.name().unwrap_or_default();
    crate::route::ensure_subnet_route(&dev_name, params.subnet)
        .context("failed to configure subnet route")?;
    crate::route::add_routes(&dev_name, &params.routes).context("failed to add extra routes")?;
    Ok(std::sync::Arc::new(tun))
}

pub async fn heartbeat_loop(
    session: Session,
    mut reader: Receiver<ControlMessage>,
    mut writer: Sender<ControlMessage>,
    shutdown: ShutdownHandle,
) -> ExitCause {
    let mut saw_disconnect = false;
    run_keepalive(
        &session,
        &mut reader,
        &mut writer,
        &shutdown,
        &mut saw_disconnect,
    )
    .await;
    resolve_cause(saw_disconnect, &shutdown)
}

async fn run_keepalive(
    session: &Session,
    reader: &mut Receiver<ControlMessage>,
    writer: &mut Sender<ControlMessage>,
    shutdown: &ShutdownHandle,
    saw_disconnect: &mut bool,
) {
    let hb = || ControlMessage {
        msg: Some(Msg::Heartbeat(crate::vpn::Heartbeat {})),
    };
    keepalive_loop(
        session,
        writer,
        reader,
        shutdown,
        KeepaliveConfig::default(),
        hb,
        disconnect_handler(saw_disconnect),
    )
    .await;
}

fn disconnect_handler(
    saw_disconnect: &mut bool,
) -> impl FnMut(&ControlMessage) -> LoopControl + '_ {
    move |m| {
        if matches!(m.msg, Some(Msg::Disconnect(_))) {
            tracing::info!("server disconnected");
            *saw_disconnect = true;
            LoopControl::Break
        } else {
            LoopControl::Continue
        }
    }
}

fn resolve_cause(saw_disconnect: bool, shutdown: &ShutdownHandle) -> ExitCause {
    if saw_disconnect {
        ExitCause::ServerDisconnect
    } else if shutdown.is_cancelled() {
        ExitCause::Interrupted
    } else {
        ExitCause::HeartbeatEnded
    }
}

/// 数据面 supervisor：集中 spawn 心跳/上行/下行/遥测 task，并统一关闭协调。
pub struct DataPlane<S> {
    session: Session,
    tasks: tokio::task::JoinSet<ExitCause>,
    _tun: std::marker::PhantomData<S>,
}

impl<S> DataPlane<S>
where
    S: PacketSource + PacketSink + Clone + Unpin + Send + Sync + 'static,
{
    pub fn spawn(
        session: Session,
        tun: S,
        channel: Channel<ControlMessage>,
        sd: &Shutdown,
    ) -> Self {
        let (writer, reader) = channel.split();
        let mut tasks: tokio::task::JoinSet<ExitCause> = tokio::task::JoinSet::new();
        spawn_heartbeat(&mut tasks, session.clone(), reader, writer, sd);
        spawn_uplink_task(&mut tasks, session.clone(), tun.clone(), sd);
        spawn_downlink_task(&mut tasks, session.clone(), tun, sd);
        spawn_telemetry_task(&mut tasks, session.clone(), sd);
        Self {
            session,
            tasks,
            _tun: std::marker::PhantomData,
        }
    }

    pub async fn run(mut self, sd: Shutdown) -> ExitCause {
        let cause = self.await_cause(&sd).await;
        self.session.close(cause.code(), cause.reason());
        sd.trigger();
        sd.drain(&mut self.tasks, "client").await;
        cause
    }

    async fn await_cause(&mut self, sd: &Shutdown) -> ExitCause {
        loop {
            let cause = tokio::select! {
                biased;
                () = sd.triggered() => ExitCause::Interrupted,
                Some(r) = self.tasks.join_next() => match r {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("data plane task panicked: {e}");
                        ExitCause::TaskPanicked
                    }
                },
            };
            if cause != ExitCause::TelemetryEnded {
                return cause;
            }
        }
    }
}

async fn uplink<S>(session: Session, tun: S, cancel: ShutdownHandle) -> ExitCause
where
    S: PacketSource + Unpin,
{
    let mut source = tun;
    let mut sink = session.datagram_tx();
    match forward(&mut source, &mut sink, &cancel).await {
        Ok(()) => ExitCause::UplinkEnded,
        Err(e) => {
            tracing::warn!("uplink ended with error: {e}");
            ExitCause::UplinkEnded
        }
    }
}

fn spawn_heartbeat(
    tasks: &mut tokio::task::JoinSet<ExitCause>,
    session: Session,
    reader: Receiver<ControlMessage>,
    writer: Sender<ControlMessage>,
    sd: &Shutdown,
) {
    let handle = sd.handle();
    tasks.spawn(async move { heartbeat_loop(session, reader, writer, handle).await });
}

fn spawn_uplink_task<S>(
    tasks: &mut tokio::task::JoinSet<ExitCause>,
    session: Session,
    tun: S,
    sd: &Shutdown,
) where
    S: PacketSource + PacketSink + Clone + Unpin + Send + Sync + 'static,
{
    let handle = sd.handle();
    tasks.spawn(async move { uplink(session, tun, handle).await });
}

fn spawn_downlink_task<S>(
    tasks: &mut tokio::task::JoinSet<ExitCause>,
    session: Session,
    tun: S,
    sd: &Shutdown,
) where
    S: PacketSource + PacketSink + Clone + Unpin + Send + Sync + 'static,
{
    let handle = sd.handle();
    tasks.spawn(async move { downlink(session, tun, handle).await });
}

fn spawn_telemetry_task(
    tasks: &mut tokio::task::JoinSet<ExitCause>,
    session: Session,
    sd: &Shutdown,
) {
    let handle = sd.handle();
    tasks.spawn(async move { crate::telemetry::run_client_telemetry(session, handle).await });
}

async fn downlink<S>(session: Session, tun: S, cancel: ShutdownHandle) -> ExitCause
where
    S: PacketSink + Unpin,
{
    let mut source = session.datagram_rx();
    let mut sink = tun;
    match forward(&mut source, &mut sink, &cancel).await {
        Ok(()) => ExitCause::DownlinkEnded,
        Err(e) => {
            tracing::warn!("downlink ended with error: {e}");
            ExitCause::DownlinkEnded
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names
)]
mod tests {
    use super::*;

    fn auth_ok() -> AuthOk {
        AuthOk {
            assigned_ip: "10.0.0.2".to_string(),
            subnet: "10.0.0.0/24".to_string(),
            gateway: "10.0.0.1".to_string(),
            mtu: 1280,
            routes: vec![],
        }
    }

    fn repo(p: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("vpn crate nested under repo root")
            .join(p)
    }

    #[tokio::test]
    async fn test_spawn_signal_watchdog_cancels_on_sigint() {
        let sd = Shutdown::new(Duration::from_secs(5));
        let ready = shutdown::spawn_signal_watchdog(sd.clone());
        ready
            .await
            .expect("watchdog should finish registering the SIGINT handler");
        unsafe {
            libc::kill(libc::getpid(), libc::SIGINT);
        }
        tokio::time::timeout(Duration::from_secs(3), sd.triggered())
            .await
            .expect("watchdog should trigger the Shutdown when SIGINT is received");
    }

    #[test]
    fn test_parse_auth_ok_when_valid_returns_params() {
        let params = parse_auth_ok(&auth_ok()).unwrap();
        assert_eq!(params.assigned_ip, Ipv4Addr::new(10, 0, 0, 2));
        assert_eq!(params.subnet, "10.0.0.0/24".parse::<Ipv4Net>().unwrap());
        assert_eq!(params.gateway, Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(params.mtu, 1280);
        assert!(params.routes.is_empty());
    }

    #[test]
    fn test_parse_auth_ok_when_invalid_assigned_ip_returns_err() {
        let mut ok = auth_ok();
        ok.assigned_ip = "not-an-ip".to_string();
        assert!(matches!(
            parse_auth_ok(&ok),
            Err(ClientError::InvalidAssignedIp(_))
        ));
    }

    #[test]
    fn test_parse_auth_ok_when_invalid_gateway_returns_err() {
        let mut ok = auth_ok();
        ok.gateway = "not-an-ip".to_string();
        assert!(matches!(
            parse_auth_ok(&ok),
            Err(ClientError::InvalidGateway(_))
        ));
    }

    #[test]
    fn test_parse_auth_ok_when_invalid_subnet_returns_err() {
        let mut ok = auth_ok();
        ok.subnet = "not-a-net".to_string();
        assert!(matches!(
            parse_auth_ok(&ok),
            Err(ClientError::InvalidSubnet(_))
        ));
    }

    #[test]
    fn test_parse_auth_ok_when_mtu_below_min_returns_err() {
        let mut ok = auth_ok();
        ok.mtu = 1000;
        assert!(matches!(
            parse_auth_ok(&ok),
            Err(ClientError::MtuTooSmall(_))
        ));
    }

    #[test]
    fn test_parse_auth_ok_when_mtu_above_max_returns_err() {
        let mut ok = auth_ok();
        ok.mtu = 65_536;
        assert!(matches!(
            parse_auth_ok(&ok),
            Err(ClientError::MtuTooLarge(_))
        ));
    }

    #[test]
    fn test_parse_auth_ok_when_gateway_outside_subnet_returns_err() {
        let mut ok = auth_ok();
        ok.gateway = "192.168.1.1".to_string();
        assert!(matches!(
            parse_auth_ok(&ok),
            Err(ClientError::GatewayOutsideSubnet(_, _))
        ));
    }

    #[test]
    fn test_parse_auth_ok_when_gateway_is_network_addr_returns_err() {
        let mut ok = auth_ok();
        ok.gateway = "10.0.0.0".to_string();
        assert!(matches!(
            parse_auth_ok(&ok),
            Err(ClientError::GatewayIsNetworkAddr(_))
        ));
    }

    #[test]
    fn test_parse_auth_ok_when_gateway_is_host_addr_returns_ok() {
        let mut ok = auth_ok();
        ok.gateway = "10.0.0.1".to_string();
        assert!(parse_auth_ok(&ok).is_ok());
    }

    #[test]
    fn test_parse_auth_ok_when_routes_present_returns_params_with_routes() {
        let mut ok = auth_ok();
        ok.routes = vec!["192.168.100.0/24".to_string()];
        let params = parse_auth_ok(&ok).unwrap();
        assert_eq!(
            params.routes,
            vec!["192.168.100.0/24".parse::<Ipv4Net>().unwrap()]
        );
    }

    #[test]
    fn test_parse_auth_ok_when_routes_empty_returns_params_with_empty_routes() {
        let mut ok = auth_ok();
        ok.routes = vec![];
        let params = parse_auth_ok(&ok).unwrap();
        assert!(params.routes.is_empty());
    }

    #[test]
    fn test_parse_auth_ok_when_routes_contains_invalid_cidr_returns_invalid_route() {
        let mut ok = auth_ok();
        ok.routes = vec!["not-a-cidr".to_string()];
        assert!(matches!(
            parse_auth_ok(&ok),
            Err(ClientError::InvalidRoute(_))
        ));
    }

    #[allow(clippy::indexing_slicing)]
    fn assert_displays_unique(all: &[String]) {
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "duplicate at {i},{j}");
            }
        }
    }

    fn all_client_error_displays() -> Vec<String> {
        vec![
            ClientError::InvalidAssignedIp("x".into()).to_string(),
            ClientError::InvalidGateway("x".into()).to_string(),
            ClientError::InvalidSubnet("x".into()).to_string(),
            ClientError::MtuTooSmall(100).to_string(),
            ClientError::MtuTooLarge(70_000).to_string(),
            ClientError::GatewayOutsideSubnet(
                Ipv4Addr::new(10, 0, 0, 9),
                "10.0.0.0/24".parse().unwrap(),
            )
            .to_string(),
            ClientError::GatewayIsNetworkAddr(Ipv4Addr::new(10, 0, 0, 0)).to_string(),
            ClientError::AuthDenied("wrong password".into()).to_string(),
            ClientError::Protocol("unexpected msg".into()).to_string(),
            ClientError::InvalidRoute("not-a-cidr".into()).to_string(),
        ]
    }

    #[test]
    #[allow(clippy::indexing_slicing)]
    fn test_client_error_variants_display_are_distinct() {
        let all = all_client_error_displays();
        assert_displays_unique(&all);
        assert!(all[0].contains("assigned_ip"));
        assert!(all[1].contains("gateway"));
        assert!(all[2].contains("subnet"));
        assert!(all[3].contains("1280"));
        assert!(all[4].contains("65535"));
        assert!(all[7].contains("authentication failed"));
        assert!(all[8].contains("protocol"));
        assert!(all[9].contains("route"));
    }

    #[test]
    fn test_deny_reason_text_maps_known_reasons() {
        assert!(deny_reason_text(crate::vpn::DenyReason::AuthFailed as i32).contains("认证失败"));
        assert!(deny_reason_text(crate::vpn::DenyReason::ServerBusy as i32).contains("服务端繁忙"));
        assert!(deny_reason_text(999).contains("未知"));
    }

    #[test]
    fn test_validate_username_when_empty_returns_err() {
        for raw in ["", "   ", "\t"] {
            let err = validate_username(raw).expect_err("empty input must error");
            assert!(err.to_string().contains("用户名不能为空"), "raw={raw:?}");
        }
    }

    #[test]
    fn test_validate_username_when_non_empty_returns_trimmed() {
        let got = validate_username("  alice  ").expect("non-empty must succeed");
        assert_eq!(got, "alice");
    }

    #[test]
    fn test_exit_cause_code_reason_mapping() {
        let cases = [
            (ExitCause::Interrupted, 0, "client-shutdown"),
            (ExitCause::ServerDisconnect, 0, "server-disconnect"),
            (ExitCause::HeartbeatEnded, 0, "heartbeat-timeout"),
            (ExitCause::UplinkEnded, 0x1, "uplink-ended"),
            (ExitCause::DownlinkEnded, 0x1, "downlink-ended"),
            (ExitCause::TelemetryEnded, 0, "telemetry-ended"),
            (ExitCause::TaskPanicked, 0x2, "data-plane-panic"),
        ];
        for (cause, code, reason) in cases {
            assert_eq!(cause.code(), code, "{cause:?}");
            assert_eq!(cause.reason(), reason.as_bytes(), "{cause:?}");
        }
    }

    #[test]
    fn test_exit_cause_displays_are_distinct() {
        let all: Vec<String> = ExitCause::ALL.iter().map(ToString::to_string).collect();
        assert_displays_unique(&all);
    }

    #[test]
    fn test_exit_cause_is_copy_and_eq() {
        let a = ExitCause::HeartbeatEnded;
        let b = a;
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn test_established_client_field_order_construct_and_access() {
        let client = quic_link::Client::builder()
            .trust_ca(repo("cert.pem"))
            .server_name("localhost")
            .build()
            .expect("build client");
        let (session, channel) = connect_for_test(&client).await;
        let est = EstablishedClient {
            session,
            channel,
            params: test_params(),
            endpoint: client,
        };
        assert_est_access(&est);
    }

    fn test_params() -> ClientTunParams {
        ClientTunParams {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 2),
            subnet: "10.0.0.0/24".parse().unwrap(),
            gateway: Ipv4Addr::new(10, 0, 0, 1),
            mtu: 1280,
            routes: vec![],
        }
    }

    fn assert_est_access(est: &EstablishedClient) {
        assert_eq!(est.params.assigned_ip, Ipv4Addr::new(10, 0, 0, 2));
        let _ = est.session.id();
        let _ = est.params.subnet;
    }

    async fn connect_for_test(client: &quic_link::Client) -> (Session, Channel<ControlMessage>) {
        let server = quic_link::Server::builder()
            .tls_from_files(repo("cert.pem"), repo("key.pem"))
            .build("127.0.0.1:0".parse().unwrap())
            .expect("build server");
        let addr = server.local_addr().unwrap();
        let (server_result, session) = tokio::join!(server.accept(), client.connect(addr));
        let _server_session = server_result.expect("server accept").expect("accept conn");
        let session = session.expect("connect to server");
        let channel = session
            .open_stream::<ControlMessage>()
            .await
            .expect("open control stream");
        (session, channel)
    }
}
