use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use anyhow::bail;
use toml::Table;
use vpn_server::db::UserStore;
use vpn_server::db::open_user_store;

use crate::hash;

pub struct UserAdmin {
    store: Arc<dyn UserStore>,
}

impl UserAdmin {
    pub async fn open(config_path: &Path) -> anyhow::Result<Self> {
        let db = read_users_db_url(config_path)?;
        let store = open_user_store(&db)
            .await
            .with_context(|| format!("failed to open database {db}"))?;
        Ok(Self { store })
    }

    pub async fn add_user(&self, username: &str, password: &str) -> anyhow::Result<bool> {
        if username.is_empty() {
            bail!("empty username is not allowed");
        }
        let existed = self.store.password_hash(username).await?.is_some();
        let phc = hash::hash_password(password);
        self.store.upsert(username, &phc).await?;
        Ok(!existed)
    }

    pub async fn list_users(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.store.list().await?)
    }

    pub async fn delete_user(&self, username: &str) -> anyhow::Result<bool> {
        if username.is_empty() {
            bail!("empty username is not allowed");
        }
        self.store.delete(username).await.map_err(Into::into)
    }
}

pub fn read_users_db_url(config_path: &Path) -> anyhow::Result<String> {
    read_db_field(config_path, "users_db")
}

pub fn read_telemetry_db_url(config_path: &Path) -> anyhow::Result<String> {
    read_db_field(config_path, "telemetry_db")
}

fn read_db_field(config_path: &Path, field: &str) -> anyhow::Result<String> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config file {}", config_path.display()))?;
    let table = content
        .parse::<Table>()
        .with_context(|| format!("failed to parse config file {}", config_path.display()))?;
    let db = table
        .get("server")
        .and_then(|s| s.get(field))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if db.is_empty() {
        bail!(
            "config file {} has no [server].{field} field",
            config_path.display()
        );
    }
    Ok(db.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_config(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
        let path = dir.path().join("server.toml");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    fn config_body(db: &str) -> String {
        format!(
            "[server]\nlisten = \"0.0.0.0:4433\"\nusers_db = \"{db}\"\ntelemetry_db = \"sqlite://telemetry.db\"\n"
        )
    }

    async fn temp_admin() -> (tempfile::TempDir, UserAdmin) {
        let dir = tempfile::tempdir().unwrap();
        let db = format!("sqlite://{}", dir.path().join("users.db").display());
        let path = write_config(&dir, &config_body(&db));
        let admin = UserAdmin::open(&path).await.unwrap();
        (dir, admin)
    }

    #[test]
    fn test_read_db_url_when_db_present_returns_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, &config_body("sqlite://users.db"));
        assert_eq!(read_users_db_url(&path).unwrap(), "sqlite://users.db");
    }

    #[test]
    fn test_read_db_url_when_db_missing_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "[server]\nlisten = \"0.0.0.0:4433\"\n");
        assert!(read_users_db_url(&path).is_err());
    }

    #[test]
    fn test_read_db_url_when_file_missing_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        assert!(read_users_db_url(&path).is_err());
    }

    #[test]
    fn test_read_db_url_when_toml_invalid_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "listen = ");
        assert!(read_users_db_url(&path).is_err());
    }

    #[tokio::test]
    async fn test_open_when_config_db_missing_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "[server]\nlisten = \"0.0.0.0:4433\"\n");
        assert!(UserAdmin::open(&path).await.is_err());
    }

    #[tokio::test]
    async fn test_add_user_when_new_upserts_and_reports_added() {
        let (_dir, admin) = temp_admin().await;
        assert!(admin.add_user("alice", "s3cret").await.unwrap());
        assert_eq!(admin.list_users().await.unwrap(), vec!["alice".to_string()]);
    }

    #[tokio::test]
    async fn test_add_user_when_existing_updates_hash_only() {
        let (_dir, admin) = temp_admin().await;
        admin.add_user("alice", "s3cret").await.unwrap();
        assert!(!admin.add_user("alice", "rotated").await.unwrap());
        assert_eq!(admin.list_users().await.unwrap(), vec!["alice".to_string()]);
        let hash = admin.store.password_hash("alice").await.unwrap().unwrap();
        assert_ne!(hash, hash::hash_password("s3cret"));
    }

    #[tokio::test]
    async fn test_add_user_when_empty_username_rejected_without_write() {
        let (_dir, admin) = temp_admin().await;
        assert!(admin.add_user("", "s3cret").await.is_err());
        assert!(admin.list_users().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_list_users_returns_sorted_usernames() {
        let (_dir, admin) = temp_admin().await;
        admin.add_user("carol", "pw").await.unwrap();
        admin.add_user("alice", "pw").await.unwrap();
        admin.add_user("bob", "pw").await.unwrap();
        let expected = vec!["alice", "bob", "carol"];
        let names = admin.list_users().await.unwrap();
        let got: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn test_delete_user_when_present_returns_true() {
        let (_dir, admin) = temp_admin().await;
        admin.add_user("alice", "pw").await.unwrap();
        assert!(admin.delete_user("alice").await.unwrap());
        assert!(admin.list_users().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_delete_user_when_missing_returns_false() {
        let (_dir, admin) = temp_admin().await;
        assert!(!admin.delete_user("eve").await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_user_when_empty_username_rejected() {
        let (_dir, admin) = temp_admin().await;
        assert!(admin.delete_user("").await.is_err());
    }
}
