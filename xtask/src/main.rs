use std::path::Path;
use std::path::PathBuf;

use anyhow::bail;
use clap::{Parser, Subcommand};

mod hash;
mod telemetry_query;
mod users;

#[derive(Parser)]
#[command(name = "xtask", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    AddUser {
        #[arg(long, default_value = "server.toml")]
        config: PathBuf,
        username: String,
    },
    ListUsers {
        #[arg(long, default_value = "server.toml")]
        config: PathBuf,
    },
    DeleteUser {
        #[arg(long, default_value = "server.toml")]
        config: PathBuf,
        username: String,
    },
    TelemetryQuery(telemetry_query::Args),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::AddUser { config, username } => add_user(&config, &username).await,
        Command::ListUsers { config } => list_users(&config).await,
        Command::DeleteUser { config, username } => delete_user(&config, &username).await,
        Command::TelemetryQuery(args) => telemetry_query::run(&args).await,
    }
}

async fn add_user(config: &Path, username: &str) -> anyhow::Result<()> {
    let admin = users::UserAdmin::open(config).await?;
    let password = rpassword::prompt_password("Password: ")?;
    let confirm = rpassword::prompt_password("Confirm password: ")?;
    if password != confirm {
        bail!("passwords do not match");
    }
    let added = admin.add_user(username, &password).await?;
    if added {
        println!("added user {username}");
    } else {
        println!("updated password for user {username}");
    }
    Ok(())
}

async fn list_users(config: &Path) -> anyhow::Result<()> {
    let admin = users::UserAdmin::open(config).await?;
    for username in admin.list_users().await? {
        println!("{username}");
    }
    Ok(())
}

async fn delete_user(config: &Path, username: &str) -> anyhow::Result<()> {
    let admin = users::UserAdmin::open(config).await?;
    if admin.delete_user(username).await? {
        println!("deleted user {username}");
        Ok(())
    } else {
        bail!("user {username} does not exist");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_default_config_paths() {
        let cli = Cli::try_parse_from(["xtask", "add-user", "alice"]).unwrap();
        match cli.command {
            Command::AddUser { config, username } => {
                assert_eq!(config, PathBuf::from("server.toml"));
                assert_eq!(username, "alice");
            }
            _ => panic!("expected AddUser"),
        }
        let cli = Cli::try_parse_from(["xtask", "list-users"]).unwrap();
        match cli.command {
            Command::ListUsers { config } => assert_eq!(config, PathBuf::from("server.toml")),
            _ => panic!("expected ListUsers"),
        }
        let cli = Cli::try_parse_from(["xtask", "delete-user", "alice"]).unwrap();
        match cli.command {
            Command::DeleteUser { config, username } => {
                assert_eq!(config, PathBuf::from("server.toml"));
                assert_eq!(username, "alice");
            }
            _ => panic!("expected DeleteUser"),
        }
    }
}
