//! Audit finding 10: header values are decoded as latin-1 like Starlette, so a non-ASCII `Origin` is a (foreign)
//! origin and is refused, not mistaken for an absent header.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::Env;
use serde_json::json;

#[tokio::test]
async fn a_non_ascii_origin_is_refused_like_python() {
    let env = Env::new(None);
    // the console's own origin with one non-ASCII byte appended: Python reads it as latin-1 and compares
    let r = env
        .post("/api/v1/systems/switchyard/actions/restart", json!({}))
        .header("origin", "http://127.0.0.1:8793\u{e9}")
        .send()
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert_eq!(r.json()["error"]["code"], "cross_origin");
    // the same request with the plain origin passes the Origin check
    let r = env
        .post("/api/v1/systems/switchyard/actions/restart", json!({}))
        .header("origin", "http://127.0.0.1:8793")
        .send()
        .await;
    assert_ne!(r.json()["error"]["code"], "cross_origin", "{}", r.text());
}
