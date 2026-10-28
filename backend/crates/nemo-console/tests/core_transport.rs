//! The real listener (audit findings 4 and 10): the serve loop of the binary on an ephemeral port, spoken to over raw
//! TCP. Covers `dispatch` (the peer address reaches loopback trust), `h11_reject`, the header-read / keep-alive
//! timeout, the connection cap, a truncated body (400, never parsed as complete) and the body timeout (408).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::Env;
use nemo_console::server::{ServeLimits, serve_connections};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const HOST: &str = "127.0.0.1:8793"; // the console's own Host allowlist (settings port), not the socket's

async fn boot(env: &Env, limits: ServeLimits) -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = nemo_console::api::app(env.state.clone());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(serve_connections(listener, app, limits, async move {
        let _ = rx.await;
    }));
    (addr, tx)
}

/// Everything the server sends until it closes or `wait` passes without a byte.
async fn read_all(s: &mut TcpStream, wait: Duration) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match tokio::time::timeout(wait, s.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return (out, true), // closed
            Ok(Ok(n)) => out.extend_from_slice(&buf[..n]),
            Err(_) => return (out, false), // still open
        }
    }
}

fn status(resp: &[u8]) -> u16 {
    let text = String::from_utf8_lossy(resp);
    text.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[tokio::test]
async fn the_real_listener_dispatches_with_the_peer_address() {
    let env = Env::new(None); // trust_loopback: a 127.0.0.1 peer without a key is the admin
    let (addr, _stop) = boot(&env, ServeLimits::DEFAULT).await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(
        format!("GET /api/v1/audit HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    let (resp, _) = read_all(&mut s, Duration::from_secs(5)).await;
    assert_eq!(status(&resp), 200, "{}", String::from_utf8_lossy(&resp));
    // h11's refusal of an HTTP/1.1 request without Host
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /api/v1/health HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    let (resp, _) = read_all(&mut s, Duration::from_secs(5)).await;
    let text = String::from_utf8_lossy(&resp);
    assert_eq!(status(&resp), 400, "{text}");
    assert!(text.ends_with("Invalid HTTP request received."), "{text}");
}

#[tokio::test]
async fn idle_and_slow_connections_are_closed() {
    let env = Env::new(None);
    let limits = ServeLimits {
        header_read_timeout: Duration::from_millis(300),
        max_connections: 16,
    };
    let (addr, _stop) = boot(&env, limits).await;
    // idle keep-alive after one answered request
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET /api/v1/health HTTP/1.1\r\nHost: {HOST}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let (resp, closed) = read_all(&mut s, Duration::from_millis(1500)).await;
    assert_eq!(status(&resp), 200);
    assert!(closed, "an idle keep-alive connection must be closed");
    // headers that never finish
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /api/v1/health HTTP/1.1\r\nHo")
        .await
        .unwrap();
    let (_, closed) = read_all(&mut s, Duration::from_millis(1500)).await;
    assert!(
        closed,
        "a connection that never sends its headers must be closed"
    );
}

#[tokio::test]
async fn connections_over_the_cap_are_closed_at_once() {
    let env = Env::new(None);
    let limits = ServeLimits {
        header_read_timeout: Duration::from_secs(10),
        max_connections: 1,
    };
    let (addr, _stop) = boot(&env, limits).await;
    let _held = TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    let (_, closed) = read_all(&mut s, Duration::from_millis(500)).await;
    assert!(closed, "a connection over the cap must be closed");
}

#[tokio::test]
async fn a_truncated_body_is_refused_not_parsed() {
    let env = Env::new(None);
    let (addr, _stop) = boot(&env, ServeLimits::DEFAULT).await;
    let body = br#"{"poll_interval_s": 5}"#;
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(
        format!(
            "PUT /api/v1/settings HTTP/1.1\r\nHost: {HOST}\r\nX-Console: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len() + 20
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    s.write_all(body).await.unwrap();
    s.shutdown().await.unwrap(); // the client goes away before the announced length
    let _ = read_all(&mut s, Duration::from_secs(3)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let v = env.get("/api/v1/settings").send().await.json();
    assert_ne!(
        v["poll_interval_s"], 5.0,
        "the partial body was applied: {v}"
    );
}

#[tokio::test]
async fn a_body_that_never_arrives_times_out() {
    let env = Env::new(None);
    let (addr, _stop) = boot(&env, ServeLimits::DEFAULT).await;
    nemo_console::api::BODY_READ_TIMEOUT_MS.store(300, Ordering::Relaxed);
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(
        format!(
            "PUT /api/v1/settings HTTP/1.1\r\nHost: {HOST}\r\nX-Console: 1\r\nContent-Type: application/json\r\nContent-Length: 40\r\n\r\n{{\"p"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let (resp, _) = read_all(&mut s, Duration::from_secs(3)).await;
    nemo_console::api::BODY_READ_TIMEOUT_MS.store(30_000, Ordering::Relaxed);
    assert_eq!(status(&resp), 408, "{}", String::from_utf8_lossy(&resp));
}
