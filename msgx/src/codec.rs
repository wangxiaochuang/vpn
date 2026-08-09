use bytes::BytesMut;
use prost::Message;
use thiserror::Error;
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

pub const MAX_FRAME_LENGTH: usize = 65_536;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("codec error")]
    Codec(#[from] std::io::Error),
    #[error("decode error")]
    Decode(#[from] prost::DecodeError),
}

pub struct ProtoCodec<M> {
    inner: LengthDelimitedCodec,
    _marker: std::marker::PhantomData<M>,
}

impl<M> ProtoCodec<M> {
    pub fn new() -> Self {
        Self {
            inner: LengthDelimitedCodec::builder()
                .big_endian()
                .length_field_length(4)
                .max_frame_length(MAX_FRAME_LENGTH)
                .new_codec(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<M> Default for ProtoCodec<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Message> Encoder<M> for ProtoCodec<M> {
    type Error = FrameError;

    fn encode(&mut self, item: M, buf: &mut BytesMut) -> Result<(), Self::Error> {
        let payload = item.encode_to_vec();
        self.inner.encode(payload.into(), buf)?;
        Ok(())
    }
}

impl<M: Message + Default> Decoder for ProtoCodec<M> {
    type Item = M;
    type Error = FrameError;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(buf)? {
            Some(payload) => Ok(Some(M::decode(payload)?)),
            None => Ok(None),
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode_eof(buf)? {
            Some(payload) => Ok(Some(M::decode(payload)?)),
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
    use prost::Message;

    #[derive(Clone, PartialEq, prost::Message)]
    struct TestMsg {
        #[prost(string, tag = "1")]
        text: String,
        #[prost(uint32, tag = "2")]
        number: u32,
    }

    fn msg(text: &str, number: u32) -> TestMsg {
        TestMsg {
            text: text.to_string(),
            number,
        }
    }

    #[test]
    fn test_encode_length_prefix_is_big_endian_u32() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        for m in [msg("alice", 1), msg("data", 0), msg(&"x".repeat(100), 999)] {
            let mut buf = BytesMut::new();
            codec.encode(m, &mut buf).unwrap();
            assert!(buf.len() >= 4);
            let bytes: [u8; 4] = [buf[0], buf[1], buf[2], buf[3]];
            let as_big = u32::from_be_bytes(bytes);
            let as_little = u32::from_le_bytes(bytes);
            assert_eq!(as_big as usize, buf.len() - 4);
            assert_ne!(as_little as usize, buf.len() - 4);
        }
    }

    #[test]
    fn test_max_frame_length_constant_equals_64kib() {
        assert_eq!(MAX_FRAME_LENGTH, 65_536);
    }

    #[test]
    fn test_roundtrip_typical_message_preserves_fields() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        for m in [msg("alice", 1), msg("hello world", 42), msg("中文", 7)] {
            let mut buf = BytesMut::new();
            codec.encode(m.clone(), &mut buf).unwrap();
            let decoded = codec.decode(&mut buf).unwrap().unwrap();
            assert_eq!(decoded, m);
        }
    }

    #[test]
    fn test_roundtrip_empty_payload_frame_returns_default() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let mut buf = BytesMut::new();
        codec.encode(TestMsg::default(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, TestMsg::default());
    }

    #[test]
    fn test_decode_zero_length_prefix_returns_default() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&0u32.to_be_bytes());
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, TestMsg::default());
    }

    #[test]
    fn test_half_packet_length_prefix_partial_returns_none() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let mut full = BytesMut::new();
        codec.encode(msg("payload", 1), &mut full).unwrap();
        let prefix = &full[..4];

        for split in [1usize, 2, 3] {
            let mut buf = BytesMut::new();
            buf.extend_from_slice(&prefix[..split]);
            assert!(codec.decode(&mut buf).unwrap().is_none());
        }
    }

    #[test]
    fn test_half_packet_payload_partial_returns_none() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let mut full = BytesMut::new();
        codec.encode(msg("payload-here", 1), &mut full).unwrap();
        assert!(full.len() > 5);

        let mut buf = full.clone();
        buf.truncate(4 + 1);
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn test_half_packet_progressive_returns_message_when_complete() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let m = msg("payload", 1);
        let mut full = BytesMut::new();
        codec.encode(m.clone(), &mut full).unwrap();

        let mut buf = BytesMut::new();
        buf.extend_from_slice(&full[..3]);
        assert!(codec.decode(&mut buf).unwrap().is_none());
        buf.extend_from_slice(&full[3..]);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, m);
    }

    #[test]
    fn test_sticky_packet_two_frames_decode_in_order() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let m1 = msg("first", 1);
        let m2 = msg("second", 2);
        let mut buf = BytesMut::new();
        codec.encode(m1.clone(), &mut buf).unwrap();
        codec.encode(m2.clone(), &mut buf).unwrap();

        assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), m1);
        assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), m2);
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn test_encode_oversized_payload_returns_err() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let oversized = msg(&"x".repeat(MAX_FRAME_LENGTH), 1);
        let mut buf = BytesMut::new();
        assert!(oversized.encode_to_vec().len() > MAX_FRAME_LENGTH);
        let err = codec.encode(oversized, &mut buf).unwrap_err();
        assert!(matches!(err, FrameError::Codec(_)));
    }

    #[test]
    fn test_decode_oversized_length_prefix_returns_err() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let mut buf = BytesMut::new();
        let oversize = (MAX_FRAME_LENGTH + 1) as u32;
        buf.extend_from_slice(&oversize.to_be_bytes());
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, FrameError::Codec(_)));
    }

    #[test]
    fn test_decode_malformed_payload_returns_decode_error() {
        let mut codec = ProtoCodec::<TestMsg>::new();
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
        let mut codec = ProtoCodec::<TestMsg>::new();

        let oversized = msg(&"x".repeat(MAX_FRAME_LENGTH), 1);
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
        let mut codec = ProtoCodec::<TestMsg>::new();
        let mut full = BytesMut::new();
        codec.encode(msg("payload", 1), &mut full).unwrap();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&full[..3]);
        assert!(codec.decode_eof(&mut buf).is_err());
    }

    #[test]
    fn test_decode_eof_no_residual_returns_none() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let mut buf = BytesMut::new();
        assert!(codec.decode_eof(&mut buf).unwrap().is_none());
    }

    #[test]
    fn test_decode_eof_complete_frame_returns_message() {
        let mut codec = ProtoCodec::<TestMsg>::new();
        let m = msg("payload", 1);
        let mut buf = BytesMut::new();
        codec.encode(m.clone(), &mut buf).unwrap();
        let decoded = codec.decode_eof(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, m);
    }

    #[test]
    fn test_default_returns_working_codec() {
        let mut codec = ProtoCodec::<TestMsg>::default();
        let m = msg("payload", 1);
        let mut buf = BytesMut::new();
        codec.encode(m.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, m);
    }
}
