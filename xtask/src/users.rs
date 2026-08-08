use thiserror::Error;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

#[derive(Debug, Error)]
pub enum UsersError {
    #[error("empty username is not allowed")]
    EmptyUsername,
}

pub fn add_or_update_user(
    doc: &mut DocumentMut,
    username: &str,
    password_hash: &str,
) -> Result<bool, UsersError> {
    if username.is_empty() {
        return Err(UsersError::EmptyUsername);
    }
    let root = doc.as_table_mut();
    match root.get_mut("users") {
        Some(Item::ArrayOfTables(arr)) => {
            for user in arr.iter_mut() {
                if user.get("username").and_then(Item::as_str) == Some(username) {
                    user["password_hash"] = toml_edit::value(password_hash);
                    return Ok(false);
                }
            }
            let mut new_user = Table::new();
            new_user["username"] = toml_edit::value(username);
            new_user["password_hash"] = toml_edit::value(password_hash);
            arr.push(new_user);
        }
        _ => {
            let mut arr = ArrayOfTables::new();
            let mut new_user = Table::new();
            new_user["username"] = toml_edit::value(username);
            new_user["password_hash"] = toml_edit::value(password_hash);
            arr.push(new_user);
            root.insert("users", Item::ArrayOfTables(arr));
        }
    }
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn doc(content: &str) -> DocumentMut {
        DocumentMut::from_str(content).unwrap()
    }

    fn server_body() -> &'static str {
        r#"[server]
listen = "0.0.0.0:4433"
mtu = 1280
# keep me
cert = "cert.pem"
key = "key.pem"
"#
    }

    #[test]
    fn test_users_add_when_new_user_appends_entry() {
        let mut d = doc(server_body());
        assert!(add_or_update_user(&mut d, "alice", "PHC1").unwrap());
        let users = d
            .as_table()
            .get("users")
            .and_then(Item::as_array_of_tables)
            .expect("users array exists");
        assert_eq!(users.len(), 1);
        assert_eq!(users.get(0).unwrap()["username"].as_str(), Some("alice"));
        assert_eq!(
            users.get(0).unwrap()["password_hash"].as_str(),
            Some("PHC1")
        );
    }

    #[test]
    fn test_users_add_when_existing_user_updates_hash_only() {
        let mut d = doc(server_body());
        assert!(add_or_update_user(&mut d, "alice", "PHC1").unwrap());
        assert!(!add_or_update_user(&mut d, "alice", "PHC2").unwrap());
        let users = d
            .as_table()
            .get("users")
            .and_then(Item::as_array_of_tables)
            .unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users.get(0).unwrap()["username"].as_str(), Some("alice"));
        assert_eq!(
            users.get(0).unwrap()["password_hash"].as_str(),
            Some("PHC2")
        );
    }

    #[test]
    fn test_users_edit_preserves_unrelated_content() {
        let mut d = doc(server_body());
        add_or_update_user(&mut d, "alice", "PHC1").unwrap();
        let out = d.to_string();
        assert!(out.contains("listen = \"0.0.0.0:4433\""));
        assert!(out.contains("# keep me"));
        assert!(out.contains("mtu = 1280"));
        assert!(out.contains("cert = \"cert.pem\""));
    }

    #[test]
    fn test_users_add_when_no_users_section_creates_array() {
        let mut d = doc(server_body());
        assert!(add_or_update_user(&mut d, "bob", "PHC1").unwrap());
        let users = d
            .as_table()
            .get("users")
            .and_then(Item::as_array_of_tables)
            .expect("users array created");
        assert_eq!(users.len(), 1);
        assert_eq!(users.get(0).unwrap()["username"].as_str(), Some("bob"));
    }

    #[test]
    fn test_users_add_when_empty_username_returns_error() {
        let mut d = doc(server_body());
        let err = add_or_update_user(&mut d, "", "PHC1").unwrap_err();
        assert!(matches!(err, UsersError::EmptyUsername));
    }

    #[test]
    fn test_users_add_when_multiple_distinct_users_appends_both() {
        let mut d = doc(server_body());
        assert!(add_or_update_user(&mut d, "alice", "PHC1").unwrap());
        assert!(add_or_update_user(&mut d, "bob", "PHC2").unwrap());
        let users = d
            .as_table()
            .get("users")
            .and_then(Item::as_array_of_tables)
            .unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users.get(1).unwrap()["username"].as_str(), Some("bob"));
    }

    #[test]
    fn test_users_add_when_update_preserves_other_users() {
        let mut d = doc(server_body());
        add_or_update_user(&mut d, "alice", "PHC1").unwrap();
        add_or_update_user(&mut d, "bob", "PHC2").unwrap();
        assert!(!add_or_update_user(&mut d, "alice", "PHC3").unwrap());
        let users = d
            .as_table()
            .get("users")
            .and_then(Item::as_array_of_tables)
            .unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(
            users.get(0).unwrap()["password_hash"].as_str(),
            Some("PHC3")
        );
        assert_eq!(
            users.get(1).unwrap()["password_hash"].as_str(),
            Some("PHC2")
        );
    }
}
