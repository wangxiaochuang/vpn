pub use msgx::FrameError;

use crate::ctrl::ControlMessage;

pub type ControlCodec = msgx::ProtoCodec<ControlMessage>;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::vpn::control_message::Msg;
    use tokio_util::codec::{Decoder, Encoder};

    fn auth_init() -> ControlMessage {
        use crate::vpn::auth_init::Method;
        ControlMessage {
            msg: Some(Msg::AuthInit(crate::vpn::AuthInit {
                username: "alice".to_string(),
                method: Some(Method::Password(crate::vpn::PasswordAuth {
                    password: "s3cret".to_string(),
                })),
            })),
        }
    }

    fn auth_ok() -> ControlMessage {
        ControlMessage {
            msg: Some(Msg::AuthOk(crate::vpn::AuthOk {
                assigned_ip: "10.0.0.2".to_string(),
                subnet: "10.0.0.0/24".to_string(),
                gateway: "10.0.0.1".to_string(),
                mtu: 1280,
                routes: vec![],
            })),
        }
    }

    fn auth_denied() -> ControlMessage {
        ControlMessage {
            msg: Some(Msg::AuthDenied(crate::vpn::AuthDenied {
                reason: crate::vpn::DenyReason::AuthFailed as i32,
            })),
        }
    }

    fn heartbeat() -> ControlMessage {
        ControlMessage {
            msg: Some(Msg::Heartbeat(crate::vpn::Heartbeat {})),
        }
    }

    fn disconnect(reason: &str) -> ControlMessage {
        ControlMessage {
            msg: Some(Msg::Disconnect(crate::vpn::Disconnect {
                reason: reason.to_string(),
            })),
        }
    }

    #[test]
    fn test_control_branches_roundtrip_preserve_fields() {
        let mut codec = ControlCodec::new();
        for msg in [
            auth_init(),
            auth_ok(),
            auth_denied(),
            heartbeat(),
            disconnect("superseded"),
        ] {
            let mut buf = bytes::BytesMut::new();
            codec.encode(msg.clone(), &mut buf).unwrap();
            let decoded = codec.decode(&mut buf).unwrap().unwrap();
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn test_heartbeat_empty_payload_roundtrip_preserves_fields() {
        let mut codec = ControlCodec::new();
        let mut buf = bytes::BytesMut::new();
        let msg = heartbeat();
        codec.encode(msg.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, msg);
    }
}
