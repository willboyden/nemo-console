//! Port of backend/tests/core/test_capable_probe.py: the opt-in active capable-tier probe (addendum 9.11).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
mod support_b;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Env, FAKE_SECRET};
use nemo_console::config::Settings;
use nemo_console::domain::models::{Severity, SystemId, now_ts};
use nemo_console::services::common::KNOWN;
use nemo_console::services::engines::{CapableProbe, ProbeResult, ProbeVerdict, probe_problems};
use nemo_console::services::environ::Environ;
use serde_json::{Map, Value, json};
use support_b::http;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ALIAS: &str = "qwen3.8-flash-next";
const CONTAINER: &str = "ai-lab-sglang-flashnext-flashnext-1";
const ROUTES: &str = r#"
[targets.lightning]
id = "nemotron-lightning"
llm_client = "litellm"

[targets.flash-next]
id = "qwen3.8-flash-next"
llm_client = "litellm"

[routes.auto-local]
id = "auto-local"
type = "stage_router"
capable_target = "flash-next"
efficient_target = "lightning"
"#;

fn ok_body() -> Value {
    json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": "SECRET-ANSWER-TEXT"}}]})
}

fn probe_at(router: &str, env: Option<Environ>, interval: f64) -> Arc<CapableProbe> {
    let environ = env.unwrap_or_else(|| Environ::from_pairs(&[("LITELLM_API_KEY", FAKE_SECRET)]));
    Arc::new(CapableProbe::new(
        http(),
        router,
        "LITELLM_API_KEY",
        interval,
        2.0,
        0.0,
        environ,
    ))
}

async fn router_with(t: ResponseTemplate) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(t)
        .mount(&s)
        .await;
    s
}

// ── one probe request ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_ok_sends_one_tiny_completion_with_the_named_credential() {
    let s = router_with(ResponseTemplate::new(200).set_body_json(ok_body())).await;
    let res = probe_at(&s.uri(), None, 300.0).probe(ALIAS).await;
    assert_eq!(
        (res.verdict, res.detail.as_str()),
        (ProbeVerdict::Ok, "answered")
    );
    let reqs = s.received_requests().await.unwrap();
    let req = reqs.last().unwrap();
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(
        body,
        json!({"model": ALIAS, "messages": [{"role": "user", "content": "ping"}], "max_tokens": 1,
               "temperature": 0, "stream": false, "cache": {"no-cache": true}}) // never a cached answer
    );
    assert_eq!(
        req.headers.get("authorization").unwrap().to_str().unwrap(),
        format!("Bearer {FAKE_SECRET}")
    );
    // registered with the redactor before use
    assert_eq!(KNOWN.scrub(FAKE_SECRET), "[redacted]");
}

#[tokio::test]
async fn test_verdicts() {
    let cases: Vec<(ResponseTemplate, ProbeVerdict, &str)> = vec![
        (
            ResponseTemplate::new(503).set_body_json(json!({"error": "x"})),
            ProbeVerdict::Broken,
            "router answered 503",
        ),
        (
            ResponseTemplate::new(400).set_body_json(json!({"error": "Invalid model name"})),
            ProbeVerdict::Broken,
            "router answered 400",
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"choices": []})),
            ProbeVerdict::Broken,
            "no choices in the answer",
        ),
        (
            ResponseTemplate::new(200).set_body_string("not json"),
            ProbeVerdict::Broken,
            "no choices in the answer",
        ),
        (
            ResponseTemplate::new(401).set_body_json(json!({"error": "no key"})),
            ProbeVerdict::Denied,
            "router answered 401",
        ),
        (
            ResponseTemplate::new(403).set_body_json(json!({"error": "not allowed"})),
            ProbeVerdict::Denied,
            "router answered 403",
        ),
    ];
    for (t, verdict, detail) in cases {
        let s = router_with(t).await;
        let res = probe_at(&s.uri(), None, 300.0).probe(ALIAS).await;
        assert_eq!((res.verdict, res.detail.as_str()), (verdict, detail));
    }
}

#[tokio::test]
async fn test_timeout_and_unreachable_are_broken() {
    let s = router_with(
        ResponseTemplate::new(200)
            .set_body_json(ok_body())
            .set_delay(Duration::from_secs(4)),
    )
    .await;
    let res = probe_at(&s.uri(), None, 300.0).probe(ALIAS).await;
    assert_eq!(
        (res.verdict, res.detail.as_str()),
        (ProbeVerdict::Broken, "no answer within 2 s")
    );
    // a closed port: the connection is refused
    let res = probe_at("http://127.0.0.1:9", None, 300.0)
        .probe(ALIAS)
        .await;
    assert_eq!(
        (res.verdict, res.detail.as_str()),
        (ProbeVerdict::Broken, "router unreachable (ConnectError)")
    );
}

