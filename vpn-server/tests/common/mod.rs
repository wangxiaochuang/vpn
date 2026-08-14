#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use argon2::Argon2;
use argon2::PasswordHasher;
use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use bytes::Bytes;
use ipnet::Ipv4Net;
use quic_link::PacketSink;
use sysprobe::sink::TelemetrySink;
use vpn_core::vpn::AuthMethod;
use vpn_server::auth::{PasswordAuthenticator, UserStore};
use vpn_server::config::ServerConfig;
use vpn_server::ledger::ConnectionLedger;
use vpn_server::server::AuthStore;
use vpn_server::server::ClientNetProfile;
use vpn_server::server::ConnectionHandle;
use vpn_server::telemetry::TelemetryPlane;

pub const ALICE_PASSWORD: &str = "s3cret";

pub struct TestDeps {
    pub ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    pub auth: Arc<AuthStore>,
    pub net_profile: Arc<ClientNetProfile>,
    pub telemetry: Arc<TelemetryPlane>,
}

pub struct DiscardSink;

impl PacketSink for DiscardSink {
    async fn send(&mut self, _pkt: Bytes) -> io::Result<()> {
        Ok(())
    }
}

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
        routes: vec![],
        users: vec![vpn_server::config::UserConfig {
            username: "alice".to_string(),
            password_hash: hash_password(ALICE_PASSWORD),
        }],
    }
}

pub fn net_profile(subnet: Ipv4Net, routes: Vec<Ipv4Net>) -> Arc<ClientNetProfile> {
    Arc::new(ClientNetProfile {
        subnet,
        gateway: vpn_server::tun_setup::gateway_addr(subnet),
        mtu: 1280,
        routes,
    })
}

pub fn auth_store(users: Vec<(String, String)>) -> Arc<AuthStore> {
    let store = UserStore::from_users(users).unwrap();
    let authenticator = PasswordAuthenticator::new(store);
    Arc::new(AuthStore {
        authenticator: Arc::new(authenticator),
        supported_methods: vec![AuthMethod::Password],
    })
}

pub fn test_ledger(subnet: Ipv4Net) -> Arc<ConnectionLedger<ConnectionHandle>> {
    Arc::new(ConnectionLedger::new(subnet).unwrap())
}

pub fn test_telemetry_plane() -> Arc<TelemetryPlane> {
    Arc::new(TelemetryPlane::new(vec![
        Arc::new(sysprobe::sink::ConsoleSink) as Arc<dyn TelemetrySink>,
    ]))
}

fn build_test_deps(
    subnet: Ipv4Net,
    users: Vec<(String, String)>,
    routes: Vec<Ipv4Net>,
    telemetry: Arc<TelemetryPlane>,
) -> Arc<TestDeps> {
    Arc::new(TestDeps {
        ledger: test_ledger(subnet),
        auth: auth_store(users),
        net_profile: net_profile(subnet, routes),
        telemetry,
    })
}

pub fn test_state_with_subnet(subnet: Ipv4Net, users: Vec<(String, String)>) -> Arc<TestDeps> {
    test_state_with_subnet_and_routes(subnet, users, vec![])
}

pub fn test_state_with_sink(
    subnet: Ipv4Net,
    users: Vec<(String, String)>,
    sink: Arc<dyn TelemetrySink>,
) -> Arc<TestDeps> {
    let plane = Arc::new(TelemetryPlane::new(vec![sink]));
    Arc::new(TestDeps {
        ledger: test_ledger(subnet),
        auth: auth_store(users),
        net_profile: net_profile(subnet, vec![]),
        telemetry: plane,
    })
}

pub fn test_state_with_subnet_and_routes(
    subnet: Ipv4Net,
    users: Vec<(String, String)>,
    routes: Vec<Ipv4Net>,
) -> Arc<TestDeps> {
    build_test_deps(subnet, users, routes, test_telemetry_plane())
}

pub async fn make_test_state() -> Arc<TestDeps> {
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
    let server_cfg = quic_link::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem"))
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

async fn spawn_accept_loop(
    endpoint: quinn::Endpoint,
    deps: Arc<TestDeps>,
    handle: shutdown::ShutdownHandle,
) {
    let accept_endpoint = endpoint.clone();
    tokio::spawn(async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            if let Ok(conn) = incoming.await {
                let deps = deps.clone();
                let ct = handle.clone();
                tokio::spawn(async move {
                    let _ = vpn_server::server::handle_conn(
                        quic_link::Session::new(conn),
                        deps.auth.clone(),
                        deps.ledger.clone(),
                        deps.net_profile.clone(),
                        deps.telemetry.clone(),
                        DiscardSink,
                        ct,
                    )
                    .await;
                });
            }
        }
    });
}

