use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use thiserror::Error;

use crate::session::Session;
use crate::tls;

/// QUIC 服务端，持有 `quinn::Endpoint`，由 [`Server::builder`] 构造。
pub struct Server {
    endpoint: quinn::Endpoint,
}

impl Server {
    pub fn builder() -> ServerBuilder {
        ServerBuilder::default()
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    pub async fn accept(&self) -> Option<Result<Session>> {
        let incoming = self.endpoint.accept().await?;
        match incoming.await {
            Ok(conn) => Some(Ok(Session::new(conn))),
            Err(e) => Some(Err(AcceptError(e).into())),
        }
    }

    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"server-close");
    }
}

#[derive(Default)]
pub struct ServerBuilder {
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
}

impl ServerBuilder {
    #[must_use]
    pub fn tls_from_files(mut self, cert: PathBuf, key: PathBuf) -> Self {
        self.cert = Some(cert);
        self.key = Some(key);
        self
    }

    pub fn build(self, addr: SocketAddr) -> Result<Server> {
        let cert = self
            .cert
            .context("server builder requires cert path (call tls_from_files)")?;
        let key = self
            .key
            .context("server builder requires key path (call tls_from_files)")?;
        let quinn_cfg = tls::build_quinn_server_config(&cert, &key)?;
        let endpoint = quinn::Endpoint::server(quinn_cfg, addr)
            .with_context(|| format!("failed to bind server endpoint to {addr}"))?;
        Ok(Server { endpoint })
    }
}

/// QUIC 客户端，持有客户端 TLS 配置，由 [`Client::builder`] 构造。
pub struct Client {
    cfg: quinn::ClientConfig,
    name: String,
    endpoint: quinn::Endpoint,
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    pub async fn connect(&self, addr: SocketAddr) -> Result<Session> {
        let conn = self
            .endpoint
            .connect_with(self.cfg.clone(), addr, &self.name)
            .with_context(|| format!("failed to initiate connection to {addr}"))?
            .await
            .context("failed to connect to server")?;
        Ok(Session::new(conn))
    }
}

#[derive(Default)]
pub struct ClientBuilder {
    ca: Option<PathBuf>,
    server_name: Option<String>,
}

impl ClientBuilder {
    #[must_use]
    pub fn trust_ca(mut self, ca: PathBuf) -> Self {
        self.ca = Some(ca);
        self
    }

    #[must_use]
    pub fn server_name(mut self, name: impl Into<String>) -> Self {
        self.server_name = Some(name.into());
        self
    }

    pub fn build(self) -> Result<Client> {
        let ca = self
            .ca
            .context("client builder requires CA path (call trust_ca)")?;
        let server_name = self
            .server_name
            .context("client builder requires server_name (call server_name)")?;
        let client_cfg = tls::build_quinn_client_config(&ca, &server_name)?;
        let bind_addr: SocketAddr = "0.0.0.0:0"
            .parse()
            .map_err(|e| anyhow::anyhow!("failed to parse client bind address: {e}"))?;
        let endpoint =
            quinn::Endpoint::client(bind_addr).context("failed to bind client endpoint")?;
        Ok(Client {
            cfg: client_cfg,
            name: server_name,
            endpoint,
        })
    }
}

#[derive(Debug, Error)]
#[error("accept error: {0}")]
struct AcceptError(#[from] quinn::ConnectionError);
