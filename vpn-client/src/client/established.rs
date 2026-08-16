use anyhow::Context;

use super::ClientTunParams;
use super::data_plane::DataPlane;
use msgx::Channel;
use quic_link::Session;
use shutdown::Shutdown;
use vpn_core::data::Tun;
use vpn_core::vpn::ControlMessage;

/// 已认证客户端，持有连接生命周期。
///
/// 字段按声明顺序析构：`session` 先、`endpoint` 最后，保证 Endpoint 活得比
/// 所有使用 Session 的 task 更久。
pub struct EstablishedClient {
    session: Session,
    channel: Channel<ControlMessage>,
    params: ClientTunParams,
    #[allow(dead_code)]
    endpoint: quic_link::Client,
}

impl EstablishedClient {
    pub(super) fn new(
        session: Session,
        channel: Channel<ControlMessage>,
        params: ClientTunParams,
        endpoint: quic_link::Client,
    ) -> Self {
        Self {
            session,
            channel,
            params,
            endpoint,
        }
    }

    pub async fn run(self, sd: &Shutdown) -> anyhow::Result<()> {
        let tun = setup_tun(&self.params)?;
        log_client_authenticated(&self.params);
        let plane = DataPlane::spawn(self.session.clone(), Tun(tun), self.channel, sd);
        let cause = plane.run(sd.clone()).await;
        tracing::info!("client exited: {cause}");
        Ok(())
    }
}

fn setup_tun(params: &ClientTunParams) -> anyhow::Result<std::sync::Arc<tun_rs::AsyncDevice>> {
    let tun = vpn_core::tun_setup::create_client_tun(params.assigned_ip, params.subnet, params.mtu)
        .context("failed to create client TUN device")?;
    let dev_name = tun.name().unwrap_or_default();
    crate::route::ensure_subnet_route(&dev_name, params.subnet)
        .context("failed to configure subnet route")?;
    crate::route::add_routes(&dev_name, &params.routes).context("failed to add extra routes")?;
    Ok(std::sync::Arc::new(tun))
}

fn log_client_authenticated(params: &ClientTunParams) {
    tracing::info!(
        "authenticated, assigned_ip={}, subnet={}, mtu={}",
        params.assigned_ip,
        params.subnet,
        params.mtu
    );
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names
)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn repo(p: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("vpn crate nested under repo root")
            .join(p)
    }

    #[tokio::test]
    async fn test_established_client_field_order_construct_and_access() {
        let client = quic_link::Client::builder()
            .trust_ca(repo("cert.pem"))
            .server_name("localhost")
            .build()
            .expect("build client");
        let (session, channel) = connect_for_test(&client).await;
        let est = EstablishedClient::new(session, channel, test_params(), client);
        assert_est_access(&est);
    }

    fn test_params() -> ClientTunParams {
        ClientTunParams {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 2),
            subnet: "10.0.0.0/24".parse().unwrap(),
            gateway: Ipv4Addr::new(10, 0, 0, 1),
            mtu: 1280,
            routes: vec![],
        }
    }

    fn assert_est_access(est: &EstablishedClient) {
        assert_eq!(est.params.assigned_ip, Ipv4Addr::new(10, 0, 0, 2));
        let _ = est.session.id();
        let _ = est.params.subnet;
    }

    async fn connect_for_test(client: &quic_link::Client) -> (Session, Channel<ControlMessage>) {
        let server = quic_link::Server::builder()
            .tls_from_files(repo("cert.pem"), repo("key.pem"))
            .build("127.0.0.1:0".parse().unwrap())
            .expect("build server");
        let addr = server.local_addr().unwrap();
        let (server_result, session) = tokio::join!(server.accept(), client.connect(addr));
        let _server_session = server_result.expect("server accept").expect("accept conn");
        let session = session.expect("connect to server");
        let channel = session
            .open_stream::<ControlMessage>()
            .await
            .expect("open control stream");
        (session, channel)
    }
}
