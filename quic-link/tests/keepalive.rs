#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use msgx::channel::{ByteStream, Channel};
use quic_link::{KeepaliveConfig, LoopControl, SessionClose, keepalive_loop};

#[derive(Clone, PartialEq, prost::Message)]
struct TestMsg {
    #[prost(oneof = "test_msg::Msg", tags = "1, 2")]
    msg: Option<test_msg::Msg>,
}

mod test_msg {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Msg {
        #[prost(string, tag = "1")]
        Text(String),
        #[prost(uint32, tag = "2")]
        Num(u32),
    }
}

fn hb() -> TestMsg {
    TestMsg {
        msg: Some(test_msg::Msg::Text("hb".into())),
    }
}

fn text(s: &str) -> TestMsg {
    TestMsg {
        msg: Some(test_msg::Msg::Text(s.into())),
    }
}

fn pair() -> (Channel<TestMsg>, Channel<TestMsg>) {
    let (a, b) = tokio::io::duplex(8192);
    let (ar, aw) = tokio::io::split(a);
    let (br, bw) = tokio::io::split(b);
    let ch_a = Channel::from_io(ByteStream::new(ar, aw));
    let ch_b = Channel::from_io(ByteStream::new(br, bw));
    (ch_a, ch_b)
}

fn sd_handle() -> shutdown::ShutdownHandle {
    shutdown::Shutdown::new(Duration::from_secs(5)).handle()
}

#[derive(Clone)]
struct RecordingClose {
    closed: Arc<AtomicBool>,
    code: Arc<AtomicU64>,
}

