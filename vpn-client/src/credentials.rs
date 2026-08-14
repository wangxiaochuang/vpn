use async_trait::async_trait;
use vpn_core::vpn::auth_challenge::Challenge;
use vpn_core::vpn::auth_init::Method;
use vpn_core::vpn::auth_response::Response;
use vpn_core::vpn::{
    AuthChallenge, AuthInit, AuthMethod, AuthResponse, PasswordAuth, TotpResponse,
};

#[async_trait]
pub trait CredentialCollector: Send {
    async fn collect_init(&mut self, methods: &[AuthMethod]) -> AuthInit;
    async fn collect_response(&mut self, challenge: &AuthChallenge) -> AuthResponse;
}

pub struct CliCredentialCollector;

#[async_trait]
impl CredentialCollector for CliCredentialCollector {
    async fn collect_init(&mut self, _methods: &[AuthMethod]) -> AuthInit {
        let username = read_username().await;
        let password = read_password().await;
        build_password_init(username, password)
    }

    async fn collect_response(&mut self, challenge: &AuthChallenge) -> AuthResponse {
        match challenge.challenge.as_ref() {
            Some(Challenge::Totp(t)) => {
                let code = read_totp_code(&t.prompt).await;
                AuthResponse {
                    response: Some(Response::Totp(TotpResponse { code })),
                }
            }
            _ => AuthResponse::default(),
        }
    }
}

fn build_password_init(username: String, password: String) -> AuthInit {
    AuthInit {
        username,
        method: Some(Method::Password(PasswordAuth { password })),
    }
}

async fn read_username() -> String {
    let raw = tokio::task::spawn_blocking(|| rpassword::prompt_password("请输入用户名："))
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .unwrap_or_default();
    raw.trim().to_string()
}

async fn read_password() -> String {
    tokio::task::spawn_blocking(|| rpassword::prompt_password("请输入密码："))
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .unwrap_or_default()
}

async fn read_totp_code(prompt: &str) -> String {
    let label = prompt.to_string();
    tokio::task::spawn_blocking(move || rpassword::prompt_password(label))
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .unwrap_or_default()
}

pub struct StaticCredentialCollector {
    pub username: String,
    pub password: String,
}

#[async_trait]
impl CredentialCollector for StaticCredentialCollector {
    async fn collect_init(&mut self, _methods: &[AuthMethod]) -> AuthInit {
        build_password_init(self.username.clone(), self.password.clone())
    }

    async fn collect_response(&mut self, _challenge: &AuthChallenge) -> AuthResponse {
        AuthResponse::default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_build_password_init_carries_username_and_password() {
        let init = build_password_init("alice".to_string(), "s3cret".to_string());
        assert_eq!(init.username, "alice");
        let Some(Method::Password(pw)) = init.method else {
            panic!("expected Password method");
        };
        assert_eq!(pw.password, "s3cret");
    }

    #[tokio::test]
    async fn test_static_collector_collect_init_returns_password_init() {
        let mut c = StaticCredentialCollector {
            username: "alice".to_string(),
            password: "s3cret".to_string(),
        };
        let init = c.collect_init(&[AuthMethod::Password]).await;
        assert_eq!(init.username, "alice");
        let Some(Method::Password(pw)) = init.method else {
            panic!("expected Password method");
        };
        assert_eq!(pw.password, "s3cret");
    }

    #[tokio::test]
    async fn test_static_collector_collect_response_returns_default() {
        let mut c = StaticCredentialCollector {
            username: "alice".to_string(),
            password: "s3cret".to_string(),
        };
        let resp = c.collect_response(&AuthChallenge::default()).await;
        assert!(resp.response.is_none());
    }
}
