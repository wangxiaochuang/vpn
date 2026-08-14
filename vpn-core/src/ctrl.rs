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
    fn test_server_hello_round_trip_preserves_protocol_version() {
        let hello = ServerHello {
            protocol_version: 1,
        };
        let decoded = round_trip(&hello);
        assert_eq!(decoded.protocol_version, 1);
    }

    #[test]
    fn test_control_message_server_hello_round_trip_fidelity() {
        let cm = ControlMessage {
            msg: Some(control_message::Msg::ServerHello(ServerHello {
                protocol_version: 42,
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
    fn test_control_message_all_six_branches_round_trip() {
        for variant in all_six_variants() {
            let original = ControlMessage { msg: Some(variant) };
            let decoded = round_trip(&original);
            assert_same_variant(original.msg.as_ref(), decoded.msg.as_ref());
        }
    }

    fn all_six_variants() -> [control_message::Msg; 6] {
        use control_message::Msg;
        [
            Msg::ServerHello(ServerHello::default()),
            Msg::AuthRequest(AuthRequest::default()),
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
            })),
        };
        let decoded = round_trip(&cm);
        assert!(
            matches!(decoded.msg, Some(control_message::Msg::ServerHello(_))),
            "only server_hello should be set"
        );
    }
}
