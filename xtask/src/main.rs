use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use toml_edit::DocumentMut;

mod hash;
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::AddUser { config, username } => add_user(&config, &username),
    }
}

fn add_user(config: &PathBuf, username: &str) -> anyhow::Result<()> {
    if username.is_empty() {
        bail!("empty username is not allowed");
    }

    let content = fs::read_to_string(config)
        .with_context(|| format!("failed to read config file {}", config.display()))?;
    let mut doc = DocumentMut::from_str(&content)
        .with_context(|| format!("failed to parse config file {}", config.display()))?;

    let password = rpassword::prompt_password("Password: ")?;
    let confirm = rpassword::prompt_password("Confirm password: ")?;
    if password != confirm {
        bail!("passwords do not match");
    }

    let password_hash = hash::hash_password(&password);
    let added = users::add_or_update_user(&mut doc, username, &password_hash)
        .context("failed to update users table")?;

    fs::write(config, doc.to_string())
        .with_context(|| format!("failed to write config file {}", config.display()))?;

    if added {
        println!("added user {username}");
    } else {
        println!("updated password for user {username}");
    }
    Ok(())
}
