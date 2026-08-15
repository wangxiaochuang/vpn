use prost::Message;

use msgx::Channel;

use crate::datagram::{DatagramRx, DatagramTx};
use crate::quinn_stream;

/// 一个已建立的 QUIC + TLS 连接，私有持有底层 `quinn::Connection`。
///
/// `Session` 的所有公开方法签名均不含 `quinn::` 类型。datagram 在创建后立即可用；
/// stream 需显式 [`Session::open_stream`] / [`Session::accept_stream`]。
#[derive(Clone, Debug)]
pub struct Session {
    conn: quinn::Connection,
}

impl Session {
    #[doc(hidden)]
    pub fn new(conn: quinn::Connection) -> Self {
        Self { conn }
    }

    pub fn close(&self, code: u64, reason: &[u8]) {
        let code = quinn::VarInt::try_from(code).unwrap_or(quinn::VarInt::MAX);
        self.conn.close(code, reason);
    }

    pub fn id(&self) -> usize {
        self.conn.stable_id()
    }

    pub fn datagram_tx(&self) -> DatagramTx {
        DatagramTx::new(self.conn.clone())
    }

    pub fn datagram_rx(&self) -> DatagramRx {
        DatagramRx::new(self.conn.clone())
    }

    pub async fn open_stream<M: Message + Default>(&self) -> Result<Channel<M>, SessionError> {
        quinn_stream::open_bi(&self.conn)
            .await
            .map_err(SessionError)
    }

    pub async fn accept_stream<M: Message + Default>(&self) -> Result<Channel<M>, SessionError> {
        quinn_stream::accept_bi(&self.conn)
            .await
            .map_err(SessionError)
    }

    /// 等待连接关闭（对端主动 close、本地 close、或传输层错误）。
    ///
    /// 返回类型刻意收敛为 `()` 以保持 `Session` 公开 API 不暴露 quinn 类型；
    /// 调用方只关心"连接已关闭"语义（如 `AuthDenied` 确认握手中的兜底等待）。
    pub async fn closed(&self) {
        self.conn.closed().await;
    }

    /// # 高级 API / 逃生口（escape hatch）
    ///
    /// 返回底层 `&quinn::Connection`，用于 `Session` 未覆盖的高级能力
    /// （如 `export_keying_material`）。**常规用法不应依赖此方法**。
    #[doc(hidden)]
    pub fn inner(&self) -> &quinn::Connection {
        &self.conn
    }
}

#[derive(Debug, thiserror::Error)]
#[error("session stream error: {0}")]
pub struct SessionError(#[from] quinn::ConnectionError);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::test_util::{make_session_pair, repo_file};

    async fn session_pair() -> (Session, Session) {
        make_session_pair(&repo_file("cert.pem"), &repo_file("key.pem")).await
    }

    #[test]
    fn test_session_public_api_has_no_quinn_types() {
        fn check_close(s: &Session, c: u64, r: &[u8]) {
            s.close(c, r);
        }
        fn check_id(s: &Session) -> usize {
            s.id()
        }
        fn check_dtx(s: &Session) -> DatagramTx {
            s.datagram_tx()
        }
        fn check_drx(s: &Session) -> DatagramRx {
            s.datagram_rx()
        }
        let _ = (check_close, check_id, check_dtx, check_drx);
    }

    #[tokio::test]
    async fn test_closed_returns_after_remote_close() {
        let (server_sess, client_sess) = session_pair().await;
        client_sess.close(0, b"test-close");
        tokio::time::timeout(Duration::from_secs(2), server_sess.closed())
            .await
            .expect("closed() must resolve after remote close, not block forever");
    }

    #[tokio::test]
    async fn test_closed_returns_after_local_close() {
        let (server_sess, _client_sess) = session_pair().await;
        server_sess.close(0, b"local-close");
        tokio::time::timeout(Duration::from_secs(2), server_sess.closed())
            .await
            .expect("closed() must resolve after local close, not block forever");
    }
}
