pub const PROTOCOL_VERSION: u32 = 1;

pub use crate::vpn::*;
pub use msgx::KEEPALIVE_INTERVAL as HEARTBEAT_INTERVAL;
pub use msgx::KEEPALIVE_TIMEOUT as HEARTBEAT_TIMEOUT;
pub use msgx::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, KeepaliveTracker, MAX_FRAME_LENGTH};

pub type HeartbeatTracker = KeepaliveTracker;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use prost::Message;

    fn round_trip<M>(msg: &M) -> M
    where
        M: Message + Default,
    {
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        M::decode(&*buf).unwrap()
    }

    #[test]
    fn test_protocol_version_equals_one() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn test_auth_method_password_equals_zero() {
        assert_eq!(AuthMethod::Password as i32, 0);
    }

    #[test]
    fn test_auth_method_totp_equals_one() {
        assert_eq!(AuthMethod::Totp as i32, 1);
    }

    #[test]
    fn test_server_hello_round_trip_preserves_protocol_version_and_methods() {
        let hello = ServerHello {
            protocol_version: 1,
            supported_methods: vec![AuthMethod::Password as i32],
        };
        let decoded = round_trip(&hello);
        assert_eq!(decoded.protocol_version, 1);
        assert_eq!(decoded.supported_methods, vec![AuthMethod::Password as i32]);
    }

    #[test]
    fn test_server_hello_round_trip_preserves_empty_methods() {
        let hello = ServerHello {
            protocol_version: 1,
            supported_methods: vec![],
        };
        let decoded = round_trip(&hello);
        assert!(decoded.supported_methods.is_empty());
    }

    #[test]
    fn test_auth_init_with_password_round_trip_preserves_fields() {
        let init = AuthInit {
            username: "alice".to_string(),
            method: Some(auth_init::Method::Password(PasswordAuth {
                password: "s3cret".to_string(),
            })),
        };
        let decoded = round_trip(&init);
        assert_eq!(decoded.username, "alice");
        assert!(matches!(
            decoded.method,
            Some(auth_init::Method::Password(ref p)) if p.password == "s3cret"
        ));
    }

    #[test]
    fn test_auth_init_with_multibyte_password_round_trip_preserves_value() {
        let init = AuthInit {
            username: "bob".to_string(),
            method: Some(auth_init::Method::Password(PasswordAuth {
                password: "密码".to_string(),
            })),
        };
        let decoded = round_trip(&init);
        let Some(auth_init::Method::Password(p)) = decoded.method else {
            panic!("expected Password method");
        };
        assert_eq!(p.password, "密码");
    }

    #[test]
    fn test_auth_challenge_with_totp_round_trip_preserves_prompt() {
        let challenge = AuthChallenge {
            challenge: Some(auth_challenge::Challenge::Totp(TotpChallenge {
                prompt: "Enter TOTP code".to_string(),
            })),
        };
        let decoded = round_trip(&challenge);
        assert!(matches!(
            decoded.challenge,
            Some(auth_challenge::Challenge::Totp(ref t)) if t.prompt == "Enter TOTP code"
        ));
    }

    #[test]
    fn test_auth_response_with_totp_round_trip_preserves_code() {
        let response = AuthResponse {
            response: Some(auth_response::Response::Totp(TotpResponse {
                code: "123456".to_string(),
            })),
        };
        let decoded = round_trip(&response);
        assert!(matches!(
            decoded.response,
            Some(auth_response::Response::Totp(ref t)) if t.code == "123456"
        ));
    }

    #[test]
    fn test_control_message_server_hello_round_trip_fidelity() {
        let cm = ControlMessage {
            msg: Some(control_message::Msg::ServerHello(ServerHello {
                protocol_version: 42,
                supported_methods: vec![],
            })),
        };
        let decoded = round_trip(&cm);
        match decoded.msg {
            Some(control_message::Msg::ServerHello(h)) => {
                assert_eq!(h.protocol_version, 42);
            }
            other => panic!("expected ServerHello, got {other:?}"),
        }
    }

    #[test]
    fn test_control_message_all_eight_branches_round_trip() {
        for variant in all_eight_variants() {
            let original = ControlMessage { msg: Some(variant) };
            let decoded = round_trip(&original);
            assert_same_variant(original.msg.as_ref(), decoded.msg.as_ref());
        }
    }

    fn all_eight_variants() -> [control_message::Msg; 8] {
        use control_message::Msg;
        [
            Msg::ServerHello(ServerHello::default()),
            Msg::AuthInit(AuthInit::default()),
            Msg::AuthChallenge(AuthChallenge::default()),
            Msg::AuthResponse(AuthResponse::default()),
            Msg::AuthOk(AuthOk::default()),
            Msg::AuthDenied(AuthDenied::default()),
            Msg::Heartbeat(Heartbeat::default()),
            Msg::Disconnect(Disconnect::default()),
        ]
    }

    fn assert_same_variant(a: Option<&control_message::Msg>, b: Option<&control_message::Msg>) {
        use std::mem::discriminant;
        match (a, b) {
            (Some(x), Some(y)) => assert_eq!(discriminant(x), discriminant(y)),
            _ => panic!("oneof variant mismatch after round-trip"),
        }
    }

    #[test]
    fn test_oneof_server_hello_is_mutually_exclusive() {
        let cm = ControlMessage {
            msg: Some(control_message::Msg::ServerHello(ServerHello {
                protocol_version: 1,
                supported_methods: vec![],
            })),
        };
        let decoded = round_trip(&cm);
        assert!(
            matches!(decoded.msg, Some(control_message::Msg::ServerHello(_))),
            "only server_hello should be set"
        );
    }

    #[test]
    fn test_oneof_auth_init_is_mutually_exclusive() {
        let cm = ControlMessage {
            msg: Some(control_message::Msg::AuthInit(AuthInit {
                username: "alice".to_string(),
                method: Some(auth_init::Method::Password(PasswordAuth {
                    password: "s3cret".to_string(),
                })),
            })),
        };
        let decoded = round_trip(&cm);
        assert!(
            matches!(decoded.msg, Some(control_message::Msg::AuthInit(_))),
            "only auth_init should be set"
        );
    }
}
