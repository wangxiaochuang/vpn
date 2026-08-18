use std::sync::Arc;

use self::conn::build_net_profile;
use self::downlink::DownlinkDaemon;
use crate::auth::PasswordAuthenticator;
use crate::config::ServerConfig;
use crate::db::open_telemetry_store;
use crate::db::open_user_store;
use crate::db::sqlite::SqliteTelemetrySink;
use crate::ledger::ConnectionLedger;
use crate::telemetry::TelemetryPlane;
use anyhow::Context;
use ipnet::Ipv4Net;
use quic_link::{Server, Session};
use shutdown::Shutdown;
use shutdown::ShutdownHandle;
use sysprobe::sink::ConsoleSink;
use sysprobe::sink::TelemetrySink;
use vpn_core::data::Tun;

pub mod conn;
pub mod downlink;
pub mod handshake;
pub mod supervisor;

pub use self::conn::{AuthStore, ClientNetProfile, ConnExitCause, ConnectionHandle};
pub use self::downlink::RegistryDispatcher;
pub use self::supervisor::{handle_conn, spawn_uplink_task};

pub struct AcceptLoop {
    endpoint: Server,
    tun: Tun,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    auth: Arc<AuthStore>,
    net_profile: Arc<ClientNetProfile>,
    telemetry: Arc<TelemetryPlane>,
    conn_set: tokio::task::JoinSet<ConnExitCause>,
}

impl AcceptLoop {
    pub fn new(
        endpoint: Server,
        tun: Tun,
        ledger: Arc<ConnectionLedger<ConnectionHandle>>,
        auth: Arc<AuthStore>,
        net_profile: Arc<ClientNetProfile>,
        telemetry: Arc<TelemetryPlane>,
    ) -> Self {
        Self {
            endpoint,
            tun,
            ledger,
            auth,
            net_profile,
            telemetry,
            conn_set: tokio::task::JoinSet::new(),
        }
    }

    pub async fn serve(&mut self, sd: &ShutdownHandle) {
        loop {
            // cancel-safety: sd.cancelled() 与 endpoint.accept() 均 cancel-safe。
            tokio::select! {
                biased;
                () = sd.cancelled() => break,
                accepted = self.endpoint.accept() => match accepted {
                    None => break,
                    Some(Err(e)) => tracing::warn!("connection accept error: {e}"),
                    Some(Ok(session)) => self.spawn_conn(session, sd),
                }
            }
        }
    }

    fn spawn_conn(&mut self, session: Session, sd: &ShutdownHandle) {
        self.conn_set.spawn(handle_conn(
            session,
            self.auth.clone(),
            self.ledger.clone(),
            self.net_profile.clone(),
            self.telemetry.clone(),
            self.tun.clone(),
            sd.clone(),
        ));
    }

    pub fn close(&self) {
        self.endpoint.close();
    }

    pub async fn drain(&mut self, sd: &Shutdown) {
        sd.drain(&mut self.conn_set, "server").await;
    }
}

pub struct VpnServer {
    tun: Tun,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    accept: AcceptLoop,
    daemon: Option<DownlinkDaemon>,
}

