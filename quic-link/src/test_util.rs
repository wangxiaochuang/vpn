#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::Session;
use crate::tls::build_quinn_server_config;

pub fn repo_file(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("quic-link crate nested under repo root")
        .join(name)
}

#[derive(Debug)]
pub struct NoVerify;

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

pub fn no_verify_client_config() -> quinn::ClientConfig {
    let rustls_client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocols")
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoVerify))
    .with_no_client_auth();
    let quic_client = quinn::crypto::rustls::QuicClientConfig::try_from(Arc::new(rustls_client))
        .expect("client cfg");
    quinn::ClientConfig::new(Arc::new(quic_client))
}

pub async fn make_session_pair(cert: &Path, key: &Path) -> (Session, Session) {
    let server = build_test_server(cert, key);
    let addr = server.local_addr().expect("local addr");
    let accept_task = spawn_accept(server.clone());
    std::mem::forget(server);
    let client_conn = dial_no_verify(addr).await;
    let server_conn = accept_task
        .await
        .expect("accept task")
        .expect("server conn");
    (Session::new(server_conn), Session::new(client_conn))
}

fn build_test_server(cert: &Path, key: &Path) -> quinn::Endpoint {
    let cfg = build_quinn_server_config(cert, key).expect("server cfg");
    let addr = "127.0.0.1:0".parse().expect("addr");
    quinn::Endpoint::server(cfg, addr).expect("server endpoint")
}

fn spawn_accept(
    server: quinn::Endpoint,
) -> tokio::task::JoinHandle<Result<quinn::Connection, quinn::ConnectionError>> {
    tokio::spawn(async move {
        let incoming = server.accept().await.expect("server should accept");
        incoming.await
    })
}

async fn dial_no_verify(addr: SocketAddr) -> quinn::Connection {
    let client = quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("client");
    let conn = client
        .connect_with(no_verify_client_config(), addr, "localhost")
        .expect("dial")
        .await
        .expect("connect");
    std::mem::forget(client);
    conn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, PartialEq, prost::Message)]
    struct PairMsg {
        #[prost(string, tag = "1")]
        text: String,
    }

    #[tokio::test]
    async fn test_make_session_pair_streams_interoperate() {
        let (server, client) =
            make_session_pair(&repo_file("cert.pem"), &repo_file("key.pem")).await;

        let accept_task = tokio::spawn(async move { server.accept_stream::<PairMsg>().await });
        let mut tx = client.open_stream::<PairMsg>().await.unwrap();
        tx.send(PairMsg {
            text: "ping".to_string(),
        })
        .await
        .unwrap();
        let mut rx = accept_task.await.unwrap().unwrap();

        let got = rx.recv().await.unwrap().unwrap();
        assert_eq!(got.text, "ping");
    }

    #[test]
    fn test_repo_file_points_at_repo_root() {
        let cert = repo_file("cert.pem");
        assert!(
            cert.is_file(),
            "cert.pem should exist at {}",
            cert.display()
        );
    }
}
