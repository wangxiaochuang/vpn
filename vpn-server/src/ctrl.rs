use crate::auth::AuthError;
use vpn_core::vpn::DenyReason;

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use prost::Message;
    use std::time::{Duration, Instant};
    use vpn_core::ctrl::HEARTBEAT_INTERVAL;
    use vpn_core::ctrl::HEARTBEAT_TIMEOUT;
    use vpn_core::ctrl::HeartbeatTracker;
    use vpn_core::ctrl::MAX_FRAME_LENGTH;
    use vpn_core::vpn::ControlMessage;
    use vpn_core::vpn::control_message::Msg;

    fn roundtrip(msg: &ControlMessage) -> ControlMessage {
        let buf = msg.encode_to_vec();
        ControlMessage::decode(&*buf).unwrap()
    }

    #[test]
    fn test_control_message_auth_init_roundtrip_preserves_fields() {
        use vpn_core::vpn::auth_init::Method;
        let msg = ControlMessage {
            msg: Some(Msg::AuthInit(vpn_core::vpn::AuthInit {
                username: "alice".to_string(),
                method: Some(Method::Password(vpn_core::vpn::PasswordAuth {
                    password: "s3cret".to_string(),
                })),
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_auth_ok_roundtrip_preserves_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthOk(vpn_core::vpn::AuthOk {
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
            msg: Some(Msg::AuthDenied(vpn_core::vpn::AuthDenied {
                reason: DenyReason::AuthFailed as i32,
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_heartbeat_roundtrip_preserves_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::Heartbeat(vpn_core::vpn::Heartbeat {})),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_disconnect_roundtrip_preserves_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::Disconnect(vpn_core::vpn::Disconnect {
                reason: "superseded".to_string(),
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_control_message_heartbeat_roundtrip_only_heartbeat_set() {
        let msg = ControlMessage {
            msg: Some(Msg::Heartbeat(vpn_core::vpn::Heartbeat {})),
        };
        let decoded = roundtrip(&msg);
        assert!(matches!(decoded.msg, Some(Msg::Heartbeat(_))));
    }

    #[test]
    fn test_auth_ok_typical_config_roundtrip_preserves_all_fields() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthOk(vpn_core::vpn::AuthOk {
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
            msg: Some(Msg::AuthOk(vpn_core::vpn::AuthOk {
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
            msg: Some(Msg::AuthOk(vpn_core::vpn::AuthOk {
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
            msg: Some(Msg::AuthOk(vpn_core::vpn::AuthOk {
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
            msg: Some(Msg::AuthDenied(vpn_core::vpn::AuthDenied {
                reason: DenyReason::AuthFailed as i32,
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_auth_denied_server_busy_roundtrip_preserves_reason() {
        let msg = ControlMessage {
            msg: Some(Msg::AuthDenied(vpn_core::vpn::AuthDenied {
                reason: DenyReason::ServerBusy as i32,
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
    }

    #[test]
    fn test_disconnect_superseded_roundtrip_preserves_reason() {
        let msg = ControlMessage {
            msg: Some(Msg::Disconnect(vpn_core::vpn::Disconnect {
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
