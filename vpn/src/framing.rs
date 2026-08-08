use bytes::BytesMut;
use prost::Message;
use thiserror::Error;
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use crate::ctrl::ControlMessage;
use crate::ctrl::MAX_FRAME_LENGTH;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("codec error")]
    Codec(#[from] std::io::Error),
    #[error("decode error")]
    Decode(#[from] prost::DecodeError),
}

pub struct ControlCodec {
    inner: LengthDelimitedCodec,
}

impl ControlCodec {
    pub fn new() -> Self {
        Self {
            inner: LengthDelimitedCodec::builder()
                .big_endian()
                .length_field_length(4)
                .max_frame_length(MAX_FRAME_LENGTH)
                .new_codec(),
        }
    }
}

impl Default for ControlCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder<ControlMessage> for ControlCodec {
    type Error = FrameError;

    fn encode(&mut self, item: ControlMessage, buf: &mut BytesMut) -> Result<(), Self::Error> {
        let payload = item.encode_to_vec();
        self.inner.encode(payload.into(), buf)?;
        Ok(())
    }
}

impl Decoder for ControlCodec {
    type Item = ControlMessage;
    type Error = FrameError;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(buf)? {
            Some(payload) => Ok(Some(ControlMessage::decode(payload)?)),
            None => Ok(None),
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode_eof(buf)? {
            Some(payload) => Ok(Some(ControlMessage::decode(payload)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]
mod tests {
    use super::*;
    use crate::vpn::control_message::Msg;

    fn auth_request() -> ControlMessage {
        ControlMessage {
            msg: Some(Msg::AuthRequest(crate::vpn::AuthRequest {
                username: "alice".to_string(),
                password: "s3cret".to_string(),
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
    fn test_encode_length_prefix_is_big_endian_u32() {
        let mut codec = ControlCodec::new();
        for msg in [auth_request(), auth_ok(), heartbeat()] {
            let mut buf = BytesMut::new();
            codec.encode(msg, &mut buf).unwrap();
            assert!(buf.len() >= 4);
            let bytes: [u8; 4] = [buf[0], buf[1], buf[2], buf[3]];
            let as_big = u32::from_be_bytes(bytes);
            let as_little = u32::from_le_bytes(bytes);
            assert_eq!(as_big as usize, buf.len() - 4);
            assert_ne!(as_little as usize, buf.len() - 4);
        }
    }

    #[test]
    fn test_roundtrip_all_branches_preserve_fields() {
        let mut codec = ControlCodec::new();
        for msg in [
            auth_request(),
            auth_ok(),
            auth_denied(),
            heartbeat(),
            disconnect("superseded"),
        ] {
            let mut buf = BytesMut::new();
            codec.encode(msg.clone(), &mut buf).unwrap();
            let decoded = codec.decode(&mut buf).unwrap().unwrap();
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn test_roundtrip_heartbeat_preserves_fields() {
        let mut codec = ControlCodec::new();
        let mut buf = BytesMut::new();
        let msg = heartbeat();
        codec.encode(msg.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_decode_zero_length_payload_frame_returns_default() {
        let mut codec = ControlCodec::new();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&0u32.to_be_bytes());
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, ControlMessage::default());
    }

    #[test]
    fn test_half_packet_length_prefix_partial_returns_none() {
        let mut codec = ControlCodec::new();
        let mut full = BytesMut::new();
        codec.encode(heartbeat(), &mut full).unwrap();
        let prefix = &full[..4];

        for split in [1usize, 2, 3] {
            let mut buf = BytesMut::new();
            buf.extend_from_slice(&prefix[..split]);
            assert!(codec.decode(&mut buf).unwrap().is_none());
        }
    }

    #[test]
    fn test_half_packet_payload_partial_returns_none() {
        let mut codec = ControlCodec::new();
        let mut full = BytesMut::new();
        codec.encode(auth_request(), &mut full).unwrap();
        assert!(full.len() > 5);

        let mut buf = full.clone();
        buf.truncate(4 + 1);
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn test_half_packet_full_returns_message() {
        let mut codec = ControlCodec::new();
        let msg = auth_request();
        let mut full = BytesMut::new();
        codec.encode(msg.clone(), &mut full).unwrap();

        let mut buf = BytesMut::new();
        buf.extend_from_slice(&full[..3]);
        assert!(codec.decode(&mut buf).unwrap().is_none());
        buf.extend_from_slice(&full[3..]);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_sticky_packet_two_frames_decode_in_order() {
        let mut codec = ControlCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(heartbeat(), &mut buf).unwrap();
        codec.encode(auth_request(), &mut buf).unwrap();

        let first = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(first, heartbeat());
        let second = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(second, auth_request());
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn test_encode_oversized_payload_returns_err() {
        let mut codec = ControlCodec::new();
        let oversized = disconnect(&"x".repeat(MAX_FRAME_LENGTH));
        let mut buf = BytesMut::new();
        let payload_len = oversized.encode_to_vec().len();
        assert!(payload_len > MAX_FRAME_LENGTH);
        let err = codec.encode(oversized, &mut buf).unwrap_err();
        assert!(matches!(err, FrameError::Codec(_)));
    }

    #[test]
    fn test_decode_oversized_length_prefix_returns_err() {
        let mut codec = ControlCodec::new();
        let mut buf = BytesMut::new();
        let oversize = (MAX_FRAME_LENGTH + 1) as u32;
        buf.extend_from_slice(&oversize.to_be_bytes());
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, FrameError::Codec(_)));
    }

    #[test]
    fn test_decode_malformed_payload_returns_decode_error() {
        let mut codec = ControlCodec::new();
        let mut buf = BytesMut::new();
        let payload: [u8; 8] = [0xFF; 8];
        let len = payload.len() as u32;
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&payload);
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, FrameError::Decode(_)));
    }

    #[test]
    fn test_error_variants_distinguish_codec_and_decode() {
        let mut codec = ControlCodec::new();

        let oversized = disconnect(&"x".repeat(MAX_FRAME_LENGTH));
        let mut buf = BytesMut::new();
        let codec_err = codec.encode(oversized, &mut buf).unwrap_err();
        assert!(matches!(codec_err, FrameError::Codec(_)));

        let mut buf2 = BytesMut::new();
        let payload: [u8; 8] = [0xFF; 8];
        let len = payload.len() as u32;
        buf2.extend_from_slice(&len.to_be_bytes());
        buf2.extend_from_slice(&payload);
        let decode_err = codec.decode(&mut buf2).unwrap_err();
        assert!(matches!(decode_err, FrameError::Decode(_)));
    }

    #[test]
    fn test_decode_eof_residual_half_frame_returns_err() {
        let mut codec = ControlCodec::new();
        let mut full = BytesMut::new();
        codec.encode(heartbeat(), &mut full).unwrap();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&full[..3]);
        assert!(codec.decode_eof(&mut buf).is_err());
    }

    #[test]
    fn test_decode_eof_no_residual_returns_none() {
        let mut codec = ControlCodec::new();
        let mut buf = BytesMut::new();
        assert!(codec.decode_eof(&mut buf).unwrap().is_none());
    }

    #[test]
    fn test_decode_eof_complete_frame_returns_message() {
        let mut codec = ControlCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(heartbeat(), &mut buf).unwrap();
        let decoded = codec.decode_eof(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, heartbeat());
    }

    #[test]
    fn test_default_returns_working_codec() {
        let mut codec = ControlCodec::default();
        let mut buf = BytesMut::new();
        codec.encode(heartbeat(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, heartbeat());
    }
}
