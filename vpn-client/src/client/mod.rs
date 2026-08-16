mod data_plane;
mod established;
mod preauth;

pub use self::data_plane::{DataPlane, ExitCause, heartbeat_loop};
pub use self::established::EstablishedClient;
pub use self::preauth::{ClientError, PreAuthClient, parse_auth_ok};

use std::net::Ipv4Addr;

use ipnet::Ipv4Net;

use crate::config::ClientConfig;
use crate::credentials::CliCredentialCollector;
use crate::credentials::CredentialCollector;
use crate::credentials::StaticCredentialCollector;
use shutdown::Shutdown;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTunParams {
    pub assigned_ip: Ipv4Addr,
    pub subnet: Ipv4Net,
    pub gateway: Ipv4Addr,
    pub mtu: u16,
    pub routes: Vec<Ipv4Net>,
}

pub struct VpnClient<C: CredentialCollector> {
    config: ClientConfig,
    collector: C,
}

impl<C: CredentialCollector> VpnClient<C> {
    pub fn new(config: ClientConfig, collector: C) -> Self {
        Self { config, collector }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let sd = Shutdown::with_signal_watchdog().await;
        let pre = PreAuthClient::connect(&self.config).await?;
        let est = pre.authenticate(&mut self.collector).await?;
        est.run(&sd).await
    }
}

pub async fn run(config: ClientConfig) -> anyhow::Result<()> {
    VpnClient::new(config, CliCredentialCollector).run().await
}

pub async fn run_with_credentials(
    config: ClientConfig,
    username: String,
    password: String,
) -> anyhow::Result<()> {
    let collector = StaticCredentialCollector { username, password };
    VpnClient::new(config, collector).run().await
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names
)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn test_spawn_signal_watchdog_cancels_on_sigint() {
        let sd = Shutdown::default();
        let ready = sd.spawn_signal_watchdog();
        ready
            .await
            .expect("watchdog should finish registering the SIGINT handler");
        unsafe {
            libc::kill(libc::getpid(), libc::SIGINT);
        }
        tokio::time::timeout(Duration::from_secs(3), sd.triggered())
            .await
            .expect("watchdog should trigger the Shutdown when SIGINT is received");
    }
}
