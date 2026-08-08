use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, anyhow};
use ipnet::Ipv4Net;
use thiserror::Error;

use crate::config::ClientConfig;
use crate::config::MIN_MTU;
use crate::ctrl::{HEARTBEAT_INTERVAL, HeartbeatTracker};
use crate::data::{PacketSink, PacketSource, QuinnDatagram, forward};
use crate::framing::ControlCodec;
use crate::server::ControlStream;
use crate::vpn::AuthOk;
use crate::vpn::control_message::Msg;
use futures::{SinkExt, StreamExt};
use tokio_util::codec::{Framed, FramedParts};
use tokio_util::sync::CancellationToken;

pub struct TunSource(pub std::sync::Arc<tun_rs::AsyncDevice>);

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

pub struct TunSink(pub std::sync::Arc<tun_rs::AsyncDevice>);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTunParams {
    pub assigned_ip: Ipv4Addr,
    pub subnet: Ipv4Net,
    pub gateway: Ipv4Addr,
    pub mtu: u16,
    pub routes: Vec<Ipv4Net>,
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

pub fn spawn_signal_watchdog() -> CancellationToken {
    spawn_signal_watchdog_inner().0
}

fn spawn_signal_watchdog_inner() -> (CancellationToken, tokio::sync::oneshot::Receiver<()>) {
    let shutdown = CancellationToken::new();
    let ctrl_shutdown = shutdown.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut sig = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        {
            Ok(sig) => sig,
            Err(e) => {
                tracing::warn!("failed to register SIGINT handler: {e}");
                return;
            }
        };
        let _ = ready_tx.send(());
        if sig.recv().await.is_some() {
            tracing::info!("received Ctrl+C, initiating graceful shutdown");
            ctrl_shutdown.cancel();
        }
    });
    (shutdown, ready_rx)
}

pub async fn run(config: ClientConfig) -> anyhow::Result<()> {
    let shutdown = spawn_signal_watchdog();
    let password = tokio::task::spawn_blocking(move || rpassword::prompt_password("请输入密码："))
        .await
        .context("password prompt task panicked")??;
    run_with_credentials(config, password, shutdown).await
}

pub async fn run_with_credentials(
    config: ClientConfig,
    password: String,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let (endpoint, conn, framed, params) = establish_connection(&config, password).await?;
    let tun = setup_tun(&params)?;
    tracing::info!(
        "authenticated as {}, assigned_ip={}, subnet={}, mtu={}",
        config.username,
        params.assigned_ip,
        params.subnet,
        params.mtu
    );
    run_data_plane(&conn, tun, framed, endpoint, shutdown).await
}

async fn establish_connection(
    config: &ClientConfig,
    password: String,
) -> anyhow::Result<(
    quinn::Endpoint,
    quinn::Connection,
    ControlFramed,
    ClientTunParams,
)> {
    let endpoint = connect_quic()?;
    let conn = endpoint
        .connect_with(
            crate::tls::build_quinn_client_config(&config.ca_cert, &config.server_name)
                .context("failed to build client TLS config")?,
            config.server,
            &config.server_name,
        )
        .context("failed to initiate QUIC connection")?
        .await
        .context("failed to connect to server")?;
    tracing::info!("connected to {}", config.server);
    let mut framed = open_control_stream(&conn).await?;
    let params = authenticate(&mut framed, &config.username, password).await?;
    Ok((endpoint, conn, framed, params))
}

fn connect_quic() -> anyhow::Result<quinn::Endpoint> {
    quinn::Endpoint::client("0.0.0.0:0".parse()?).context("failed to bind client endpoint")
}

async fn open_control_stream(conn: &quinn::Connection) -> anyhow::Result<ControlFramed> {
    let (send, recv) = conn
        .open_bi()
        .await
        .context("failed to open control stream")?;
    Ok(Framed::new(
        ControlStream::new(send, recv),
        ControlCodec::new(),
    ))
}

async fn authenticate(
    framed: &mut ControlFramed,
    username: &str,
    password: String,
) -> anyhow::Result<ClientTunParams> {
    send_auth_request(framed, username, password).await?;
    let first = framed
        .next()
        .await
        .ok_or_else(|| anyhow!("control stream closed before AuthOk"))?
        .context("failed to decode first response")?;
    interpret_auth_response(first)
}

async fn send_auth_request(
    framed: &mut ControlFramed,
    username: &str,
    password: String,
) -> anyhow::Result<()> {
    framed
        .send(crate::vpn::ControlMessage {
            msg: Some(Msg::AuthRequest(crate::vpn::AuthRequest {
                username: username.to_string(),
                password,
            })),
        })
        .await
        .context("failed to send AuthRequest")
}

fn interpret_auth_response(first: crate::vpn::ControlMessage) -> anyhow::Result<ClientTunParams> {
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

type ControlFramed = Framed<ControlStream, ControlCodec>;
type HeartbeatReader = Framed<quinn::RecvStream, ControlCodec>;
type HeartbeatWriter = Framed<quinn::SendStream, ControlCodec>;

pub async fn heartbeat_loop(
    conn: quinn::Connection,
    reader: HeartbeatReader,
    writer: HeartbeatWriter,
    shutdown: CancellationToken,
) {
    let mut reader = reader;
    let mut writer = writer;
    let mut tracker = HeartbeatTracker::new(tokio::time::Instant::now().into_std());
    let mut send_tick = tokio::time::interval(HEARTBEAT_INTERVAL);
    let mut timeout_tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        let should_break = tokio::select! {
            biased;
            () = shutdown.cancelled() => true,
            _ = timeout_tick.tick() => close_if_dead(&conn, &tracker),
            _ = send_tick.tick() => send_heartbeat(&mut writer).await.is_err(),
            msg = reader.next() => !handle_heartbeat_msg(msg, &mut tracker),
        };
        if should_break {
            break;
        }
    }
}