#[tokio::test]
async fn test_without_the_credential_nothing_is_sent() {
    let s = router_with(ResponseTemplate::new(200).set_body_json(ok_body())).await;
    let res = probe_at(&s.uri(), Some(Environ::from_pairs(&[])), 300.0)
        .probe(ALIAS)
        .await;
    assert!(res.verdict == ProbeVerdict::Unconfigured && res.detail.contains("LITELLM_API_KEY"));
    assert_eq!(s.received_requests().await.unwrap().len(), 0);
}

/// A tracing writer that keeps everything written.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// One process-wide capturing subscriber. A thread-local one (`set_default`) is not reliable here: another test
/// thread that hits the library's callsite first caches its interest as "never" (tracing's one-dispatcher fast path).
fn global_capture() -> &'static Capture {
    static CAP: std::sync::OnceLock<Capture> = std::sync::OnceLock::new();
    CAP.get_or_init(|| {
        let cap = Capture::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(cap.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::set_global_default(sub)
            .expect("one global subscriber per test binary");
        cap
    })
}

#[tokio::test]
async fn test_logs_carry_status_only() {
    let s = router_with(ResponseTemplate::new(200).set_body_json(ok_body())).await;
    let cap = global_capture();
    probe_at(&s.uri(), None, 300.0).probe(ALIAS).await;
    // every test of this binary logs here: none of them may leak the answer, the prompt or the key either
    let text = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("capable probe qwen3.8-flash-next: ok"),
        "{text}"
    );
    assert!(
        !text.contains("SECRET-ANSWER-TEXT")
            && !text.contains("ping")
            && !text.contains(FAKE_SECRET)
    );
}

// ── scheduling: background, rate-limited, one at a time ──────────────────────────────────────────

/// A tiny router that measures how many probe requests are in flight at once.
async fn counting_router() -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let (c, p, f) = (
        Arc::clone(&calls),
        Arc::clone(&peak),
        Arc::clone(&in_flight),
    );
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let (c, p, f) = (Arc::clone(&c), Arc::clone(&p), Arc::clone(&f));
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                let now = f.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                f.fetch_sub(1, Ordering::SeqCst);
                axum::Json(ok_body())
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), calls, peak)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_schedule_is_rate_limited_and_sequential() {
    let (url, calls, peak) = counting_router().await;
    let p = probe_at(&url, None, 300.0);
    let abc: Vec<String> = ["a", "b", "c"].map(str::to_owned).to_vec();
    p.schedule(&abc);
    p.schedule(&abc); // a pass is running: ignored
    assert!(p.has_task());
    p.join().await;
    assert_eq!(calls.load(Ordering::SeqCst), 3); // one request per alias
    assert_eq!(peak.load(Ordering::SeqCst), 1); // never two at once
    p.schedule(&abc); // every result is fresh: nothing is due
    p.join().await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    p.set_result(
        "b",
        ProbeResult {
            verdict: ProbeVerdict::Ok,
            detail: "answered".to_owned(),
            at: now_ts() - 301.0,
        },
    );
    p.schedule(&abc);
    assert!(p.has_task());
    p.join().await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    p.aclose().await;
}

#[test]
fn test_interval_below_five_minutes_is_refused() {
    let mut init = Map::new();
    init.insert("capable_probe_interval_s".to_owned(), json!(299.0));
    assert!(Settings::from_sources(None, &[], init).is_err());
}

// ── problems ────────────────────────────────────────────────────────────────────────────────────

#[test]
fn test_probe_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let running = vec![
        (
            ALIAS.to_owned(),
            CONTAINER.to_owned(),
            vec!["auto-local".to_owned()],
        ),
        (
            "other".to_owned(),
            "c-2".to_owned(),
            vec!["r2".to_owned(), "r3".to_owned()],
        ),
    ];
    let now = now_ts();
    let r = |v: ProbeVerdict, d: &str| ProbeResult {
        verdict: v,
        detail: d.to_owned(),
        at: now,
    };
    let one = |pairs: Vec<(&str, ProbeResult)>| -> std::collections::HashMap<String, ProbeResult> {
        pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
    };
    let start = indexmap::IndexMap::new();
    assert!(
        probe_problems(
            &one(vec![(ALIAS, r(ProbeVerdict::Ok, "answered"))]),
            &running,
            root,
            &start
        )
        .is_empty()
    );
    let ps = probe_problems(
        &one(vec![(
            ALIAS,
            r(ProbeVerdict::Broken, "router answered 503"),
        )]),
        &running,
        root,
        &start,
    );
    assert_eq!(ps.len(), 1);
    let p = &ps[0];
    assert!(p.severity == Severity::Warn && p.code == "switchyard.tier.capable_broken");
    assert!(
        p.message
            .contains(&format!("{ALIAS} runs (container {CONTAINER})"))
            && p.message.contains("router answered 503")
    );
    assert!(p.message.contains("Route auto-local cannot escalate"));
    assert!(
        p.fix
            .as_deref()
            .unwrap()
            .starts_with(&format!("docker logs --tail 80 {CONTAINER}"))
    );
    let probs = probe_problems(
        &one(vec![
            (ALIAS, r(ProbeVerdict::Unconfigured, "env var X is not set")),
            (
                "other",
                r(ProbeVerdict::Unconfigured, "env var X is not set"),
            ),
        ]),
        &running,
        root,
        &start,
    );
    let got: Vec<(Severity, &str)> = probs
        .iter()
        .map(|p| (p.severity, p.code.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![(Severity::Info, "switchyard.tier.probe_unconfigured")]
    ); // one, not two
    let ds = probe_problems(
        &one(vec![(
            ALIAS,
            r(ProbeVerdict::Denied, "router answered 401"),
        )]),
        &running,
        root,
        &start,
    );
    assert_eq!(ds.len(), 1);
    assert!(ds[0].code == "switchyard.tier.probe_denied" && ds[0].severity == Severity::Info);
    assert!(
        probe_problems(
            &one(vec![("not-running", r(ProbeVerdict::Broken, "x"))]),
            &running,
            root,
            &start
        )
        .is_empty()
    );
}

