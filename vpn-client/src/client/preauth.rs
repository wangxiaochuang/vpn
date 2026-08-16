use std::net::Ipv4Addr;

use anyhow::{Context, anyhow};
use ipnet::Ipv4Net;
use thiserror::Error;

use super::ClientTunParams;
use super::EstablishedClient;
use crate::config::ClientConfig;
use crate::config::MIN_MTU;
use crate::credentials::CredentialCollector;
use msgx::Channel;
use quic_link::Session;
use vpn_core::vpn::AuthOk;
use vpn_core::vpn::ControlMessage;
use vpn_core::vpn::control_message::Msg;

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
    #[error("incompatible protocol version: server={server}, client={client}", server = .0, client = vpn_core::ctrl::PROTOCOL_VERSION)]
    IncompatibleVersion(u32),
    #[error("protocol error: {0}")]
    Protocol(String),
}

pub struct PreAuthClient {
    session: Session,
    channel: Channel<ControlMessage>,
    supported_methods: Vec<vpn_core::vpn::AuthMethod>,
    endpoint: quic_link::Client,
}

impl PreAuthClient {
    pub async fn connect(config: &ClientConfig) -> anyhow::Result<Self> {
        let endpoint = quic_link::Client::builder()
            .trust_ca(config.ca_cert.clone())
            .server_name(config.server_name.clone())
            .build()
            .context("failed to build client")?;
        let session = endpoint.connect(config.server).await?;
        tracing::info!("connected to {}", config.server);
        let mut channel = open_control_stream(&session).await?;
        send_open_signal(&mut channel).await?;
        let supported_methods = recv_and_validate_hello(&mut channel).await?;
        Ok(Self {
            session,
            channel,
            supported_methods,
            endpoint,
        })
    }

    pub fn session_id(&self) -> usize {
        self.session.id()
    }

    pub async fn authenticate<C: CredentialCollector>(
        mut self,
        collector: &mut C,
    ) -> anyhow::Result<EstablishedClient> {
        let params = auth_loop(&mut self.channel, &self.supported_methods, collector).await?;
        Ok(EstablishedClient::new(
            self.session,
            self.channel,
            params,
            self.endpoint,
        ))
    }
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
        r if r == vpn_core::vpn::DenyReason::AuthFailed as i32 => "认证失败（用户名或密码错误）",
        r if r == vpn_core::vpn::DenyReason::ServerBusy as i32 => "服务端繁忙（IP 池耗尽）",
        _ => "未知拒绝原因",
    }
}

async fn send_open_signal(channel: &mut Channel<ControlMessage>) -> anyhow::Result<()> {
    channel
        .send(ControlMessage { msg: None })
        .await
        .context("failed to open control stream on peer")
}

async fn recv_and_validate_hello(
    channel: &mut Channel<ControlMessage>,
) -> Result<Vec<vpn_core::vpn::AuthMethod>, ClientError> {
    let first = channel
        .recv()
        .await
        .map_err(|e| ClientError::Protocol(format!("failed to receive ServerHello: {e}")))?
        .ok_or_else(|| ClientError::Protocol("control stream closed before ServerHello".into()))?;
    match first.msg {
        Some(Msg::ServerHello(h)) => {
            if h.protocol_version != vpn_core::ctrl::PROTOCOL_VERSION {
                return Err(ClientError::IncompatibleVersion(h.protocol_version));
            }
            Ok(parse_supported_methods(&h.supported_methods))
        }
        _ => Err(ClientError::Protocol(
            "expected ServerHello as first control message".into(),
        )),
    }
}

fn parse_supported_methods(raw: &[i32]) -> Vec<vpn_core::vpn::AuthMethod> {
    raw.iter()
        .filter_map(|&i| vpn_core::vpn::AuthMethod::try_from(i).ok())
        .collect()
}

async fn open_control_stream(session: &Session) -> anyhow::Result<Channel<ControlMessage>> {
    session
        .open_stream::<ControlMessage>()
        .await
        .context("failed to open control stream")
}

async fn auth_loop<C: CredentialCollector>(
    channel: &mut Channel<ControlMessage>,
    methods: &[vpn_core::vpn::AuthMethod],
    collector: &mut C,
) -> anyhow::Result<ClientTunParams> {
    let init = collector.collect_init(methods).await;
    send_auth_init(channel, init).await?;
    loop {
        let msg = channel
            .recv()
            .await
            .map_err(|e| anyhow!("failed to receive auth response: {e}"))?
            .ok_or_else(|| anyhow!("control stream closed during auth"))?;
        match handle_auth_msg(channel, msg, collector).await? {
            AuthLoopExit::Ok(params) => return Ok(params),
            AuthLoopExit::Continue => {}
        }
    }
}

enum AuthLoopExit {
    Ok(ClientTunParams),
    Continue,
}

async fn handle_auth_msg<C: CredentialCollector>(
    channel: &mut Channel<ControlMessage>,
    msg: ControlMessage,
    collector: &mut C,
) -> anyhow::Result<AuthLoopExit> {
    match msg.msg {
        Some(Msg::AuthOk(ok)) => {
            let params = parse_auth_ok(&ok)?;
            Ok(AuthLoopExit::Ok(params))
        }
        Some(Msg::AuthDenied(d)) => {
            let reason = deny_reason_text(d.reason);
            tracing::error!("{reason}");
            Err(ClientError::AuthDenied(reason.to_string()).into())
        }
        Some(Msg::AuthChallenge(challenge)) => {
            let response = collector.collect_response(&challenge).await;
            send_auth_response(channel, response).await?;
            Ok(AuthLoopExit::Continue)
        }
        _ => Err(ClientError::Protocol("unexpected message during auth loop".into()).into()),
    }
}

async fn send_auth_init(
    channel: &mut Channel<ControlMessage>,
    init: vpn_core::vpn::AuthInit,
) -> anyhow::Result<()> {
    channel
        .send(ControlMessage {
            msg: Some(Msg::AuthInit(init)),
        })
        .await
        .context("failed to send AuthInit")
}

async fn send_auth_response(
    channel: &mut Channel<ControlMessage>,
    response: vpn_core::vpn::AuthResponse,
) -> anyhow::Result<()> {
    channel
        .send(ControlMessage {
            msg: Some(Msg::AuthResponse(response)),
        })
        .await
        .context("failed to send AuthResponse")
}

#[cfg(test)]
fn validate_username(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("用户名不能为空");
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::indexing_slicing
)]
mod tests {
    use std::net::Ipv4Addr;

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
            ClientError::IncompatibleVersion(99).to_string(),
            ClientError::Protocol("unexpected msg".into()).to_string(),
            ClientError::InvalidRoute("not-a-cidr".into()).to_string(),
        ]
    }

    #[test]
    fn test_client_error_variants_display_are_distinct() {
        let all = all_client_error_displays();
        assert_displays_unique(&all);
        assert!(all[0].contains("assigned_ip"));
        assert!(all[1].contains("gateway"));
        assert!(all[2].contains("subnet"));
        assert!(all[3].contains("1280"));
        assert!(all[4].contains("65535"));
        assert!(all[7].contains("authentication failed"));
        assert!(all[8].contains("incompatible"));
        assert!(all[9].contains("protocol"));
        assert!(all[10].contains("route"));
    }

    #[test]
    fn test_deny_reason_text_maps_known_reasons() {
        assert!(
            deny_reason_text(vpn_core::vpn::DenyReason::AuthFailed as i32).contains("认证失败")
        );
        assert!(
            deny_reason_text(vpn_core::vpn::DenyReason::ServerBusy as i32).contains("服务端繁忙")
        );
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
}