fn close_if_dead(conn: &quinn::Connection, tracker: &HeartbeatTracker) -> bool {
    if tracker.is_dead(tokio::time::Instant::now().into_std()) {
        conn.close(0x100u32.into(), b"timeout");
        true
    } else {
        false
    }
}

async fn send_heartbeat(writer: &mut HeartbeatWriter) -> Result<(), ()> {
    let hb = crate::vpn::ControlMessage {
        msg: Some(Msg::Heartbeat(crate::vpn::Heartbeat {})),
    };
    writer.send(hb).await.map_err(|_| ())
}

fn handle_heartbeat_msg<E>(
    msg: Option<std::result::Result<crate::vpn::ControlMessage, E>>,
    tracker: &mut HeartbeatTracker,
) -> bool {
    match msg {
        Some(Ok(crate::vpn::ControlMessage {
            msg: Some(Msg::Heartbeat(_)),
        })) => {
            tracker.observe(tokio::time::Instant::now().into_std());
            true
        }
        Some(Ok(crate::vpn::ControlMessage {
            msg: Some(Msg::Disconnect(d)),
        })) => {
            tracing::info!("server disconnected: {}", d.reason);
            false
        }
        Some(Ok(_)) => true,
        _ => false,
    }
}

async fn run_data_plane(
    conn: &quinn::Connection,
    tun: std::sync::Arc<tun_rs::AsyncDevice>,
    framed: ControlFramed,
    endpoint: quinn::Endpoint,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let (reader, writer) = split_control_stream(framed);
    let mut tasks = spawn_data_tasks(conn, tun, reader, writer, &shutdown);
    wait_for_shutdown(&shutdown).await;
    conn.close(0u32.into(), b"client-shutdown");
    crate::shutdown::drain_with_timeout(&mut tasks, Duration::from_secs(5), "client").await;
    endpoint.close(0u32.into(), b"client-shutdown");
    Ok(())
}

fn split_control_stream(framed: ControlFramed) -> (HeartbeatReader, HeartbeatWriter) {
    let parts = framed.into_parts();
    let control_stream = parts.io;
    let read_buf = parts.read_buf;
    let (send_stream, recv_stream) = control_stream.into_parts();
    let mut reader_parts = FramedParts::new(recv_stream, ControlCodec::new());
    reader_parts.read_buf = read_buf;
    let reader = Framed::from_parts(reader_parts);
    let writer = Framed::new(send_stream, ControlCodec::new());
    (reader, writer)
}

fn spawn_data_tasks(
    conn: &quinn::Connection,
    tun: std::sync::Arc<tun_rs::AsyncDevice>,
    reader: HeartbeatReader,
    writer: HeartbeatWriter,
    shutdown: &CancellationToken,
) -> tokio::task::JoinSet<()> {
    let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    let conn_for_hb = conn.clone();
    let ctrl_shutdown = shutdown.clone();
    tasks.spawn(async move {
        heartbeat_loop(conn_for_hb, reader, writer, ctrl_shutdown.clone()).await;
        ctrl_shutdown.cancel();
    });
    spawn_uplink(conn, tun.clone(), shutdown, &mut tasks);
    spawn_downlink(conn, tun, shutdown, &mut tasks);
    tasks
}

fn spawn_uplink(
    conn: &quinn::Connection,
    tun: std::sync::Arc<tun_rs::AsyncDevice>,
    shutdown: &CancellationToken,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    let uplink_conn = conn.clone();
    let uplink_shutdown = shutdown.clone();
    tasks.spawn(async move {
        let mut source = TunSource(tun);
        let mut sink = QuinnDatagram::new(uplink_conn.clone());
        let _ = forward(&mut source, &mut sink, &uplink_shutdown).await;
        uplink_conn.close(0x101u32.into(), b"uplink-ended");
        uplink_shutdown.cancel();
    });
}

fn spawn_downlink(
    conn: &quinn::Connection,
    tun: std::sync::Arc<tun_rs::AsyncDevice>,
    shutdown: &CancellationToken,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    let downlink_conn = conn.clone();
    let downlink_shutdown = shutdown.clone();
    tasks.spawn(async move {
        let mut source = QuinnDatagram::new(downlink_conn.clone());
        let mut sink = TunSink(tun);
        let _ = forward(&mut source, &mut sink, &downlink_shutdown).await;
        downlink_conn.close(0x102u32.into(), b"downlink-ended");
        downlink_shutdown.cancel();
    });
}

async fn wait_for_shutdown(shutdown: &CancellationToken) {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => {}
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received Ctrl+C, initiating graceful shutdown");
            shutdown.cancel();
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

    #[tokio::test]
    async fn test_spawn_signal_watchdog_cancels_on_sigint() {
        let (shutdown, ready) = spawn_signal_watchdog_inner();
        ready
            .await
            .expect("watchdog should finish registering the SIGINT handler");
        unsafe {
            libc::kill(libc::getpid(), libc::SIGINT);
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), shutdown.cancelled())
            .await
            .expect("watchdog should cancel the token when SIGINT is received");
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
}
