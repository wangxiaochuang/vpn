#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use futures::SinkExt;
use futures::StreamExt;
use vpn_core::ctrl::auth_challenge::Challenge;
use vpn_core::ctrl::auth_init::Method;
use vpn_core::ctrl::auth_response::Response;
use vpn_core::ctrl::control_message::Msg;
use vpn_core::ctrl::{
    AuthDenied, AuthMethod, AuthOk, AuthResponse, ControlMessage, DenyReason, PasswordAuth,
    ServerHello, TotpChallenge, TotpResponse,
};

type ClientFramed = tokio_util::codec::Framed<
    quic_link::quinn_stream::QuinnStream,
    vpn_core::framing::ControlCodec,
>;

fn hello_with_methods(methods: Vec<AuthMethod>) -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::ServerHello(ServerHello {
            protocol_version: vpn_core::ctrl::PROTOCOL_VERSION,
            supported_methods: methods.into_iter().map(|m| m as i32).collect(),
        })),
    }
}

fn auth_ok() -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::AuthOk(AuthOk {
            assigned_ip: "10.0.0.2".to_string(),
            subnet: "10.0.0.0/24".to_string(),
            gateway: "10.0.0.1".to_string(),
            mtu: 1280,
            routes: vec![],
        })),
    }
}

fn auth_denied() -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::AuthDenied(AuthDenied {
            reason: DenyReason::AuthFailed as i32,
        })),
    }
}

fn totp_challenge() -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::AuthChallenge(vpn_core::ctrl::AuthChallenge {
            challenge: Some(Challenge::Totp(TotpChallenge {
                prompt: "Enter TOTP code".to_string(),
            })),
        })),
    }
}

fn password_init(username: &str, password: &str) -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::AuthInit(vpn_core::ctrl::AuthInit {
            username: username.to_string(),
            method: Some(Method::Password(PasswordAuth {
                password: password.to_string(),
            })),
        })),
    }
}

async fn mock_auth_server(responses: Vec<ControlMessage>) -> std::net::SocketAddr {
    let server = quic_link::Server::builder()
        .tls_from_files(common::repo("cert.pem"), common::repo("key.pem"))
        .build("127.0.0.1:0".parse().unwrap())
        .expect("build server");
    let addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        let session = server.accept().await.expect("accept").expect("conn");
        let mut channel = session
            .accept_stream::<ControlMessage>()
            .await
            .expect("accept stream");
        for resp in responses {
            channel.send(resp).await.expect("send");
        }
        std::future::pending::<()>().await;
    });
    addr
}

async fn connect_and_open_framed(addr: std::net::SocketAddr) -> ClientFramed {
    let client = common::client_endpoint();
    let conn = client
        .connect_with(common::client_config(), addr, "localhost")
        .expect("dial")
        .await
        .expect("connect");
    std::mem::forget(client);
    let (send, recv) = conn.open_bi().await.expect("open_bi");
    tokio_util::codec::Framed::new(
        quic_link::quinn_stream::QuinnStream::new(send, recv),
        vpn_core::framing::ControlCodec::new(),
    )
}

async fn recv_msg(framed: &mut ClientFramed) -> ControlMessage {
    tokio::time::timeout(Duration::from_secs(5), framed.next())
        .await
        .expect("timeout")
        .expect("stream closed")
        .expect("decode error")
}

#[tokio::test]
async fn test_password_auth_zero_challenge_receives_auth_ok() {
    let addr = mock_auth_server(vec![
        hello_with_methods(vec![AuthMethod::Password]),
        auth_ok(),
    ])
    .await;
    let mut framed = connect_and_open_framed(addr).await;
    framed
        .send(ControlMessage { msg: None })
        .await
        .expect("open");
    let _hello = recv_msg(&mut framed).await;
    let resp = recv_msg(&mut framed).await;
    assert!(matches!(resp.msg, Some(Msg::AuthOk(_))));
}

#[tokio::test]
async fn test_wrong_password_receives_auth_denied() {
    let addr = mock_auth_server(vec![
        hello_with_methods(vec![AuthMethod::Password]),
        auth_denied(),
    ])
    .await;
    let mut framed = connect_and_open_framed(addr).await;
    framed
        .send(ControlMessage { msg: None })
        .await
        .expect("open");
    let _hello = recv_msg(&mut framed).await;
    let resp = recv_msg(&mut framed).await;
    assert!(matches!(resp.msg, Some(Msg::AuthDenied(_))));
}

#[tokio::test]
async fn test_server_hello_carries_supported_methods_password() {
    let addr = mock_auth_server(vec![
        hello_with_methods(vec![AuthMethod::Password]),
        auth_ok(),
    ])
    .await;
    let mut framed = connect_and_open_framed(addr).await;
    framed
        .send(ControlMessage { msg: None })
        .await
        .expect("open");
    let hello = recv_msg(&mut framed).await;
    match hello.msg {
        Some(Msg::ServerHello(h)) => {
            assert!(h.supported_methods.contains(&(AuthMethod::Password as i32)));
        }
        other => panic!("expected ServerHello, got {other:?}"),
    }
}

#[tokio::test]
async fn test_client_sends_password_auth_when_supported() {
    let addr = mock_auth_server(vec![
        hello_with_methods(vec![AuthMethod::Password]),
        auth_ok(),
    ])
    .await;
    let mut framed = connect_and_open_framed(addr).await;
    framed
        .send(ControlMessage { msg: None })
        .await
        .expect("open");
    let _hello = recv_msg(&mut framed).await;
    framed
        .send(password_init("alice", "s3cret"))
        .await
        .expect("send init");
}

#[tokio::test]
async fn test_challenge_response_loop() {
    let addr = mock_auth_server(vec![
        hello_with_methods(vec![AuthMethod::Password]),
        totp_challenge(),
        auth_ok(),
    ])
    .await;
    let mut framed = connect_and_open_framed(addr).await;
    framed
        .send(ControlMessage { msg: None })
        .await
        .expect("open");
    let _hello = recv_msg(&mut framed).await;
    let challenge = recv_msg(&mut framed).await;
    assert!(matches!(challenge.msg, Some(Msg::AuthChallenge(_))));
    let resp = AuthResponse {
        response: Some(Response::Totp(TotpResponse {
            code: "123456".to_string(),
        })),
    };
    framed
        .send(ControlMessage {
            msg: Some(Msg::AuthResponse(resp)),
        })
        .await
        .expect("send response");
    let ok = recv_msg(&mut framed).await;
    assert!(matches!(ok.msg, Some(Msg::AuthOk(_))));
}
