use std::net::Ipv4Addr;

use crate::auth::{AuthError, UserStore};
use crate::ipam::IpPool;

pub use crate::vpn::*;
pub use msgx::KEEPALIVE_INTERVAL as HEARTBEAT_INTERVAL;
pub use msgx::KEEPALIVE_TIMEOUT as HEARTBEAT_TIMEOUT;
pub use msgx::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, KeepaliveTracker, MAX_FRAME_LENGTH};

pub type HeartbeatTracker = KeepaliveTracker;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerSideError {
    Auth(AuthError),
    PoolExhausted,
}

pub fn deny_reason_from(e: &ServerSideError) -> DenyReason {
    match e {
        ServerSideError::Auth(_) => DenyReason::AuthFailed,
        ServerSideError::PoolExhausted => DenyReason::ServerBusy,
    }
}

pub fn authenticate(
    store: &UserStore,
    pool: &mut IpPool,
    req: &AuthRequest,
) -> Result<Ipv4Addr, ServerSideError> {
    store
        .verify(&req.username, &req.password)
        .map_err(ServerSideError::Auth)?;
    pool.alloc().map_err(|_| ServerSideError::PoolExhausted)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::auth::UserStore;
    use crate::ipam::IpPool;
    use crate::vpn::control_message::Msg;
    use argon2::password_hash::SaltString;
    use argon2::password_hash::rand_core::OsRng;
    use argon2::{Argon2, PasswordHasher};
    use ipnet::Ipv4Net;
    use prost::Message;
    use std::net::Ipv4Addr;
    use std::time::{Duration, Instant};

    fn hash_password(pw: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(pw.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    fn alice_store() -> UserStore {
        UserStore::from_users([("alice".to_string(), hash_password("s3cret"))]).unwrap()
    }

    fn net24() -> IpPool {
        IpPool::new(Ipv4Net::new_assert(Ipv4Addr::new(10, 0, 0, 0), 24)).unwrap()
    }

    fn roundtrip(msg: &ControlMessage) -> ControlMessage {
        let buf = msg.encode_to_vec();
        ControlMessage::decode(&*buf).unwrap()
    }

    #[test]
    fn test_control_message_auth_request_roundtrip_preserves_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthRequest(AuthRequest {
                username: "alice".to_string(),
                password: "s3cret".to_string(),
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_auth_ok_roundtrip_preserves_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthOk(AuthOk {
                assigned_ip: "10.0.0.2".to_string(),
                subnet: "10.0.0.0/24".to_string(),
                gateway: "10.0.0.1".to_string(),
                mtu: 1280,
                routes: vec!["192.168.100.0/24".to_string()],
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_auth_denied_roundtrip_preserves_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthDenied(AuthDenied {
                reason: DenyReason::AuthFailed as i32,
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_heartbeat_roundtrip_preserves_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::Heartbeat(Heartbeat {})),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_disconnect_roundtrip_preserves_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::Disconnect(Disconnect {
                reason: "superseded".to_string(),
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_heartbeat_roundtrip_only_heartbeat_set() {
        let msg = ControlMessage {
            msg: Some(Msg::Heartbeat(Heartbeat {})),
        };
        let decoded = roundtrip(&msg);
        assert!(matches!(decoded.msg, Some(Msg::Heartbeat(_))));
        assert!(!matches!(decoded.msg, Some(Msg::AuthRequest(_))));
        assert!(!matches!(decoded.msg, Some(Msg::AuthOk(_))));
        assert!(!matches!(decoded.msg, Some(Msg::AuthDenied(_))));
        assert!(!matches!(decoded.msg, Some(Msg::Disconnect(_))));
    }

    #[test]
    fn test_auth_request_multibyte_password_roundtrip_preserves_value() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthRequest(AuthRequest {
                username: "bob".to_string(),
                password: "密码".to_string(),
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_auth_ok_typical_config_roundtrip_preserves_all_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthOk(AuthOk {
                assigned_ip: "10.0.0.2".to_string(),
                subnet: "10.0.0.0/24".to_string(),
                gateway: "10.0.0.1".to_string(),
                mtu: 1280,
                routes: vec![],
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_auth_ok_with_multiple_routes_roundtrip_preserves_routes() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthOk(AuthOk {
                assigned_ip: "10.0.0.2".to_string(),
                subnet: "10.0.0.0/24".to_string(),
                gateway: "10.0.0.1".to_string(),
                mtu: 1280,
                routes: vec!["192.168.100.0/24".to_string(), "10.88.0.0/16".to_string()],
            })),
        };
        let decoded = roundtrip(&msg);
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_auth_ok_with_empty_routes_roundtrip_preserves_routes() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthOk(AuthOk {
                assigned_ip: "10.0.0.2".to_string(),
                subnet: "10.0.0.0/24".to_string(),
                gateway: "10.0.0.1".to_string(),
                mtu: 1280,
                routes: vec![],
            })),
        };
        let decoded = roundtrip(&msg);
        assert_eq!(decoded, msg);
        let Msg::AuthOk(ok) = decoded.msg.unwrap() else {
            unreachable!()
        };
        assert!(ok.routes.is_empty());
    }

    #[test]
    fn test_auth_ok_with_single_route_roundtrip_preserves_routes() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthOk(AuthOk {
                assigned_ip: "10.0.0.2".to_string(),
                subnet: "10.0.0.0/24".to_string(),
                gateway: "10.0.0.1".to_string(),
                mtu: 1280,
                routes: vec!["172.16.0.0/12".to_string()],
            })),
        };
        let decoded = roundtrip(&msg);
        assert_eq!(decoded, msg);
        let Msg::AuthOk(ok) = decoded.msg.unwrap() else {
            unreachable!()
        };
        assert_eq!(ok.routes, vec!["172.16.0.0/12".to_string()]);
    }

    #[test]
    fn test_auth_denied_auth_failed_roundtrip_preserves_reason() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthDenied(AuthDenied {
                reason: DenyReason::AuthFailed as i32,
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_auth_denied_server_busy_roundtrip_preserves_reason() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthDenied(AuthDenied {
                reason: DenyReason::ServerBusy as i32,
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_disconnect_superseded_roundtrip_preserves_reason() {
        let msg = ControlMessage {
            msg: Some(Msg::Disconnect(Disconnect {
                reason: "superseded".to_string(),
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_deny_reason_from_auth_error_returns_auth_failed() {
        let e = ServerSideError::Auth(AuthError::InvalidCredentials);
        assert_eq!(deny_reason_from(&e), DenyReason::AuthFailed);
    }

    #[test]
    fn test_deny_reason_from_pool_exhausted_returns_server_busy() {
        let e = ServerSideError::PoolExhausted;
        assert_eq!(deny_reason_from(&e), DenyReason::ServerBusy);
    }

    #[test]
    fn test_heartbeat_interval_equals_10_seconds() {
        assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(10));
    }

    #[test]
    fn test_heartbeat_timeout_equals_30_seconds() {
        assert_eq!(HEARTBEAT_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn test_max_frame_length_equals_64kib() {
        assert_eq!(MAX_FRAME_LENGTH, 65_536);
    }

    #[test]
    fn test_authenticate_when_valid_credentials_returns_allocated_ip_and_decrements() {
        let store = alice_store();
        let mut pool = net24();
        let before = pool.available_count();
        let req = AuthRequest {
            username: "alice".to_string(),
            password: "s3cret".to_string(),
        };
        let result = authenticate(&store, &mut pool, &req);
        assert_eq!(result, Ok(Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(pool.available_count(), before - 1);
    }

    #[test]
    fn test_authenticate_when_wrong_password_returns_auth_error_and_pool_unchanged() {
        let store = alice_store();
        let mut pool = net24();
        let before = pool.available_count();
        let req = AuthRequest {
            username: "alice".to_string(),
            password: "wrong".to_string(),
        };
        let result = authenticate(&store, &mut pool, &req);
        assert_eq!(
            result,
            Err(ServerSideError::Auth(AuthError::InvalidCredentials))
        );
        assert_eq!(pool.available_count(), before);
    }

    #[test]
    fn test_authenticate_when_unknown_user_returns_auth_error_and_pool_unchanged() {
        let store = alice_store();
        let mut pool = net24();
        let before = pool.available_count();
        let req = AuthRequest {
            username: "eve".to_string(),
            password: "anything".to_string(),
        };
        let result = authenticate(&store, &mut pool, &req);
        assert_eq!(
            result,
            Err(ServerSideError::Auth(AuthError::InvalidCredentials))
        );
        assert_eq!(pool.available_count(), before);
    }

    #[test]
    fn test_authenticate_when_empty_username_returns_auth_error_and_pool_unchanged() {
        let store = alice_store();
        let mut pool = net24();
        let before = pool.available_count();
        let req = AuthRequest {
            username: String::new(),
            password: "s3cret".to_string(),
        };
        let result = authenticate(&store, &mut pool, &req);
        assert_eq!(
            result,
            Err(ServerSideError::Auth(AuthError::InvalidCredentials))
        );
        assert_eq!(pool.available_count(), before);
    }

    #[test]
    fn test_authenticate_when_pool_exhausted_returns_pool_exhausted() {
        let store = alice_store();
        let mut pool = IpPool::new(Ipv4Net::new_assert(Ipv4Addr::new(10, 0, 0, 0), 30)).unwrap();
        pool.alloc().unwrap();
        assert_eq!(pool.available_count(), 0);
        let req = AuthRequest {
            username: "alice".to_string(),
            password: "s3cret".to_string(),
        };
        let result = authenticate(&store, &mut pool, &req);
        assert_eq!(result, Err(ServerSideError::PoolExhausted));
    }

    fn exhausted_pool() -> IpPool {
        let mut pool = IpPool::new(Ipv4Net::new_assert(Ipv4Addr::new(10, 0, 0, 0), 30)).unwrap();
        pool.alloc().unwrap();
        pool
    }

    #[test]
    fn test_authenticate_wrong_password_maps_to_auth_failed_deny_reason() {
        let store = alice_store();
        let mut pool = exhausted_pool();
        let auth_err = authenticate(
            &store,
            &mut pool,
            &AuthRequest {
                username: "alice".to_string(),
                password: "wrong".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(deny_reason_from(&auth_err), DenyReason::AuthFailed);
    }

    #[test]
    fn test_authenticate_pool_exhausted_maps_to_server_busy_deny_reason() {
        let store = alice_store();
        let mut pool = exhausted_pool();
        let pool_err = authenticate(
            &store,
            &mut pool,
            &AuthRequest {
                username: "alice".to_string(),
                password: "s3cret".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(deny_reason_from(&pool_err), DenyReason::ServerBusy);
    }

    #[test]
    fn test_heartbeat_tracker_is_dead_at_construction_returns_false() {
        let t0 = Instant::now();
        let tracker = HeartbeatTracker::new(t0);
        assert!(!tracker.is_dead(t0));
    }

    #[test]
    fn test_heartbeat_tracker_is_dead_just_below_timeout_returns_false() {
        let t0 = Instant::now();
        let tracker = HeartbeatTracker::new(t0);
        let just_below = (t0 + HEARTBEAT_TIMEOUT)
            .checked_sub(Duration::from_nanos(1))
            .unwrap();
        assert!(!tracker.is_dead(just_below));
    }

    #[test]
    fn test_heartbeat_tracker_is_dead_at_exact_timeout_returns_true() {
        let t0 = Instant::now();
        let tracker = HeartbeatTracker::new(t0);
        assert!(tracker.is_dead(t0 + HEARTBEAT_TIMEOUT));
    }

    #[test]
    fn test_heartbeat_tracker_is_dead_beyond_timeout_returns_true() {
        let t0 = Instant::now();
        let tracker = HeartbeatTracker::new(t0);
        assert!(tracker.is_dead(t0 + HEARTBEAT_TIMEOUT + Duration::from_secs(5)));
    }

    #[test]
    fn test_heartbeat_tracker_observe_revives_after_death() {
        let t0 = Instant::now();
        let mut tracker = HeartbeatTracker::new(t0);
        let deadline = t0 + HEARTBEAT_TIMEOUT;
        assert!(tracker.is_dead(deadline));
        tracker.observe(deadline);
        assert!(!tracker.is_dead(deadline + Duration::from_secs(1)));
    }

    #[test]
    fn test_heartbeat_tracker_next_deadline_equals_last_seen_plus_timeout() {
        let t0 = Instant::now();
        let tracker = HeartbeatTracker::new(t0);
        assert_eq!(tracker.next_deadline(), t0 + HEARTBEAT_TIMEOUT);
    }

    #[test]
    fn test_heartbeat_tracker_next_deadline_updates_after_observe() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(100);
        let mut tracker = HeartbeatTracker::new(t0);
        tracker.observe(t1);
        assert_eq!(tracker.next_deadline(), t1 + HEARTBEAT_TIMEOUT);
    }
}
