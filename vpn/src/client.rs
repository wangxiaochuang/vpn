use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, anyhow};
use ipnet::Ipv4Net;
use thiserror::Error;

use crate::config::ClientConfig;
use crate::config::MIN_MTU;
use crate::data::{Tun, forward};
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
    let password = tokio::task::spawn_blocking(move || rpassword::prompt_password("请输入密码："))
        .await
        .context("password prompt task panicked")??;
    run_with_credentials(config, password, sd).await
}

pub async fn run_with_credentials(
    config: ClientConfig,
    password: String,
    sd: Shutdown,
) -> anyhow::Result<()> {
    let (client, session, channel, params) = establish_connection(&config, password).await?;
    let tun = setup_tun(&params)?;
    tracing::info!(
        "authenticated as {}, assigned_ip={}, subnet={}, mtu={}",
        config.username,
        params.assigned_ip,
        params.subnet,
        params.mtu
    );
    run_data_plane(&session, tun, channel, client, sd).await
}

async fn establish_connection(
    config: &ClientConfig,
    password: String,
) -> anyhow::Result<(
    quic_link::Client,
    Session,
    Channel<ControlMessage>,
    ClientTunParams,
)> {
    let client = quic_link::Client::builder()
        .trust_ca(config.ca_cert.clone())
        .server_name(config.server_name.clone())
        .build()
        .context("failed to build client")?;
    let session = client.connect(config.server).await?;
    tracing::info!("connected to {}", config.server);
    let mut channel = open_control_stream(&session).await?;
    let params = authenticate(&mut channel, &config.username, password).await?;
    Ok((client, session, channel, params))
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
) {
    let hb = || ControlMessage {
        msg: Some(Msg::Heartbeat(crate::vpn::Heartbeat {})),
    };
    keepalive_loop(
        &session,
        &mut writer,
        &mut reader,
        &shutdown,
        KeepaliveConfig::default(),
        hb,
        handle_ctrl_msg,
    )
    .await;
}

fn handle_ctrl_msg(m: &ControlMessage) -> LoopControl {
    if matches!(m.msg, Some(Msg::Disconnect(_))) {
        tracing::info!("server disconnected");
        LoopControl::Break
    } else {
        LoopControl::Continue
    }
}

async fn run_data_plane(
    session: &Session,
    tun: std::sync::Arc<tun_rs::AsyncDevice>,
    channel: Channel<ControlMessage>,
    client: quic_link::Client,
    sd: Shutdown,
) -> anyhow::Result<()> {
    let (writer, reader) = channel.split();
    let mut tasks = spawn_data_tasks(session, tun, reader, writer, &sd);
    shutdown::wait_for_interrupt(&sd).await;
    session.close(0, b"client-shutdown");
    sd.drain(&mut tasks, "client").await;
    std::mem::forget(client);
    Ok(())
}

fn spawn_data_tasks(
    session: &Session,
    tun: std::sync::Arc<tun_rs::AsyncDevice>,
    reader: Receiver<ControlMessage>,
    writer: Sender<ControlMessage>,
    sd: &Shutdown,
) -> tokio::task::JoinSet<()> {
    let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    let session_for_hb = session.clone();
    let hb_handle = sd.handle();
    tasks.spawn(async move {
        heartbeat_loop(session_for_hb, reader, writer, hb_handle.clone()).await;
        hb_handle.cancel();
    });
    spawn_uplink(session, tun.clone(), sd.handle(), &mut tasks);
    spawn_downlink(session, tun, sd.handle(), &mut tasks);
    tasks
}

fn spawn_uplink(
    session: &Session,
    tun: std::sync::Arc<tun_rs::AsyncDevice>,
    shutdown: ShutdownHandle,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    let session_for_uplink = session.clone();
    tasks.spawn(async move {
        let mut source = Tun(tun);
        let mut sink = session_for_uplink.datagram_tx();
        let _ = forward(&mut source, &mut sink, &shutdown).await;
        session_for_uplink.close(0x101, b"uplink-ended");
        shutdown.cancel();
    });
}

fn spawn_downlink(
    session: &Session,
    tun: std::sync::Arc<tun_rs::AsyncDevice>,
    shutdown: ShutdownHandle,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    let session_for_downlink = session.clone();
    tasks.spawn(async move {
        let mut source = session_for_downlink.datagram_rx();
        let mut sink = Tun(tun);
        let _ = forward(&mut source, &mut sink, &shutdown).await;
        session_for_downlink.close(0x102, b"downlink-ended");
        shutdown.cancel();
    });
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
}
