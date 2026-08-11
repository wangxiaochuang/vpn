#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;

use quic_link::{DatagramRx, DatagramTx, PacketSink, PacketSource};

fn repo(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("quic-link crate nested under repo root")
        .join(p)
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

fn build_server_config() -> quinn::ServerConfig {
    quic_link::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem")).expect("server cfg")
}

fn build_client_config() -> quinn::ClientConfig {
    let rustls_client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoVerify))
    .with_no_client_auth();
    let quic_client =
        quinn::crypto::rustls::QuicClientConfig::try_from(Arc::new(rustls_client)).unwrap();
    quinn::ClientConfig::new(Arc::new(quic_client))
}

struct ConnectionPair {
    _server_endpoint: quinn::Endpoint,
    server: quinn::Connection,
    client: quinn::Connection,
}

async fn make_connection_pair() -> ConnectionPair {
    let server =
        quinn::Endpoint::server(build_server_config(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = server.local_addr().unwrap();
    let server_for_accept = server.clone();
    let accept_task = tokio::spawn(async move {
        server_for_accept
            .accept()
            .await
            .expect("accept")
            .await
            .expect("server accept conn")
    });
    let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let client_conn = client
        .connect_with(build_client_config(), addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    std::mem::forget(client);
    let server_conn = accept_task.await.unwrap();
    ConnectionPair {
        _server_endpoint: server,
        server: server_conn,
        client: client_conn,
    }
}

#[tokio::test]
async fn test_session_datagram_tx_rx_roundtrip() {
    let pair = make_connection_pair().await;
    let mut tx = DatagramTx::new(pair.client.clone());
    let mut rx = DatagramRx::new(pair.server.clone());

    tx.send(Bytes::from_static(b"hello")).await.unwrap();
    let received = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("recv should not hang")
        .unwrap();
    assert_eq!(received, Bytes::from_static(b"hello"));

    tx.send(Bytes::from_static(b"world")).await.unwrap();
    let received = rx.recv().await.unwrap();
    assert_eq!(received, Bytes::from_static(b"world"));
}

#[tokio::test]
async fn test_datagram_tx_clone_both_send() {
    let pair = make_connection_pair().await;
    let mut tx1 = DatagramTx::new(pair.client.clone());
    let mut tx2 = tx1.clone();
    let mut rx = DatagramRx::new(pair.server.clone());

    tx1.send(Bytes::from_static(b"from-tx1")).await.unwrap();
    tx2.send(Bytes::from_static(b"from-tx2")).await.unwrap();

    let mut got = Vec::new();
    got.push(rx.recv().await.unwrap());
    got.push(rx.recv().await.unwrap());
    assert!(got.contains(&Bytes::from_static(b"from-tx1")));
    assert!(got.contains(&Bytes::from_static(b"from-tx2")));
}
