use std::time::Duration;

use crate::auth::AuthError;

pub use crate::vpn::*;

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_FRAME_LENGTH: usize = 65_536;

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
    use crate::vpn::control_message::Msg;
    use prost::Message;

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
            })),
        };
        assert_eq!(roundtrip(&msg), msg);
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
}