// ── end to end through the Switchyard status ─────────────────────────────────────────────────────

async fn switchyard(env: &Env) -> Value {
    env.get("/api/v1/systems/switchyard/status")
        .send()
        .await
        .json()
}

#[tokio::test]
async fn test_off_by_default_sends_nothing() {
    let s = router_with(ResponseTemplate::new(200).set_body_json(ok_body())).await;
    let env = Env::new(Some(&s.uri()));
    std::fs::write(&env.cfg_file, ROUTES).unwrap();
    env.state.status.invalidate(SystemId::Switchyard);
    let st = switchyard(&env).await;
    assert!(env.state.capable_probe.is_none());
    assert_eq!(s.received_requests().await.unwrap().len(), 0);
    assert_eq!(st["health"], "up");
}

#[tokio::test]
async fn test_running_but_broken_engine_degrades_switchyard() {
    let s = router_with(ResponseTemplate::new(503).set_body_json(json!({"error": "rank 1 died"})))
        .await;
    let env = Env::with_settings(
        Some(&s.uri()),
        json!({"capable_probe": true, "capable_probe_retry_s": 0.0}),
    )
    .with_env(&[("LITELLM_API_KEY", FAKE_SECRET)]);
    std::fs::write(&env.cfg_file, ROUTES).unwrap();
    let first = switchyard(&env).await; // schedules the background pass; reports nothing yet
    assert!(
        !first["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "switchyard.tier.capable_broken")
    );
    let probe = env.state.capable_probe.clone().unwrap();
    assert!(probe.has_task());
    probe.join().await;
    env.state.status.invalidate(SystemId::Switchyard);
    let st = switchyard(&env).await;
    let broken: Vec<&Value> = st["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["code"] == "switchyard.tier.capable_broken")
        .collect();
    assert_eq!(broken.len(), 1);
    assert!(
        broken[0]["message"]
            .as_str()
            .unwrap()
            .contains("router answered 503")
    );
    assert!(!st.to_string().contains("rank 1 died"));
    assert_eq!(st["health"], "degraded");
    assert!(
        st["summary"]
            .as_str()
            .unwrap()
            .starts_with("capable tier not answering")
    );
    let reqs = s.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2); // one retry; the second status did not probe again
    let last: Value = serde_json::from_slice(&reqs.last().unwrap().body).unwrap();
    assert_eq!(last["model"], ALIAS);
    probe.aclose().await;
}

async fn sequential_router(first: ResponseTemplate, second: ResponseTemplate) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(first)
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(second)
        .with_priority(2)
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn test_a_failure_is_retried_once_before_it_counts() {
    let s = sequential_router(
        ResponseTemplate::new(503),
        ResponseTemplate::new(200).set_body_json(ok_body()),
    )
    .await;
    let p = probe_at(&s.uri(), None, 300.0);
    p.schedule(&[ALIAS.to_owned()]);
    assert!(p.has_task());
    p.join().await;
    assert_eq!(s.received_requests().await.unwrap().len(), 2);
    assert_eq!(p.results()[ALIAS].verdict, ProbeVerdict::Ok); // busy once, then answered: ok
}

#[tokio::test]
async fn test_two_failures_are_broken_and_say_so() {
    let s = sequential_router(ResponseTemplate::new(503), ResponseTemplate::new(502)).await;
    let p = probe_at(&s.uri(), None, 300.0);
    p.schedule(&[ALIAS.to_owned()]);
    assert!(p.has_task());
    p.join().await;
    let res = &p.results()[ALIAS];
    assert_eq!(
        (res.verdict, res.detail.as_str()),
        (ProbeVerdict::Broken, "router answered 502, twice 0 s apart")
    );
}
