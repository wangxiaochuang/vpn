use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use prost::Message;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::timeout;
use tokio_util::codec::Framed;

use crate::codec::{FrameError, ProtoCodec};

pub type SendError = FrameError;
pub type RecvError = FrameError;

#[derive(Debug, Error)]
pub enum SendTimeoutError {
    #[error("send timed out")]
    Timeout,
    #[error("stream closed")]
    Closed,
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum RecvTimeoutError {
    #[error("recv timed out")]
    Timeout,
    #[error("stream closed")]
    Closed,
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

pub struct ByteStream {
    read: Box<dyn AsyncRead + Unpin + Send>,
    write: Box<dyn AsyncWrite + Unpin + Send>,
}

impl ByteStream {
    pub fn new<R, W>(read: R, write: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self {
            read: Box::new(read),
            write: Box::new(write),
        }
    }
}

impl AsyncRead for ByteStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.read).poll_read(cx, buf)
    }
}

impl AsyncWrite for ByteStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.write).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.write).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.write).poll_shutdown(cx)
    }
}

pub struct Channel<M> {
    framed: Framed<ByteStream, ProtoCodec<M>>,
}

impl<M: Message + Default> Channel<M> {
    pub fn from_io(io: ByteStream) -> Self {
        Self {
            framed: Framed::new(io, ProtoCodec::new()),
        }
    }

    pub async fn send(&mut self, msg: M) -> Result<(), SendError> {
        self.framed.send(msg).await
    }

    pub async fn recv(&mut self) -> Result<Option<M>, RecvError> {
        match self.framed.next().await {
            None => Ok(None),
            Some(Ok(m)) => Ok(Some(m)),
            Some(Err(e)) => Err(e),
        }
    }

    pub async fn send_timeout(&mut self, msg: M, dur: Duration) -> Result<(), SendTimeoutError> {
        match timeout(dur, self.send(msg)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(FrameError::Codec(e))) => Err(send_closed_or_io(e)),
            Ok(Err(FrameError::Decode(e))) => Err(SendTimeoutError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                e,
            ))),
            Err(_) => Err(SendTimeoutError::Timeout),
        }
    }

    pub async fn recv_timeout(&mut self, dur: Duration) -> Result<M, RecvTimeoutError> {
        match timeout(dur, self.recv()).await {
            Ok(Ok(Some(m))) => Ok(m),
            Ok(Ok(None)) => Err(RecvTimeoutError::Closed),
            Ok(Err(FrameError::Codec(e))) => Err(recv_closed_or_io(e)),
            Ok(Err(FrameError::Decode(e))) => Err(RecvTimeoutError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                e,
            ))),
            Err(_) => Err(RecvTimeoutError::Timeout),
        }
    }

    pub fn split(self) -> (Sender<M>, Receiver<M>) {
        let (sink, stream) = self.framed.split();
        (Sender { sink }, Receiver { stream })
    }
}

pub struct Sender<M> {
    sink: futures::stream::SplitSink<Framed<ByteStream, ProtoCodec<M>>, M>,
}

impl<M: Message + Default> Sender<M> {
    pub async fn send(&mut self, msg: M) -> Result<(), SendError> {
        self.sink.send(msg).await
    }

    pub async fn send_timeout(&mut self, msg: M, dur: Duration) -> Result<(), SendTimeoutError> {
        match timeout(dur, self.send(msg)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(FrameError::Codec(e))) => Err(send_closed_or_io(e)),
            Ok(Err(FrameError::Decode(e))) => Err(SendTimeoutError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                e,
            ))),
            Err(_) => Err(SendTimeoutError::Timeout),
        }
    }
}

pub struct Receiver<M> {
    stream: futures::stream::SplitStream<Framed<ByteStream, ProtoCodec<M>>>,
}

impl<M: Message + Default> Receiver<M> {
    pub async fn recv(&mut self) -> Result<Option<M>, RecvError> {
        match self.stream.next().await {
            None => Ok(None),
            Some(Ok(m)) => Ok(Some(m)),
            Some(Err(e)) => Err(e),
        }
    }

    pub async fn recv_timeout(&mut self, dur: Duration) -> Result<M, RecvTimeoutError> {
        match timeout(dur, self.recv()).await {
            Ok(Ok(Some(m))) => Ok(m),
            Ok(Ok(None)) => Err(RecvTimeoutError::Closed),
            Ok(Err(FrameError::Codec(e))) => Err(recv_closed_or_io(e)),
            Ok(Err(FrameError::Decode(e))) => Err(RecvTimeoutError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                e,
            ))),
            Err(_) => Err(RecvTimeoutError::Timeout),
        }
    }
}

fn send_closed_or_io(e: io::Error) -> SendTimeoutError {
    if is_closed_kind(e.kind()) {
        SendTimeoutError::Closed
    } else {
        SendTimeoutError::Io(e)
    }
}

