use std::net::Ipv4Addr;
use std::time::Duration;

use super::conn::{AuthStore, ClientNetProfile, ConnectionHandle};
use crate::ctrl::{self, deny_reason_from};
use crate::ledger::{ConnectionLedger, Evicted, ReservedIp};
use msgx::Channel;
use quic_link::Session;
use vpn_core::vpn::control_message::Msg;
use vpn_core::vpn::{AuthDenied, AuthOk, ControlMessage, ServerHello};

const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
const AUTH_DENY_CONFIRM: Duration = Duration::from_secs(1);

pub(super) async fn try_authenticate(
    session: &Session,
    auth: &AuthStore,
    ledger: &ConnectionLedger<ConnectionHandle>,
    profile: &ClientNetProfile,
) -> Option<(ConnectionHandle, String, Channel<ControlMessage>)> {
    let mut channel = accept_control_stream(session).await?;
    send_server_hello(&mut channel).await?;
    authenticate(session, auth, ledger, profile, channel).await
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

async fn send_server_hello(channel: &mut Channel<ControlMessage>) -> Option<()> {
    let hello = ControlMessage {
        msg: Some(Msg::ServerHello(ServerHello {
            protocol_version: ctrl::PROTOCOL_VERSION,
        })),
    };
    match channel.send(hello).await {
        Ok(()) => Some(()),
        Err(e) => {
            tracing::warn!("failed to send ServerHello: {e}");
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
    let deadline = tokio::time::Instant::now() + AUTH_REQUEST_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match channel.recv_timeout(remaining).await {
            Ok(msg) => match msg.msg {
                Some(Msg::AuthRequest(req)) => return Some(req),
                None => {}
                _ => {
                    session.close(0, b"protocol-error");
                    return None;
                }
            },
            Err(e) => {
                tracing::warn!("failed to receive auth request: {e}");
                return None;
            }
        }
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::mutable_key_type,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    use ipnet::Ipv4Net;
    use vpn_core::tun_setup::gateway_addr;

    fn net_profile_for(subnet: Ipv4Net, mtu: u16, routes: Vec<Ipv4Net>) -> ClientNetProfile {
        ClientNetProfile {
            subnet,
            gateway: gateway_addr(subnet),
            mtu,
            routes,
        }
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
