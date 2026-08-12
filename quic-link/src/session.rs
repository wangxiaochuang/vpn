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
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn repo(p: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("quic-link crate nested under repo root")
            .join(p)
    }

    async fn make_session_pair() -> (Session, Session) {
        let server = build_test_server();
        let addr = server.local_addr().expect("local addr");
        let accept_task = spawn_test_accept(server.clone());
        let client_conn = dial_test_client(addr).await;
        let server_conn = accept_task
            .await
            .expect("accept task")
            .expect("server conn");
        (Session::new(server_conn), Session::new(client_conn))
    }

    fn build_test_server() -> quinn::Endpoint {
        let cfg = crate::tls::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem"))
            .expect("server cfg");
        quinn::Endpoint::server(cfg, "127.0.0.1:0".parse().expect("addr")).expect("server endpoint")
    }

    fn spawn_test_accept(
        server: quinn::Endpoint,
    ) -> tokio::task::JoinHandle<Result<quinn::Connection, quinn::ConnectionError>> {
        tokio::spawn(async move {
            let incoming = server.accept().await.expect("server should accept");
            incoming.await
        })
    }

    async fn dial_test_client(addr: std::net::SocketAddr) -> quinn::Connection {
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("client");
        let conn = client
            .connect_with(build_no_verify_client_config(), addr, "localhost")
            .expect("dial")
            .await
            .expect("connect");
        std::mem::forget(client);
        conn
    }

    fn build_no_verify_client_config() -> quinn::ClientConfig {
        let rustls_client = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("protocols")
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(NoVerify))
        .with_no_client_auth();
        let quic_client =
            quinn::crypto::rustls::QuicClientConfig::try_from(std::sync::Arc::new(rustls_client))
                .expect("client cfg");
        quinn::ClientConfig::new(std::sync::Arc::new(quic_client))
    }

    #[derive(Debug)]
    struct NoVerify;

    impl rustls::client::danger::ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls_pki_types::CertificateDer<'_>,
            _intermediates: &[rustls_pki_types::CertificateDer<'_>],
            _server_name: &rustls_pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls_pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls_pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls_pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ED25519,
                rustls::SignatureScheme::RSA_PSS_SHA256,
            ]
        }
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
        let (server_sess, client_sess) = make_session_pair().await;
        client_sess.close(0, b"test-close");
        tokio::time::timeout(Duration::from_secs(2), server_sess.closed())
            .await
            .expect("closed() must resolve after remote close, not block forever");
    }

    #[tokio::test]
    async fn test_closed_returns_after_local_close() {
        let (server_sess, _client_sess) = make_session_pair().await;
        server_sess.close(0, b"local-close");
        tokio::time::timeout(Duration::from_secs(2), server_sess.closed())
            .await
            .expect("closed() must resolve after local close, not block forever");
    }
}