fn recv_closed_or_io(e: io::Error) -> RecvTimeoutError {
    if is_closed_kind(e.kind()) {
        RecvTimeoutError::Closed
    } else {
        RecvTimeoutError::Io(e)
    }
}

fn is_closed_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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

    fn pair() -> (Channel<TestMsg>, Channel<TestMsg>) {
        let (a, b) = tokio::io::duplex(8192);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let ch_a = Channel::from_io(ByteStream::new(ar, aw));
        let ch_b = Channel::from_io(ByteStream::new(br, bw));
        (ch_a, ch_b)
    }

    #[tokio::test]
    async fn test_send_message_is_received_by_other_side() {
        let (mut ch_a, mut ch_b) = pair();
        ch_a.send(msg("alice", 42)).await.unwrap();
        let received = ch_b.recv().await.unwrap().unwrap();
        assert_eq!(received, msg("alice", 42));
    }

    #[tokio::test]
    async fn test_multiple_messages_preserve_order() {
        let (mut ch_a, mut ch_b) = pair();
        ch_a.send(msg("first", 1)).await.unwrap();
        ch_a.send(msg("second", 2)).await.unwrap();
        ch_a.send(msg("third", 3)).await.unwrap();

        assert_eq!(ch_b.recv().await.unwrap().unwrap(), msg("first", 1));
        assert_eq!(ch_b.recv().await.unwrap().unwrap(), msg("second", 2));
        assert_eq!(ch_b.recv().await.unwrap().unwrap(), msg("third", 3));
    }

    #[tokio::test]
    async fn test_recv_returns_none_after_peer_closed() {
        let (ch_a, mut ch_b) = pair();
        drop(ch_a);
        let result = tokio::time::timeout(Duration::from_secs(2), ch_b.recv())
            .await
            .expect("recv should resolve after peer drop");
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_split_sender_and_receiver_usable_in_separate_tasks() {
        let (ch_a, ch_b) = pair();
        let (mut sender_a, _receiver_a) = ch_a.split();
        let (_sender_b, mut receiver_b) = ch_b.split();

        let send_task = tokio::spawn(async move {
            sender_a.send(msg("hello", 1)).await.unwrap();
            sender_a.send(msg("world", 2)).await.unwrap();
        });
        let recv_task = tokio::spawn(async move {
            let m1 = receiver_b.recv().await.unwrap().unwrap();
            let m2 = receiver_b.recv().await.unwrap().unwrap();
            (m1, m2)
        });

        send_task.await.unwrap();
        let (m1, m2) = recv_task.await.unwrap();
        assert_eq!(m1, msg("hello", 1));
        assert_eq!(m2, msg("world", 2));
    }

    #[tokio::test]
    async fn test_recv_timeout_returns_message_when_sent_in_time() {
        let (mut ch_a, mut ch_b) = pair();
        ch_a.send(msg("ping", 7)).await.unwrap();
        let m = ch_b.recv_timeout(Duration::from_secs(2)).await.unwrap();
        assert_eq!(m, msg("ping", 7));
    }

    #[tokio::test]
    async fn test_recv_timeout_returns_timeout_when_no_message() {
        let (_ch_a, mut ch_b) = pair();
        let err = ch_b
            .recv_timeout(Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, RecvTimeoutError::Timeout));
    }

    #[tokio::test]
    async fn test_recv_timeout_does_not_consume_buffer_on_timeout() {
        let (mut ch_a, mut ch_b) = pair();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = ch_b
            .recv_timeout(Duration::from_millis(10))
            .await
            .unwrap_err();
        ch_a.send(msg("late", 1)).await.unwrap();
        let m = ch_b.recv().await.unwrap().unwrap();
        assert_eq!(m, msg("late", 1));
    }

    #[tokio::test]
    async fn test_recv_timeout_returns_closed_after_peer_drop() {
        let (ch_a, mut ch_b) = pair();
        drop(ch_a);
        let err = ch_b.recv_timeout(Duration::from_secs(2)).await.unwrap_err();
        assert!(matches!(err, RecvTimeoutError::Closed));
    }

    #[tokio::test]
    async fn test_send_timeout_succeeds_within_deadline() {
        let (mut ch_a, mut ch_b) = pair();
        ch_a.send_timeout(msg("ok", 1), Duration::from_secs(2))
            .await
            .unwrap();
        let m = ch_b.recv().await.unwrap().unwrap();
        assert_eq!(m, msg("ok", 1));
    }

    #[tokio::test]
    async fn test_recv_timeout_error_variants_distinguishable() {
        let (mut ch_a, mut ch_b) = pair();

        let timeout_err = ch_b
            .recv_timeout(Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(matches!(timeout_err, RecvTimeoutError::Timeout));

        ch_a.send(msg("delivered", 1)).await.unwrap();
        let _ = ch_b.recv_timeout(Duration::from_secs(2)).await.unwrap();

        drop(ch_a);
        let closed_err = ch_b.recv_timeout(Duration::from_secs(2)).await.unwrap_err();
        assert!(matches!(closed_err, RecvTimeoutError::Closed));
    }
}
