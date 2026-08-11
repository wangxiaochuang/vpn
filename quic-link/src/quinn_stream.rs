use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use msgx::channel::{ByteStream, Channel};

pub struct QuinnStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl QuinnStream {
    pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self { send, recv }
    }
}

impl AsyncRead for QuinnStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buf)
    }
}

impl AsyncWrite for QuinnStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
    }
}

pub async fn open_bi<M: Message + Default>(
    conn: &quinn::Connection,
) -> Result<Channel<M>, quinn::ConnectionError> {
    let (send, recv) = conn.open_bi().await?;
    Ok(Channel::from_io(ByteStream::new(recv, send)))
}

pub async fn accept_bi<M: Message + Default>(
    conn: &quinn::Connection,
) -> Result<Channel<M>, quinn::ConnectionError> {
    let (send, recv) = conn.accept_bi().await?;
    Ok(Channel::from_io(ByteStream::new(recv, send)))
}