impl RecordingClose {
    fn new() -> Self {
        Self {
            closed: Arc::new(AtomicBool::new(false)),
            code: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl SessionClose for RecordingClose {
    fn close(&self, code: u64, _reason: &[u8]) {
        self.closed.store(true, Ordering::SeqCst);
        self.code.store(code, Ordering::SeqCst);
    }
}

fn fast_cfg() -> KeepaliveConfig {
    KeepaliveConfig {
        interval: Duration::from_millis(50),
        timeout: Duration::from_millis(200),
    }
}

#[tokio::test]
async fn test_keepalive_loop_exits_on_shutdown() {
    let (ch_a, _ch_b) = pair();
    let (mut sender, mut receiver) = ch_a.split();
    let cancel = sd_handle();
    let close = RecordingClose::new();

    let cancel_clone = cancel.clone();
    let close_clone = close.clone();
    let task = tokio::spawn(async move {
        keepalive_loop(
            &close_clone,
            &mut sender,
            &mut receiver,
            &cancel_clone,
            fast_cfg(),
            hb,
            |_| LoopControl::Continue,
        )
        .await;
    });

    tokio::task::yield_now().await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("loop exits on shutdown")
        .expect("task not panic");
    assert!(
        !close.closed.load(Ordering::SeqCst),
        "shutdown should not close"
    );
}

#[tokio::test]
async fn test_keepalive_loop_exits_on_peer_stream_close() {
    let (ch_a, ch_b) = pair();
    let (mut sender, mut receiver) = ch_a.split();
    drop(ch_b);

    let cancel = sd_handle();
    let close = RecordingClose::new();
    tokio::time::timeout(
        Duration::from_secs(3),
        keepalive_loop(
            &close,
            &mut sender,
            &mut receiver,
            &cancel,
            KeepaliveConfig::default(),
            hb,
            |_| LoopControl::Continue,
        ),
    )
    .await
    .expect("loop exits when peer stream closes");
}

#[tokio::test]
async fn test_keepalive_loop_closes_on_timeout() {
    let (ch_a, _ch_b) = pair();
    let (mut sender, mut receiver) = ch_a.split();
    let cancel = sd_handle();
    let close = RecordingClose::new();

    keepalive_loop(
        &close,
        &mut sender,
        &mut receiver,
        &cancel,
        fast_cfg(),
        hb,
        |_| LoopControl::Continue,
    )
    .await;

    assert!(close.closed.load(Ordering::SeqCst), "timeout should close");
    assert_eq!(close.code.load(Ordering::SeqCst), 0x100);
}

#[tokio::test]
async fn test_keepalive_loop_exits_on_send_failure() {
    let (ch_a, ch_b) = pair();
    let (mut sender, mut receiver) = ch_a.split();
    drop(ch_b);

    let cancel = sd_handle();
    let close = RecordingClose::new();
    let cfg = KeepaliveConfig {
        interval: Duration::from_millis(10),
        timeout: Duration::from_secs(60),
    };

    tokio::time::timeout(
        Duration::from_secs(3),
        keepalive_loop(&close, &mut sender, &mut receiver, &cancel, cfg, hb, |_| {
            LoopControl::Continue
        }),
    )
    .await
    .expect("loop exits when send fails");
}

#[tokio::test]
async fn test_keepalive_loop_heartbeat_factory_invoked_per_interval() {
    let (ch_a, _ch_b) = pair();
    let (mut sender, mut receiver) = ch_a.split();
    let cancel = sd_handle();
    let close = RecordingClose::new();

    let count = Arc::new(AtomicU64::new(0));
    let count_clone = count.clone();
    let factory = move || {
        count_clone.fetch_add(1, Ordering::SeqCst);
        hb()
    };

    let cfg = KeepaliveConfig {
        interval: Duration::from_millis(50),
        timeout: Duration::from_secs(60),
    };

    let cancel_clone = cancel.clone();
    let close_clone = close.clone();
    let task = tokio::spawn(async move {
        keepalive_loop(
            &close_clone,
            &mut sender,
            &mut receiver,
            &cancel_clone,
            cfg,
            factory,
            |_| LoopControl::Continue,
        )
        .await;
    });

    tokio::time::sleep(Duration::from_millis(175)).await;
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    assert!(
        count.load(Ordering::SeqCst) >= 2,
        "heartbeat factory invoked at least twice, got {}",
        count.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn test_keepalive_loop_on_msg_break_on_disconnect() {
    let (ch_a, ch_b) = pair();
    let (mut sender_a, mut receiver_a) = ch_a.split();
    let (mut sender_b, _receiver_b) = ch_b.split();

    sender_b.send(text("disconnect")).await.unwrap();
    drop(sender_b);

    let cancel = sd_handle();
    let close = RecordingClose::new();
    tokio::time::timeout(
        Duration::from_secs(3),
        keepalive_loop(
            &close,
            &mut sender_a,
            &mut receiver_a,
            &cancel,
            KeepaliveConfig::default(),
            hb,
            |m| {
                if matches!(&m.msg, Some(test_msg::Msg::Text(s)) if s == "disconnect") {
                    LoopControl::Break
                } else {
                    LoopControl::Continue
                }
            },
        ),
    )
    .await
    .expect("loop exits on Disconnect via on_msg Break");
}

#[tokio::test]
async fn test_keepalive_loop_any_msg_resets_tracker() {
    let (ch_loop, ch_peer) = pair();
    let (loop_sender, loop_receiver) = ch_loop.split();
    let (mut peer_sender, _peer_receiver) = ch_peer.split();

    let cancel = sd_handle();
    let close = RecordingClose::new();
    let close_clone = close.clone();
    let cancel_clone = cancel.clone();
    let mut loop_sender = loop_sender;
    let mut loop_receiver = loop_receiver;

    let loop_task = tokio::spawn(async move {
        keepalive_loop(
            &close_clone,
            &mut loop_sender,
            &mut loop_receiver,
            &cancel_clone,
            fast_cfg(),
            hb,
            |_| LoopControl::Continue,
        )
        .await;
    });

    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = peer_sender.send(text("msg")).await;
    }
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_task).await;
    assert!(
        !close.closed.load(Ordering::SeqCst),
        "messages should reset tracker, preventing timeout"
    );
}

fn repo(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("quic-link crate nested under repo root")
        .join(p)
}

#[tokio::test]
async fn test_keepalive_loop_works_with_real_session() {
    let server = quic_link::Server::builder()
        .tls_from_files(repo("cert.pem"), repo("key.pem"))
        .build("127.0.0.1:0".parse().unwrap())
        .expect("server");
    let addr = server.local_addr().unwrap();
    let client = quic_link::Client::builder()
        .trust_ca(repo("cert.pem"))
        .server_name("localhost")
        .build()
        .expect("client");
    let (server_result, client_result) = tokio::join!(server.accept(), client.connect(addr));
    let server_sess = server_result.unwrap().unwrap();
    let _client_sess = client_result.expect("connect");
    std::mem::forget(client);
    std::mem::forget(server);

    let (ch_a, _ch_b) = pair();
    let (mut sender, mut receiver) = ch_a.split();
    let cancel = sd_handle();
    let cancel_clone = cancel.clone();
    let sess_clone = server_sess.clone();
    let task = tokio::spawn(async move {
        keepalive_loop(
            &sess_clone,
            &mut sender,
            &mut receiver,
            &cancel_clone,
            KeepaliveConfig::default(),
            hb,
            |_| LoopControl::Continue,
        )
        .await;
    });

    tokio::task::yield_now().await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("loop exits")
        .expect("task ok");
}
