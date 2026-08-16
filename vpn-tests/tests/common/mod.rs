#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;

use argon2::Argon2;
use argon2::PasswordHasher;
use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use bytes::Bytes;
use ipnet::Ipv4Net;
use quic_link::PacketSink;
pub use quic_link::test_util::no_verify_client_config as client_config;
pub use quic_link::test_util::repo_file as repo;
use sysprobe::sink::TelemetrySink;
use user_store::InMemoryUserStore;
use vpn_core::vpn::AuthMethod;
use vpn_server::auth::PasswordAuthenticator;
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
    pub store: Arc<dyn user_store::UserStore>,
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

pub fn test_config(db: &str) -> ServerConfig {
    ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        tun_subnet: Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
        mtu: 1280,
        cert: repo("cert.pem"),
        key: repo("key.pem"),
        routes: vec![],
        db: db.to_string(),
    }
}

pub fn net_profile(subnet: Ipv4Net, routes: Vec<Ipv4Net>) -> Arc<ClientNetProfile> {
    Arc::new(ClientNetProfile {
        subnet,
        gateway: vpn_core::tun_setup::gateway_addr(subnet),
        mtu: 1280,
        routes,
    })
}

pub fn auth_store(
    users: Vec<(String, String)>,
) -> (Arc<AuthStore>, Arc<dyn user_store::UserStore>) {
    let store: Arc<dyn user_store::UserStore> =
        Arc::new(InMemoryUserStore::from_pairs(users).unwrap());
    let authenticator = PasswordAuthenticator::new(store.clone());
    let auth = Arc::new(AuthStore {
        authenticator: Arc::new(authenticator),
        supported_methods: vec![AuthMethod::Password],
    });
    (auth, store)
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
    let (auth, store) = auth_store(users);
    Arc::new(TestDeps {
        ledger: test_ledger(subnet),
        auth,
        store,
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
    let (auth, store) = auth_store(users);
    Arc::new(TestDeps {
        ledger: test_ledger(subnet),
        auth,
        store,
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

pub fn client_endpoint() -> quinn::Endpoint {
    quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap()
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

pub async fn start_test_server_with_store(
    store: Arc<dyn user_store::UserStore>,
    subnet: Ipv4Net,
) -> (quinn::Endpoint, Arc<TestDeps>, shutdown::ShutdownHandle) {
    let server_cfg = quic_link::build_quinn_server_config(&repo("cert.pem"), &repo("key.pem"))
        .expect("server cfg");
    let endpoint = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap())
        .expect("server endpoint");
    let authenticator = PasswordAuthenticator::new(store.clone());
    let state = Arc::new(TestDeps {
        ledger: test_ledger(subnet),
        auth: Arc::new(AuthStore {
            authenticator: Arc::new(authenticator),
            supported_methods: vec![AuthMethod::Password],
        }),
        store,
        net_profile: net_profile(subnet, vec![]),
        telemetry: test_telemetry_plane(),
    });
    let handle = shutdown::Shutdown::default().handle();
    spawn_accept_loop(endpoint.clone(), state.clone(), handle.clone()).await;
    (endpoint, state, handle)
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
    vpn_core::framing::ControlCodec,
>;

pub async fn open_control(conn: &quinn::Connection) -> ClientFramed {
    let (send, recv) = conn.open_bi().await.expect("open_bi");
    tokio_util::codec::Framed::new(
        quic_link::quinn_stream::QuinnStream::new(send, recv),
        vpn_core::framing::ControlCodec::new(),
    )
}

pub async fn send_open_signal(framed: &mut ClientFramed) {
    use futures::SinkExt;
    framed
        .send(vpn_core::ctrl::ControlMessage { msg: None })
        .await
        .expect("send open signal");
}

pub async fn send_auth_request(framed: &mut ClientFramed, username: &str, password: &str) {
    use futures::SinkExt;
    use vpn_core::ctrl::auth_init::Method;
    use vpn_core::ctrl::control_message::Msg;
    send_open_signal(framed).await;
    recv_server_hello(framed).await;
    framed
        .send(vpn_core::ctrl::ControlMessage {
            msg: Some(Msg::AuthInit(vpn_core::ctrl::AuthInit {
                username: username.to_string(),
                method: Some(Method::Password(vpn_core::ctrl::PasswordAuth {
                    password: password.to_string(),
                })),
            })),
        })
        .await
        .expect("send auth init");
}

pub async fn recv_server_hello(framed: &mut ClientFramed) {
    use vpn_core::ctrl::control_message::Msg;
    let msg = recv_control(framed).await.expect("expected ServerHello");
    assert!(
        matches!(msg.msg, Some(Msg::ServerHello(_))),
        "expected ServerHello, got {msg:?}"
    );
}

pub async fn send_heartbeat(framed: &mut ClientFramed) {
    use futures::SinkExt;
    use vpn_core::ctrl::control_message::Msg;
    framed
        .send(vpn_core::ctrl::ControlMessage {
            msg: Some(Msg::Heartbeat(vpn_core::ctrl::Heartbeat {})),
        })
        .await
        .expect("send heartbeat");
}

pub async fn recv_control(framed: &mut ClientFramed) -> Option<vpn_core::ctrl::ControlMessage> {
    use futures::StreamExt;
    framed.next().await.and_then(|r| r.ok())
}
