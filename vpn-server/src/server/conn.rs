use std::hash::{Hash, Hasher};
use std::net::Ipv4Addr;
use std::sync::Arc;

use crate::auth::UserStore;
use crate::config::ServerConfig;
use crate::ledger::ReservedIp;
use crate::telemetry::TelemetryTxSlot;
use crate::telemetry::make_telemetry_tx_slot;
use ipnet::Ipv4Net;
use vpn_core::tun_setup::gateway_addr;

/// 每连接 supervisor 的退出原因（"遗言"契约）。纯枚举，不携带错误信息。
///
/// 与客户端 `ExitCause` 的差异：服务端没有 `Downlink`（下行是全局泵，非 per-conn）、
/// 没有 `HeartbeatEnded`/`ServerDisconnect`（用 `keepalive_loop` 的归并 `CtrlEnded` 表达）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnExitCause {
    ServerShutdown,
    CtrlEnded,
    UplinkEnded,
    TelemetryEnded,
    TaskPanicked,
}

impl ConnExitCause {
    pub const ALL: [Self; 5] = [
        Self::ServerShutdown,
        Self::CtrlEnded,
        Self::UplinkEnded,
        Self::TelemetryEnded,
        Self::TaskPanicked,
    ];

    pub fn code(self) -> u64 {
        match self {
            Self::UplinkEnded | Self::CtrlEnded => 0x1,
            Self::TaskPanicked => 0x2,
            Self::ServerShutdown | Self::TelemetryEnded => 0,
        }
    }

    pub fn reason(self) -> &'static [u8] {
        match self {
            Self::ServerShutdown => b"server-shutdown",
            Self::CtrlEnded => b"ctrl-ended",
            Self::UplinkEnded => b"uplink-ended",
            Self::TelemetryEnded => b"telemetry-ended",
            Self::TaskPanicked => b"conn-panic",
        }
    }
}

impl std::fmt::Display for ConnExitCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ServerShutdown => write!(f, "server-shutdown"),
            Self::CtrlEnded => write!(f, "ctrl-ended"),
            Self::UplinkEnded => write!(f, "uplink-ended"),
            Self::TelemetryEnded => write!(f, "telemetry-ended"),
            Self::TaskPanicked => write!(f, "conn-panic"),
        }
    }
}

pub struct ConnectionHandle {
    id: usize,
    pub session: quic_link::Session,
    pub ip: Ipv4Addr,
    pub telemetry_tx: TelemetryTxSlot,
    pub(crate) retire_slot: Arc<std::sync::Mutex<Option<ReservedIp>>>,
}

impl std::fmt::Debug for ConnectionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionHandle")
            .field("id", &self.id)
            .field("ip", &self.ip)
            .finish_non_exhaustive()
    }
}

impl ConnectionHandle {
    pub fn new(session: quic_link::Session, ip: Ipv4Addr) -> Self {
        let id = session.id();
        Self {
            id,
            session,
            ip,
            telemetry_tx: make_telemetry_tx_slot(),
            retire_slot: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub async fn request_collect(
        &self,
        kinds: Vec<sysprobe::proto::InfoKind>,
    ) -> Result<(), crate::telemetry::TelemetryError> {
        crate::telemetry::request_collect(&self.telemetry_tx, kinds).await
    }
}

impl Clone for ConnectionHandle {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            session: self.session.clone(),
            ip: self.ip,
            telemetry_tx: self.telemetry_tx.clone(),
            retire_slot: self.retire_slot.clone(),
        }
    }
}

impl PartialEq for ConnectionHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ConnectionHandle {}

impl Hash for ConnectionHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// 客户端网络画像：认证成功后下发给客户端的 TUN 配置派生。
/// gateway 在 boot 时由 `gateway_addr(tun_subnet)` 预算一次，所有连接共享。
pub struct ClientNetProfile {
    pub subnet: Ipv4Net,
    pub gateway: Ipv4Addr,
    pub mtu: u16,
    pub routes: Vec<Ipv4Net>,
}

pub(super) fn build_net_profile(config: ServerConfig) -> Arc<ClientNetProfile> {
    Arc::new(ClientNetProfile {
        subnet: config.tun_subnet,
        gateway: gateway_addr(config.tun_subnet),
        mtu: config.mtu,
        routes: config.routes,
    })
}

/// 认证存储（只读共享）。
pub struct AuthStore {
    pub users: UserStore,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::mutable_key_type,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hash;
    use std::net::Ipv4Addr;
    use std::sync::Arc as StdArc;

    use quic_link::Session;

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

    fn repo(p: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("vpn crate nested under repo root")
            .join(p)
    }

    async fn make_client_sessions(n: usize) -> Vec<Session> {
        let server = build_test_server();
        let client_cfg = build_no_verify_client_config();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = server.local_addr().unwrap();
        let mut sessions = Vec::new();
        for _ in 0..n {
            let conn = client
                .connect_with(client_cfg.clone(), addr, "localhost")
                .expect("dial")
                .await
                .expect("connect");
            sessions.push(Session::new(conn));
        }
        std::mem::forget(client);
        sessions
    }