pub async fn start_test_server(
    users: Vec<(String, String)>,
    subnet: Ipv4Net,
) -> (quinn::Endpoint, Arc<TestDeps>) {
    let (endpoint, state, _shutdown) = start_test_server_with_shutdown(users, subnet).await;
    (endpoint, state)
}

pub async fn start_test_server_with_routes(
    users: Vec<(String, String)>,
    subnet: Ipv4Net,
    routes: Vec<Ipv4Net>,
) -> (quinn::Endpoint, Arc<TestDeps>, shutdown::ShutdownHandle) {
    let server_cfg = quic_link::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem"))
        .expect("server cfg");
    let endpoint = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap())
        .expect("server endpoint");
    let state = test_state_with_subnet_and_routes(subnet, users, routes);

    let handle = shutdown::Shutdown::default().handle();
    spawn_accept_loop(endpoint.clone(), state.clone(), handle.clone()).await;
    (endpoint, state, handle)
}

pub async fn start_test_server_with_state(
    state: Arc<TestDeps>,
) -> (quinn::Endpoint, shutdown::Shutdown) {
    let server_cfg = quic_link::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem"))
        .expect("server cfg");
    let endpoint = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap())
        .expect("server endpoint");
    let sd = shutdown::Shutdown::new(std::time::Duration::from_secs(10));
    spawn_accept_loop(endpoint.clone(), state.clone(), sd.handle()).await;
    (endpoint, sd)
}

pub async fn start_test_server_with_shutdown(
    users: Vec<(String, String)>,
    subnet: Ipv4Net,
) -> (quinn::Endpoint, Arc<TestDeps>, shutdown::Shutdown) {
    let server_cfg = quic_link::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem"))
        .expect("server cfg");
    let endpoint = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap())
        .expect("server endpoint");
    let state = test_state_with_subnet(subnet, users);

    let sd = shutdown::Shutdown::default();
    spawn_accept_loop(endpoint.clone(), state.clone(), sd.handle()).await;
    (endpoint, state, sd)
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

pub type ClientFramed = tokio_util::codec::Framed<
    quic_link::quinn_stream::QuinnStream,
    vpn_server::framing::ControlCodec,
>;

pub async fn open_control(conn: &quinn::Connection) -> ClientFramed {
    let (send, recv) = conn.open_bi().await.expect("open_bi");
    tokio_util::codec::Framed::new(
        quic_link::quinn_stream::QuinnStream::new(send, recv),
        vpn_server::framing::ControlCodec::new(),
    )
}

pub async fn send_auth_request(framed: &mut ClientFramed, username: &str, password: &str) {
    use futures::SinkExt;
    use vpn_server::ctrl::auth_init::Method;
    use vpn_server::ctrl::control_message::Msg;
    framed
        .send(vpn_server::ctrl::ControlMessage {
            msg: Some(Msg::AuthInit(vpn_server::ctrl::AuthInit {
                username: username.to_string(),
                method: Some(Method::Password(vpn_server::ctrl::PasswordAuth {
                    password: password.to_string(),
                })),
            })),
        })
        .await
        .expect("send auth init");
    recv_server_hello(framed).await;
}

pub async fn send_heartbeat(framed: &mut ClientFramed) {
    use futures::SinkExt;
    use vpn_server::ctrl::control_message::Msg;
    framed
        .send(vpn_server::ctrl::ControlMessage {
            msg: Some(Msg::Heartbeat(vpn_server::ctrl::Heartbeat {})),
        })
        .await
        .expect("send heartbeat");
}

pub async fn send_open_signal(framed: &mut ClientFramed) {
    use futures::SinkExt;
    framed
        .send(vpn_server::ctrl::ControlMessage { msg: None })
        .await
        .expect("send open signal");
}

pub async fn recv_server_hello(framed: &mut ClientFramed) -> vpn_server::ctrl::ServerHello {
    use vpn_server::ctrl::control_message::Msg;
    let msg = recv_control(framed).await.expect("expected ServerHello");
    match msg.msg {
        Some(Msg::ServerHello(h)) => h,
        other => panic!("expected ServerHello, got {other:?}"),
    }
}

pub async fn recv_control(framed: &mut ClientFramed) -> Option<vpn_server::ctrl::ControlMessage> {
    use futures::StreamExt;
    framed.next().await.and_then(|r| r.ok())
}
