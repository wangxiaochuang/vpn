use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
enum Cli {
    Server {
        #[arg(long)]
        config: PathBuf,
    },
    Client {
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli {
        Cli::Server { config } => {
            let cfg = vpn::config::ServerConfig::load(&config)?;
            vpn::server::run(cfg).await?;
        }
        Cli::Client { config } => {
            let cfg = vpn::config::ClientConfig::load(&config)?;
            vpn::client::run(cfg).await?;
        }
    }

    Ok(())
}
