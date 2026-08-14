use std::sync::Arc;

use self::conn::build_net_profile;
use self::downlink::DownlinkDaemon;
use crate::auth::UserStore;
use crate::config::ServerConfig;
use crate::ledger::ConnectionLedger;
use crate::telemetry::TelemetryPlane;
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
    sd: Shutdown,
}

impl VpnServer {
    pub fn boot(config: ServerConfig) -> anyhow::Result<Self> {
        let (accept, tun, ledger) = build_accept(config)?;
        let sd = Shutdown::default();
        Ok(Self {
            tun,
            ledger,
            accept,
            daemon: None,
            sd,
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let _ = shutdown::spawn_signal_watchdog(self.sd.clone()).await;
        let sd_handle = self.sd.handle();
        self.daemon = Some(DownlinkDaemon::spawn(
            self.tun.clone(),
            self.ledger.clone(),
            sd_handle.clone(),
        ));
        self.accept.serve(&sd_handle).await;
        self.graceful_stop().await;
        Ok(())
    }

    async fn graceful_stop(&mut self) {
        tracing::info!("initiating graceful shutdown");
        self.sd.trigger();
        self.accept.close();
        self.accept.drain(&self.sd).await;
        if let Some(daemon) = &mut self.daemon {
            daemon.drain(&self.sd).await;
        }
    }
}

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    VpnServer::boot(config)?.run().await
}

fn build_accept(
    config: ServerConfig,
) -> anyhow::Result<(AcceptLoop, Tun, Arc<ConnectionLedger<ConnectionHandle>>)> {
    let endpoint = build_server(&config)?;
    let auth = build_auth_store(&config)?;
    let ledger = build_ledger(config.tun_subnet)?;
    let tun = create_tun(config.tun_subnet, config.mtu)?;
    let net_profile = build_net_profile(config);
    let telemetry = build_telemetry_plane();
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

fn build_auth_store(config: &ServerConfig) -> anyhow::Result<Arc<AuthStore>> {
    let user_pairs: Vec<(String, String)> = config
        .users
        .iter()
        .map(|u| (u.username.clone(), u.password_hash.clone()))
        .collect();
    let users = UserStore::from_users(user_pairs)?;
    Ok(Arc::new(AuthStore { users }))
}

fn build_ledger(subnet: Ipv4Net) -> anyhow::Result<Arc<ConnectionLedger<ConnectionHandle>>> {
    Ok(Arc::new(ConnectionLedger::new(subnet)?))
}

fn build_telemetry_plane() -> Arc<TelemetryPlane> {
    Arc::new(TelemetryPlane::new(vec![
        Arc::new(ConsoleSink) as Arc<dyn TelemetrySink>
    ]))
}
