#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use quic_link::Session;
use std::time::Duration;
use sysprobe::proto::TelemetryMessage;

#[tokio::test]
async fn test_a() {
    let pair = common::make_connected_pair().await;
    // open client telemetry first, then spawn server accept
    let cs = Session::new(pair.client.clone());
    let _ch = cs.open_stream::<TelemetryMessage>().await.unwrap();
    let ss = Session::new(pair.server.clone());
    let t = tokio::spawn(async move { ss.accept_stream::<TelemetryMessage>().await });
    let r = tokio::time::timeout(Duration::from_secs(3), t).await;
    println!("order client-then-accept: ok={}", r.is_ok());
}

#[tokio::test]
async fn test_b() {
    let pair = common::make_connected_pair().await;
    // spawn server accept first, then open client telemetry
    let ss = Session::new(pair.server.clone());
    let t = tokio::spawn(async move { ss.accept_stream::<TelemetryMessage>().await });
    let cs = Session::new(pair.client.clone());
    let _ch = cs.open_stream::<TelemetryMessage>().await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(3), t).await;
    println!("order accept-then-client: ok={}", r.is_ok());
}