    fn build_test_server() -> quinn::Endpoint {
        let cert = repo("cert.pem");
        let key = repo("key.pem");
        let server_cfg = quic_link::build_quinn_server_config(&cert, &key).expect("server cfg");
        let server = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap())
            .expect("server endpoint");
        let server_for_accept = server.clone();
        tokio::spawn(async move {
            while let Some(incoming) = server_for_accept.accept().await {
                let _ = incoming.accept().map(|c| {
                    tokio::spawn(async move {
                        let _ = c.await;
                    });
                });
            }
        });
        server
    }

    fn build_no_verify_client_config() -> quinn::ClientConfig {
        let rustls_client = rustls::ClientConfig::builder_with_provider(StdArc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(StdArc::new(NoVerify))
        .with_no_client_auth();
        let quic_client =
            quinn::crypto::rustls::QuicClientConfig::try_from(StdArc::new(rustls_client)).unwrap();
        quinn::ClientConfig::new(StdArc::new(quic_client))
    }

    #[tokio::test]
    async fn test_connection_handle_eq_by_id_across_clones() {
        let mut sessions = make_client_sessions(1).await;
        let session = sessions.remove(0);
        let dup = session.clone();
        let h1 = ConnectionHandle::new(session, Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(dup, Ipv4Addr::new(10, 0, 0, 9));
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn test_connection_handle_neq_different_id() {
        let mut sessions = make_client_sessions(2).await;
        let h1 = ConnectionHandle::new(sessions.remove(0), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(sessions.remove(0), Ipv4Addr::new(10, 0, 0, 3));
        assert_ne!(h1.id(), h2.id());
        assert_ne!(h1, h2);
    }

    #[tokio::test]
    async fn test_connection_handle_hash_by_id() {
        let mut sessions = make_client_sessions(1).await;
        let session = sessions.remove(0);
        let h1 = ConnectionHandle::new(session.clone(), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(session, Ipv4Addr::new(10, 0, 0, 9));
        let mut s1 = DefaultHasher::new();
        let mut s2 = DefaultHasher::new();
        h1.hash(&mut s1);
        h2.hash(&mut s2);
        assert_eq!(s1.finish(), s2.finish());
    }

    #[tokio::test]
    async fn test_connection_handle_dedups_in_hashset() {
        let mut sessions = make_client_sessions(1).await;
        let session = sessions.remove(0);
        let h1 = ConnectionHandle::new(session.clone(), Ipv4Addr::new(10, 0, 0, 2));
        let h2 = ConnectionHandle::new(session, Ipv4Addr::new(10, 0, 0, 3));
        let mut set = HashSet::new();
        set.insert(h1);
        assert!(set.contains(&h2));
        set.insert(h2);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_conn_exit_cause_code_reason_mapping() {
        let cases = [
            (ConnExitCause::ServerShutdown, 0, "server-shutdown"),
            (ConnExitCause::CtrlEnded, 0x1, "ctrl-ended"),
            (ConnExitCause::UplinkEnded, 0x1, "uplink-ended"),
            (ConnExitCause::TelemetryEnded, 0, "telemetry-ended"),
            (ConnExitCause::TaskPanicked, 0x2, "conn-panic"),
        ];
        for (cause, code, reason) in cases {
            assert_eq!(cause.code(), code, "{cause:?}");
            assert_eq!(cause.reason(), reason.as_bytes(), "{cause:?}");
        }
    }

    #[test]
    fn test_conn_exit_cause_displays_are_distinct() {
        let all: Vec<String> = ConnExitCause::ALL.iter().map(ToString::to_string).collect();
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "duplicate at {i},{j}");
            }
        }
    }

    #[test]
    fn test_conn_exit_cause_is_copy_and_eq() {
        let a = ConnExitCause::CtrlEnded;
        let b = a;
        assert_eq!(a, b);
    }

    fn server_config_for(subnet: Ipv4Net, mtu: u16, routes: Vec<Ipv4Net>) -> ServerConfig {
        ServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tun_subnet: subnet,
            mtu,
            cert: std::path::PathBuf::new(),
            key: std::path::PathBuf::new(),
            routes,
            users: vec![],
        }
    }

    #[test]
    fn test_build_net_profile_projects_config_and_precomputes_gateway() {
        let subnet = Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap();
        let routes = vec![Ipv4Net::new(Ipv4Addr::new(192, 168, 100, 0), 24).unwrap()];
        let config = server_config_for(subnet, 1280, routes.clone());
        let profile = build_net_profile(config);
        assert_eq!(profile.subnet, subnet);
        assert_eq!(profile.gateway, gateway_addr(subnet));
        assert_eq!(profile.gateway, Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(profile.mtu, 1280);
        assert_eq!(profile.routes, routes);
    }
}
