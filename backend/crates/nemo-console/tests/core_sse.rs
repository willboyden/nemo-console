//! Port of backend/tests/core/test_sse.py: the event stream delivers redacted events, caps connections, ends on
//! idle / consumer cancel / server shutdown and always frees its slot.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use common::Env;
use futures::StreamExt;
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::domain::models::{SystemEvent, SystemId, now_ts};
use nemo_console::services::events::{Shutdown, SseLimiter, event_stream};
use serde_json::{Value, json};

fn ev(n: u32) -> SystemEvent {
    SystemEvent {
        ts: now_ts(),
        system: SystemId::Switchyard,
        kind: "routing.decision".to_owned(),
        summary: format!("e{n} Bearer abcdefghijklmnop"),
        data: json!({"route": "r", "api_key": "hidden-value"})
            .as_object()
            .unwrap()
            .clone(),
    }
}

fn sy(env: &Env) -> Arc<dyn SystemAdapter> {
    Arc::clone(env.a(SystemId::Switchyard)) as Arc<dyn SystemAdapter>
}

#[tokio::test]
async fn test_stream_delivers_events_redacted_and_releases_slot() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Switchyard).knobs().follow = vec![ev(1), ev(2)];
    let r = env
        .get("/api/v1/systems/switchyard/events/stream")
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("text/event-stream")
    );
    let text = r.text();
    let datas: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str(d).unwrap())
        .collect();
    let first: Vec<&str> = datas
        .iter()
        .map(|d| {
            d["summary"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
        })
        .collect();
    assert_eq!(first, ["e1", "e2"]);
    assert!(!text.contains("abcdefghijklmnop") && !text.contains("hidden-value"));
    assert_eq!(env.state.sse.active(), 0);
    assert_eq!(r.header("cache-control").unwrap(), "no-store");
}

#[tokio::test]
async fn test_connection_cap_returns_429() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    assert!(env.state.sse.acquire() && env.state.sse.acquire()); // cap is 2 in the test settings
    let r = env
        .get("/api/v1/systems/switchyard/events/stream")
        .send()
        .await;
    assert_eq!(r.status, 429);
    assert_eq!(r.code(), "too_many_streams");
    assert!(r.headers.contains_key("content-security-policy"));
    env.state.sse.release();
    env.a(SystemId::Switchyard).knobs().follow = vec![];
    assert_eq!(
        env.get("/api/v1/systems/switchyard/events/stream")
            .send()
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn test_idle_timeout_closes_stream_and_frees_slot() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Switchyard).knobs().follow_block = true;
    let lim = Arc::new(SseLimiter::new(1));
    let t0 = Instant::now();
    let chunks: Vec<Bytes> =
        event_stream(sy(&env), Arc::clone(&lim), 0.3, 0.1, Shutdown::default())
            .collect()
            .await;
    assert!(t0.elapsed() < Duration::from_secs(2));
    let has = |s: &str| {
        chunks
            .iter()
            .any(|c| String::from_utf8_lossy(c).contains(s))
    };
    assert!(has("idle-timeout") && has("keep-alive"));
    assert_eq!(lim.active(), 0);
}

#[tokio::test]
async fn test_cancelled_consumer_releases_slot() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Switchyard).knobs().follow_block = true;
    let lim = Arc::new(SseLimiter::new(1));
    let mut stream = Box::pin(event_stream(
        sy(&env),
        Arc::clone(&lim),
        30.0,
        0.05,
        Shutdown::default(),
    ));
    let task = tokio::spawn(async move { while stream.next().await.is_some() {} });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(lim.active(), 1);
    task.abort();
    let _ = task.await;
    assert_eq!(lim.active(), 0);
}

#[tokio::test]
async fn test_server_shutdown_ends_open_streams_cleanly() {
    // 2026-10-24: a browser's stream kept the process alive on SIGTERM; begin_shutdown() (the server's exit hook)
    // must end every open stream at once with `event: shutdown` and free its slot.
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Switchyard).knobs().follow_block = true;
    let lim = Arc::new(SseLimiter::new(1));
    let shutdown = Shutdown::default();
    let chunks: Arc<Mutex<Vec<Bytes>>> = Arc::new(Mutex::new(Vec::new()));
    let (sink, stream) = (
        Arc::clone(&chunks),
        event_stream(sy(&env), Arc::clone(&lim), 60.0, 30.0, shutdown.clone()),
    );
    let task = tokio::spawn(async move {
        let mut s = Box::pin(stream);
        while let Some(c) = s.next().await {
            sink.lock().unwrap().push(c);
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await; // connected, waiting on a 30 s heartbeat
    let t0 = Instant::now();
    shutdown.begin();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1));
    let last = chunks.lock().unwrap().last().cloned().unwrap();
    assert!(last.starts_with(b"event: shutdown"));
    assert_eq!(lim.active(), 0);
}