impl VpnServer {
    pub async fn boot(config: ServerConfig) -> anyhow::Result<Self> {
        let (accept, tun, ledger) = build_accept(config).await?;
        Ok(Self {
            tun,
            ledger,
            accept,
            daemon: None,
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let sd = Shutdown::with_signal_watchdog().await;
        let sd_handle = sd.handle();
        self.daemon = Some(DownlinkDaemon::spawn(
            self.tun.clone(),
            self.ledger.clone(),
            sd.handle(),
        ));
        self.accept.serve(&sd_handle).await;
        self.graceful_stop(&sd).await;
        Ok(())
    }

    async fn graceful_stop(&mut self, sd: &Shutdown) {
        tracing::info!("initiating graceful shutdown");
        sd.trigger();
        self.accept.close();
        self.accept.drain(sd).await;
        if let Some(daemon) = &mut self.daemon {
            daemon.drain(sd).await;
        }
    }
}

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    VpnServer::boot(config).await?.run().await
}

async fn build_accept(
    config: ServerConfig,
) -> anyhow::Result<(AcceptLoop, Tun, Arc<ConnectionLedger<ConnectionHandle>>)> {
    let endpoint = build_server(&config)?;
    let auth = build_auth_store(&config).await?;
    let ledger = build_ledger(config.tun_subnet)?;
    let tun = create_tun(config.tun_subnet, config.mtu)?;
    let telemetry = build_telemetry_plane(&config.telemetry_db).await?;
    let net_profile = build_net_profile(config);
    let accept = AcceptLoop::new(
        endpoint,
        tun.clone(),
        ledger.clone(),
        auth,
        net_profile,
        telemetry,
    );
    Ok((accept, tun, ledger))
}

fn create_tun(tun_subnet: Ipv4Net, mtu: u16) -> anyhow::Result<Tun> {
    Ok(Tun(Arc::new(vpn_core::tun_setup::create_tun(
        tun_subnet, mtu,
    )?)))
}

fn build_server(config: &ServerConfig) -> anyhow::Result<Server> {
    let server = quic_link::Server::builder()
        .tls_from_files(config.cert.clone(), config.key.clone())
        .build(config.listen)?;
    tracing::info!(
        "listening on {}",
        server.local_addr().unwrap_or(config.listen)
    );
    Ok(server)
}

async fn build_auth_store(config: &ServerConfig) -> anyhow::Result<Arc<AuthStore>> {
    let store = open_user_store(&config.users_db)
        .await
        .context("database initialization failed")?;
    let authenticator = PasswordAuthenticator::new(store);
    Ok(Arc::new(AuthStore {
        authenticator: Arc::new(authenticator),
        supported_methods: vec![vpn_core::vpn::AuthMethod::Password],
    }))
}

fn build_ledger(subnet: Ipv4Net) -> anyhow::Result<Arc<ConnectionLedger<ConnectionHandle>>> {
    Ok(Arc::new(ConnectionLedger::new(subnet)?))
}

async fn build_telemetry_plane(db: &str) -> anyhow::Result<Arc<TelemetryPlane>> {
    let store = open_telemetry_store(db)
        .await
        .context("database initialization failed")?;
    let sink = SqliteTelemetrySink::new(store);
    Ok(Arc::new(TelemetryPlane::new(vec![
        Arc::new(ConsoleSink) as Arc<dyn TelemetrySink>,
        Arc::new(sink) as Arc<dyn TelemetrySink>,
    ])))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn config_with_db(db: &str) -> ServerConfig {
        ServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tun_subnet: "10.0.0.0/24".parse().unwrap(),
            mtu: 1280,
            cert: std::path::PathBuf::new(),
            key: std::path::PathBuf::new(),
            routes: vec![],
            users_db: db.to_string(),
            telemetry_db: db.to_string(),
        }
    }

    #[tokio::test]
    async fn test_build_auth_store_when_db_valid_returns_password_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = format!("sqlite://{}", dir.path().join("users.db").display());
        let auth = build_auth_store(&config_with_db(&db)).await.unwrap();
        assert_eq!(
            auth.supported_methods,
            vec![vpn_core::vpn::AuthMethod::Password]
        );
    }

    #[tokio::test]
    async fn test_build_auth_store_when_db_unwritable_fails_fast() {
        let config = config_with_db("sqlite:///nonexistent-dir/users.db");
        let err = build_auth_store(&config)
            .await
            .err()
            .expect("boot should fail fast");
        assert!(err.to_string().contains("database"));
    }

    #[tokio::test]
    async fn test_build_auth_store_when_db_invalid_url_fails_fast() {
        let config = config_with_db("not-a-url");
        let err = build_auth_store(&config)
            .await
            .err()
            .expect("boot should fail fast");
        assert!(err.to_string().contains("database"));
    }

    #[tokio::test]
    async fn test_build_telemetry_plane_when_db_valid_assembles_two_sinks() {
        let dir = tempfile::tempdir().unwrap();
        let db = format!("sqlite://{}", dir.path().join("telemetry.db").display());
        let plane = build_telemetry_plane(&db).await.unwrap();
        assert_eq!(plane.sinks_len(), 2);
    }

    #[tokio::test]
    async fn test_build_telemetry_plane_persists_reports_to_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = format!("sqlite://{}", dir.path().join("telemetry.db").display());
        let plane = build_telemetry_plane(&db).await.unwrap();
        plane
            .store(&sample_source(), &sample_report())
            .await
            .unwrap();
        let rows = query_by_user(&db, "alice").await;
        let row = rows.first().expect("row should be persisted");
        assert_eq!(row.kind, sysprobe::proto::InfoKind::ProcessSummary as i32);
    }

    async fn query_by_user(db: &str, username: &str) -> Vec<crate::db::TelemetryRow> {
        let store = crate::db::open_telemetry_store(db).await.unwrap();
        let filter = crate::db::TelemetryFilter {
            username: Some(username.into()),
            ..crate::db::TelemetryFilter::default()
        };
        store.query(&filter).await.unwrap()
    }

    #[tokio::test]
    async fn test_build_telemetry_plane_when_db_unwritable_fails_fast() {
        let err = build_telemetry_plane("sqlite:///nonexistent-dir/telemetry.db")
            .await
            .err()
            .expect("boot should fail fast");
        assert!(err.to_string().contains("database"));
    }

    fn sample_report() -> sysprobe::proto::TelemetryReport {
        sysprobe::proto::TelemetryReport {
            ts_ms: 1,
            items: vec![sysprobe::proto::InfoSnapshot {
                kind: sysprobe::proto::InfoKind::ProcessSummary as i32,
                payload: None,
            }],
        }
    }

    fn sample_source() -> sysprobe::sink::SinkSource {
        sysprobe::sink::SinkSource {
            session_id: 1,
            username: "alice".into(),
            virtual_ip: None,
        }
    }
}
