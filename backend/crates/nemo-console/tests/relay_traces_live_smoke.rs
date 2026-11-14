//! Opt-in READ-ONLY run of the Relay trace index (addendum 9.12) over this workstation's real capture directories
//! (NEMO_CONSOLE_LIVE=1; skipped otherwise, before anything is touched). It prints aggregate numbers only: counts,
//! times and sizes. Nothing from a capture (no name, id, model or text) is printed or asserted on.
//!
//!   NEMO_CONSOLE_LIVE=1 cargo test --release --test relay_traces_live_smoke -- --nocapture
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::sync::Arc;
use std::time::Instant;

use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, SystemAdapter};
use nemo_console::adapters::relay::RelayAdapter;
use nemo_console::adapters::relay_traces::analytics::AnalyticsQuery;
use nemo_console::adapters::relay_traces::query::{
    Range, SessionQuery, SessionSort, TraceQuery, TraceSort, TrajQuery, Window,
};
use nemo_console::adapters::relay_traces::{Request, TraceConfig};
use nemo_console::config::home_dir;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use nemo_console::services::runner::SubprocessRunner;
use serde_json::Value;

/// Resident memory of this process in MiB.
fn rss_mib() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    pages * 4096 / (1024 * 1024)
}

fn count_where(items: &[Value], f: impl Fn(&Value) -> bool) -> usize {
    items.iter().filter(|v| f(v)).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_live_index_of_the_real_capture_directories() {
    if std::env::var("NEMO_CONSOLE_LIVE").ok().as_deref() != Some("1") {
        return; // live smoke is opt-in (NEMO_CONSOLE_LIVE=1)
    }
    let base = home_dir().join(".local/share/nemo-relay");
    let paths = IndexMap::from([
        ("relay_atof_dir".to_owned(), base.join("atof")),
        ("relay_atif_dir".to_owned(), base.join("atif")),
        ("relay_hermes_dir".to_owned(), base.join("hermes")),
        (
            "relay_plugins".to_owned(),
            home_dir().join(".config/nemo-relay/plugins.toml"),
        ),
    ]);
    let mut cfg = TraceConfig::default();
    if let Some(n) = std::env::var("NEMO_TRACE_MAX_SPANS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        cfg.limits.max_spans = n;
    }
    if let Some(n) = std::env::var("NEMO_TRACE_SCAN_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        cfg.limits.scan_bytes = n;
    }
    let a = Arc::new(RelayAdapter::with_traces(
        AdapterContext {
            runner: Arc::new(SubprocessRunner::with_environ(&[], Environ::process())),
            http: Http::new().unwrap(),
            urls: IndexMap::new(),
            paths,
            secret_env: IndexMap::new(),
            state_dir: std::env::temp_dir(),
            environ: Environ::process(),
        },
        cfg,
    ));
    let rss0 = rss_mib();
    let t0 = Instant::now();
    // the first pass on its own thread; meanwhile: how soon does a reader see data?
    let worker = Arc::clone(&a);
    let pass = std::thread::spawn(move || worker.traces.refresh_blocking());
    let mut first_data_ms = None;
    while !pass.is_finished() {
        let st = a.traces.snapshot().index_state();
        if first_data_ms.is_none() && st["traces"].as_u64().unwrap_or(0) > 0 {
            first_data_ms = Some(t0.elapsed().as_millis());
            println!(
                "first traces visible after {} ms ({} traces, state {})",
                t0.elapsed().as_millis(),
                st["traces"],
                st["state"]
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    pass.join().unwrap();
    let build = t0.elapsed();
    let st = a.traces.snapshot().index_state();
    let files = st["files"].as_array().unwrap();
    println!(
        "full build: {} ms; resident memory {} MiB -> {} MiB",
        build.as_millis(),
        rss0,
        rss_mib()
    );
    println!(
        "files: {} ({} indexed, {} partial, {} skipped, {} error); bytes on disk {}; skipped lines {}",
        files.len(),
        count_where(files, |f| f["state"] == "indexed"),
        count_where(files, |f| f["state"] == "partial"),
        count_where(files, |f| f["state"] == "skipped"),
        count_where(files, |f| f["state"] == "error"),
        files
            .iter()
            .map(|f| f["bytes"].as_u64().unwrap_or(0))
            .sum::<u64>(),
        files
            .iter()
            .map(|f| f["skipped_lines"].as_u64().unwrap_or(0))
            .sum::<u64>(),
    );
    println!(
        "state {}; events {}; spans {} ({} open); marks {}; chunk marks {}; traces {}; sessions {}; trajectories {}; truncated {}",
        st["state"],
        st["events"],
        st["spans"],
        st["open_spans"],
        st["marks"],
        st["chunk_marks"],
        st["traces"],
        st["sessions"],
        st["trajectories"],
        st["truncated"]
    );
    println!(
        "problem codes: {:?}",
        st["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["code"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
    );
    assert!(matches!(st["state"].as_str(), Some("ready" | "empty")));

    // a second pass with nothing new must be cheap and must not change the counts
    let t1 = Instant::now();
    a.traces.refresh_blocking();
    let st2 = a.traces.snapshot().index_state();
    println!(
        "second pass (nothing new unless the gateway wrote meanwhile): {} ms",
        t1.elapsed().as_millis()
    );
    assert!(st2["events"].as_u64() >= st["events"].as_u64());

    // every view answers; time each one
    let all = Range {
        window: Window::All,
        anchor_now: false,
    };
    let timed = |name: &'static str, t: Instant, v: &Value| {
        println!(
            "{name}: {} ms, {} bytes of JSON",
            t.elapsed().as_millis(),
            v.to_string().len()
        );
    };
    let t = Instant::now();
    let traces = a
        .trace_explorer(Request::Traces(TraceQuery {
            range: all,
            client: None,
            model: None,
            kind: None,
            status: None,
            session: None,
            q: None,
            min_ms: 0,
            sort: TraceSort::Tokens,
            desc: true,
            limit: 200,
            offset: 0,
        }))
        .await
        .unwrap();
    timed("traces (all, by tokens)", t, &traces);
    let items = traces["items"].as_array().unwrap();
    println!(
        "of the {} listed traces: {} with a client, {} with tokens, {} with routing, {} errors, {} open, {} with a trajectory",
        items.len(),
        count_where(items, |t| !t["client"].is_null()),
        count_where(items, |t| !t["total_tokens"].is_null()),
        count_where(items, |t| !t["routing"].is_null()),
        count_where(items, |t| t["status"] == "error"),
        count_where(items, |t| t["status"] == "open"),
        count_where(items, |t| t["has_trajectory"] == true),
    );
    let t = Instant::now();
    let sessions = a
        .trace_explorer(Request::Sessions(SessionQuery {
            range: all,
            client: None,
            kind: None,
            q: None,
            sort: SessionSort::Tokens,
            desc: true,
            limit: 200,
            offset: 0,
        }))
        .await
        .unwrap();
    timed("sessions (all)", t, &sessions);
    let t = Instant::now();
    let an = a
        .trace_explorer(Request::Analytics(AnalyticsQuery {
            range: all,
            client: None,
            model: None,
            kind: None,
            buckets: 240,
        }))
        .await
        .unwrap();
    timed("analytics (all, 240 buckets)", t, &an);
    println!(
        "analytics totals: llm_calls {}, tool_calls {}, errors {}, input tokens known {}, ttfc known {}, models {}, tools {}, routing requests {}",
        an["totals"]["llm_calls"],
        an["totals"]["tool_calls"],
        an["totals"]["errors"],
        !an["totals"]["input_tokens"].is_null(),
        !an["totals"]["ttfc_p50_ms"].is_null(),
        an["by_model"].as_array().unwrap().len(),
        an["tools"].as_array().unwrap().len(),
        an["routing"]["requests"],
    );
    // the largest trace in full, and how complete its LLM spans are
    if let Some(id) = items.first().and_then(|t| t["trace_id"].as_str()) {
        let t = Instant::now();
        let d = a
            .trace_explorer(Request::Trace {
                trace_id: id.to_owned(),
            })
            .await
            .unwrap();
        timed("trace detail (largest by tokens)", t, &d);
        let spans = d["spans"].as_array().unwrap();
        let llm: Vec<&Value> = spans.iter().filter(|s| !s["llm"].is_null()).collect();
        let has = |k: &str| llm.iter().filter(|s| !s["llm"][k].is_null()).count();
        println!(
            "its {} spans: {} llm (model {}, served {}, input {}, ttfc {}, finish {}, messages {}, chunks {}), {} tool, {} with content",
            spans.len(),
            llm.len(),
            has("model"),
            has("served_model"),
            has("input_tokens"),
            has("ttfc_ms"),
            has("finish_reason"),
            has("messages"),
            has("chunks"),
            count_where(spans, |s| !s["tool"].is_null()),
            count_where(spans, |s| s["content_available"] == true),
        );
        // content of one LLM span of it (sizes only)
        if let Some(sp) = llm.iter().rev().find(|s| s["content_available"] == true) {
            let t = Instant::now();
            let c = a
                .trace_explorer(Request::SpanContent {
                    trace_id: id.to_owned(),
                    span_id: sp["id"].as_str().unwrap().to_owned(),
                    query: nemo_console::adapters::relay_traces::content::ContentQuery {
                        max_chars: 32_768,
                        head: false,
                    },
                })
                .await;
            match c {
                Ok(c) => {
                    println!(
                        "span content: {} ms, {} bytes of JSON, compressed source {}, messages {}/{}, truncated {}, redactions {}",
                        t.elapsed().as_millis(),
                        c.to_string().len(),
                        c["source"]["compressed"],
                        c["request"]["messages_returned"],
                        c["request"]["messages_total"],
                        c["truncated"],
                        c["redactions"]
                    );
                    assert!(c.to_string().len() <= 4 * 1024 * 1024);
                }
                Err(e) => println!(
                    "span content: {} after {} ms",
                    e.py_class(),
                    t.elapsed().as_millis()
                ),
            }
        }
    }
    let t = Instant::now();
    let tj = a
        .trace_explorer(Request::Trajectories(TrajQuery {
            source: None,
            q: None,
            trace: None,
            limit: 200,
            offset: 0,
        }))
        .await
        .unwrap();
    timed("trajectories", t, &tj);
    let tjs = tj["items"].as_array().unwrap();
    println!(
        "trajectories listed {}: {} linked to a trace, {} with token totals",
        tjs.len(),
        count_where(tjs, |t| !t["trace_id"].is_null()),
        count_where(tjs, |t| !t["prompt_tokens"].is_null()),
    );
    if let Some(big) = tjs.iter().max_by_key(|t| t["bytes"].as_u64().unwrap_or(0)) {
        let t = Instant::now();
        let d = a
            .trace_explorer(Request::Trajectory {
                id: big["id"].as_str().unwrap().to_owned(),
                source: big["source"].as_str().map(str::to_owned),
            })
            .await
            .unwrap();
        timed("trajectory detail (largest listed file)", t, &d);
    }
    let t = Instant::now();
    let cap = a.trace_explorer(Request::Capture).await.unwrap();
    timed("capture", t, &cap);
    println!(
        "capture levels: {:?}",
        cap["levels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| format!(
                "{}={}",
                l["id"].as_str().unwrap_or(""),
                l["state"].as_str().unwrap_or("")
            ))
            .collect::<Vec<_>>()
    );
    println!("resident memory at the end: {} MiB", rss_mib());
    a.traces.aclose().await;
}
