#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use argon2::Argon2;
use argon2::PasswordHasher;
use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use ipnet::Ipv4Net;
use tokio_util::sync::CancellationToken;
use vpn::auth::UserStore;
use vpn::config::ServerConfig;
use vpn::ipam::IpPool;
use vpn::route::SessionRegistry;
use vpn::server::{ServerState, SharedState};

pub const ALICE_PASSWORD: &str = "s3cret";

pub fn hash_password(pw: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .unwrap()
        .to_string()
}

pub fn alice_users() -> Vec<(String, String)> {
    vec![("alice".to_string(), hash_password(ALICE_PASSWORD))]
}

pub fn repo(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("vpn crate nested under repo root")
        .join(p)
}

pub fn test_config() -> ServerConfig {
    ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        tun_subnet: Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
        mtu: 1280,
        cert: repo("cert.pem"),
        key: repo("key.pem"),
        users: vec![vpn::config::UserConfig {
            username: "alice".to_string(),
            password_hash: hash_password(ALICE_PASSWORD),
        }],
    }
}

pub fn test_state_with_subnet(subnet: Ipv4Net, users: Vec<(String, String)>) -> SharedState {
    let user_pairs: Vec<(String, String)> = users;
    let store = UserStore::from_users(user_pairs).unwrap();
    let pool = IpPool::new(subnet).unwrap();
    let config = ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        tun_subnet: subnet,
        mtu: 1280,
        cert: repo("cert.pem"),
        key: repo("key.pem"),
        users: vec![],
    };
    Arc::new(ServerState {
        users: store,
        pool: std::sync::Mutex::new(pool),
        registry: std::sync::Mutex::new(SessionRegistry::new()),
        tun: None,
        config: Arc::new(config),
    })
}

pub async fn make_test_state() -> SharedState {
    test_state_with_subnet(
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
        alice_users(),
    )
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

pub fn client_endpoint() -> quinn::Endpoint {
    quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap()
}

pub fn client_config() -> quinn::ClientConfig {
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

pub struct ConnectionPair {
    _endpoint: quinn::Endpoint,
    pub server: quinn::Connection,
    pub client: quinn::Connection,
}

pub async fn make_connected_pair() -> ConnectionPair {
    let server_cfg = vpn::tls::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem"))
        .expect("server cfg");
    let server = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap())
        .expect("server endpoint");

    let addr = server.local_addr().unwrap();
    let server_for_accept = server.clone();
    let accept_task = tokio::spawn(async move {
        let incoming = server_for_accept.accept().await;
        if let Some(incoming) = incoming {
            let conn = incoming.await.expect("server accept conn");
            Some(conn)
        } else {
            None
        }
    });

    let client = client_endpoint();
    let client_conn = client
        .connect_with(client_config(), addr, "localhost")
        .expect("dial")
        .await
        .expect("connect");

    let server_conn = accept_task.await.unwrap().unwrap();
    std::mem::forget(client);

    ConnectionPair {
        _endpoint: server,
        server: server_conn,
        client: client_conn,
    }
}

pub async fn start_test_server(
    users: Vec<(String, String)>,
    subnet: Ipv4Net,
) -> (quinn::Endpoint, SharedState) {
    let (endpoint, state, _shutdown) = start_test_server_with_shutdown(users, subnet).await;
    (endpoint, state)
}

pub async fn start_test_server_with_shutdown(
    users: Vec<(String, String)>,
    subnet: Ipv4Net,
) -> (quinn::Endpoint, SharedState, CancellationToken) {
    let server_cfg = vpn::tls::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem"))
        .expect("server cfg");
    let endpoint = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap())
        .expect("server endpoint");
    let state = test_state_with_subnet(subnet, users);

    let shutdown = CancellationToken::new();
    let accept_endpoint = endpoint.clone();
    let state_clone = state.clone();
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            if let Ok(conn) = incoming.await {
                let state = state_clone.clone();
                let ct = shutdown_clone.clone();
                tokio::spawn(async move {
                    let _ = vpn::server::handle_conn(conn, state, ct).await;
                });
            }
        }
    });

    (endpoint, state, shutdown)
}

pub async fn test_client_connect(addr: std::net::SocketAddr) -> quinn::Connection {
    let client = client_endpoint();
    let conn = client
        .connect_with(client_config(), addr, "localhost")
        .expect("dial")
        .await
        .expect("connect");
    std::mem::forget(client);
    conn
}

pub type ClientFramed =
    tokio_util::codec::Framed<vpn::server::ControlStream, vpn::framing::ControlCodec>;

pub async fn open_control(conn: &quinn::Connection) -> ClientFramed {
    let (send, recv) = conn.open_bi().await.expect("open_bi");
    tokio_util::codec::Framed::new(
        vpn::server::ControlStream::new(send, recv),
        vpn::framing::ControlCodec::new(),
    )
}

pub async fn send_auth_request(framed: &mut ClientFramed, username: &str, password: &str) {
    use futures::SinkExt;
    use vpn::ctrl::control_message::Msg;
    framed
        .send(vpn::ctrl::ControlMessage {
            msg: Some(Msg::AuthRequest(vpn::ctrl::AuthRequest {
                username: username.to_string(),
                password: password.to_string(),
            })),
        })
        .await
        .expect("send auth request");
}

pub async fn send_heartbeat(framed: &mut ClientFramed) {
    use futures::SinkExt;
    use vpn::ctrl::control_message::Msg;
    framed
        .send(vpn::ctrl::ControlMessage {
            msg: Some(Msg::Heartbeat(vpn::ctrl::Heartbeat {})),
        })
        .await
        .expect("send heartbeat");
}

pub async fn recv_control(framed: &mut ClientFramed) -> Option<vpn::ctrl::ControlMessage> {
    use futures::StreamExt;
    framed.next().await.and_then(|r| r.ok())
}
