#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use quic_link::quinn_stream::{accept_bi, open_bi};

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

fn spawn_server_accept(server: &quinn::Endpoint) -> tokio::task::JoinHandle<quinn::Connection> {
    let server_for_accept = server.clone();
    tokio::spawn(async move {
        let incoming = server_for_accept.accept().await.expect("accept");
        incoming.await.expect("server accept conn")
    })
}

async fn dial_client(addr: std::net::SocketAddr) -> quinn::Connection {
    let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let conn = client
        .connect_with(build_client_config(), addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    std::mem::forget(client);
    conn
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
    let accept_task = spawn_server_accept(&server);
    let client_conn = dial_client(addr).await;
    let server_conn = accept_task.await.unwrap();
    ConnectionPair {
        _server_endpoint: server,
        server: server_conn,
        client: client_conn,
    }
}

#[tokio::test]
async fn test_open_bi_and_accept_bi_channels_communicate_bidirectionally() {
    let pair = make_connection_pair().await;
    let client_conn = pair.client.clone();

    let client_task = tokio::spawn(async move {
        let mut ch = open_bi::<TestMsg>(&client_conn).await.unwrap();
        ch.send(msg("hello", 1)).await.unwrap();
        ch
    });
    let mut server_ch = accept_bi::<TestMsg>(&pair.server).await.unwrap();
    let mut client_ch = client_task.await.unwrap();

    let received = server_ch.recv().await.unwrap().unwrap();
    assert_eq!(received, msg("hello", 1));

    server_ch.send(msg("world", 2)).await.unwrap();
    let echo = client_ch.recv().await.unwrap().unwrap();
    assert_eq!(echo, msg("world", 2));
}

#[tokio::test]
async fn test_accept_bi_recv_returns_none_when_client_stream_closes() {
    let pair = make_connection_pair().await;
    let client_conn = pair.client.clone();

    let client_task = tokio::spawn(async move {
        let mut ch = open_bi::<TestMsg>(&client_conn).await.unwrap();
        ch.send(msg("first", 1)).await.unwrap();
        ch
    });
    let mut server_ch = accept_bi::<TestMsg>(&pair.server).await.unwrap();
    let client_ch = client_task.await.unwrap();

    let received = server_ch.recv().await.unwrap().unwrap();
    assert_eq!(received, msg("first", 1));

    drop(client_ch);

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(3), server_ch.recv())
        .await
        .expect("recv resolves after client stream closes");
    assert!(matches!(outcome, Ok(None)) || outcome.is_err());
}
