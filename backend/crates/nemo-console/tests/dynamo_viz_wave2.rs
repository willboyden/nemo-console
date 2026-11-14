//! Addendum 9.14, Dynamo: windowed TTFT / ITL / phase quantiles (the requests that finished since the previous
//! poll, from histogram bucket deltas), preemptions, capacity and spec-decode acceptance per draft position. Driven
//! by the CAPTURED frontend and worker scrapes; later polls add SYNTHETIC observations to their buckets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::needless_pass_by_value
)]

mod dynamo_support;
mod switchyard_support;

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use dynamo_support::{Fix, fx, is_docker, worker_doc};
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::dynamo::{
    DynamoAdapter, HistCounts, HistWindows, bucket_quantile, hist_counts, hist_delta, parse_prom,
};
use nemo_console::domain::models::MetricPoint;
use nemo_console::services::metrics::{MAX_SERIES_PER_SYSTEM, series_name};
use switchyard_support::{Reply, ok};

const TTFT: &str = "dynamo_frontend_time_to_first_token_seconds";
const ITL: &str = "dynamo_frontend_inter_token_latency_seconds";
const QUEUE: &str = "vllm:request_queue_time_seconds";

const WINDOWED_KEYS: [&str; 3] = ["dyn.ttft_window_ms", "dyn.itl_window_ms", "dyn.phase_ms"];

/// Add `n` observations of a value just above `above` to `family` in a Prometheus text: every `_bucket` line with
/// `le` > `above` (cumulative buckets, `+Inf` included) and the `_count` line grow by `n`. SYNTHETIC.
fn bump(text: &str, family: &str, above: f64, n: f64) -> String {
    let bucket = format!("{family}_bucket{{");
    let count = format!("{family}_count");
    let mut out = String::new();
    for line in text.lines() {
        let grow = if line.starts_with(&bucket) {
            let le = line
                .split("le=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            let le: f64 = if le == "+Inf" {
                f64::INFINITY
            } else {
                le.parse().unwrap()
            };
            le > above
        } else {
            line.starts_with(&count)
        };
        if grow {
            let (head, val) = line.rsplit_once(' ').unwrap();
            let v: f64 = val.parse().unwrap();
            out.push_str(&format!("{head} {}\n", v + n));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `key` -> {sorted labels -> value}.
fn by(points: &[MetricPoint], key: &str) -> BTreeMap<Vec<(String, String)>, f64> {
    points
        .iter()
        .filter(|p| p.key == key)
        .map(|p| {
            let mut l: Vec<(String, String)> = p
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            l.sort();
            (l, p.value)
        })
        .collect()
}

fn lab(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut l: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    l.sort();
    l
}

fn windowed(points: &[MetricPoint]) -> Vec<&MetricPoint> {
    points
        .iter()
        .filter(|p| WINDOWED_KEYS.contains(&p.key.as_str()))
        .collect()
}

/// A live frontend and worker whose `/metrics` texts a test changes between polls.
struct Live {
    f: Fix,
    a: Arc<DynamoAdapter>,
    worker: Arc<Mutex<String>>,
}

impl Live {
    async fn new() -> Live {
        let f = Fix::new().await;
        dynamo_support::wire(&f.runner, vec![worker_doc()], "");
        let worker = Arc::new(Mutex::new(fx("worker_metrics.prom")));
        let w = Arc::clone(&worker);
        let health = fx("worker_health.json");
        f.runner.on_fn(is_docker("exec"), move |_a: &[String]| {
            ok(&format!(
                "{}\n---METRICS---\n{}",
                health.trim(),
                w.lock().unwrap()
            ))
        });
        dynamo_support::http_live(&f.mock, true);
        // every read is a poll here (the container sets 90% of the poll interval)
        let a = f.make_with(|a| a.window_min_s = 0.0);
        Live { f, a, worker }
    }

    fn frontend(&self, text: &str) {
        self.f.mock.get("/metrics", Reply::text(200, text));
    }

    fn worker(&self, text: &str) {
        *self.worker.lock().unwrap() = text.to_owned();
    }

    async fn poll(&self) -> Vec<MetricPoint> {
        let snap = self.a.metrics().await.unwrap();
        assert!(snap.available, "{:?}", snap.error);
        snap.points
    }
}

// ── the window arithmetic ───────────────────────────────────────────────────────────────────────────

fn counts(text: &str, family: &str) -> HistCounts {
    hist_counts(&parse_prom(text), family)
}

#[test]
fn test_hist_delta_sums_label_sets_and_sees_resets() {
    let fe = fx("frontend_metrics.prom");
    let before = counts(&fe, TTFT);
    assert_eq!(before.len(), 1, "one model label set");
    let after = counts(&bump(&fe, TTFT, 0.1, 3.0), TTFT);
    let d = hist_delta(&before, &after).unwrap();
    // three observations, all in (0.1, 0.22]
    assert_eq!(d.iter().map(|x| x.1).fold(0.0, f64::max), 3.0);
    let p50 = bucket_quantile(d, 0.5).unwrap();
    assert!((p50 - 0.16).abs() < 1e-12, "{p50}");
    // nothing new: zero deltas, no quantile
    let none = hist_delta(&after, &after).unwrap();
    assert!(none.iter().all(|x| x.1 == 0.0));
    assert_eq!(bucket_quantile(none, 0.5), None);
    // a bucket that went down is a reset
    assert_eq!(hist_delta(&after, &before), None);
    // a label set only one side has adds nothing (a worker restarted under a new id)
    let renamed = counts(&fe.replace("Qwen3.8-27B-NVFP4\",le", "Other\",le"), TTFT);
    assert!(hist_delta(&before, &renamed).unwrap().is_empty());
}

#[test]
fn test_windows_track_the_previous_poll() {
    let fe = fx("frontend_metrics.prom");
    let mut w = HistWindows::default();
    let c0 = counts(&fe, TTFT);
    let c1 = counts(&bump(&fe, TTFT, 0.1, 2.0), TTFT);
    let c2 = counts(&bump(&bump(&fe, TTFT, 0.1, 2.0), TTFT, 1.0, 4.0), TTFT);
    // first read: no window yet
    assert_eq!(w.observe(TTFT, 0.0, 1.8, Some(c0.clone())), None);
    // the poller 2 s later: two observations
    let d = w.observe(TTFT, 2.0, 1.8, Some(c1.clone())).unwrap();
    assert_eq!(bucket_quantile(d, 1.0).unwrap(), 0.22);
    // an extra read 0.5 s later compares with the newest read at least 1.8 s old (the first: 2 + 4 new) ...
    let extra = w.observe(TTFT, 2.5, 1.8, Some(c2.clone())).unwrap();
    assert_eq!(extra.iter().map(|x| x.1).fold(0.0, f64::max), 6.0);
    // ... and the next poll still compares with the previous poll (2 s back), not with the extra read
    let d = w.observe(TTFT, 4.0, 1.8, Some(c2.clone())).unwrap();
    assert_eq!(d.iter().map(|x| x.1).fold(0.0, f64::max), 4.0);
    // nothing new since then: no quantile
    let d = w.observe(TTFT, 6.0, 1.8, Some(c2.clone())).unwrap();
    assert_eq!(bucket_quantile(d, 0.5), None);
    // a counter reset: no window this poll, the reset read is the new baseline
    assert_eq!(w.observe(TTFT, 8.0, 1.8, Some(c0)), None);
    let d = w.observe(TTFT, 10.0, 1.8, Some(c1)).unwrap();
    assert_eq!(d.iter().map(|x| x.1).fold(0.0, f64::max), 2.0);
    // the family disappears: its history is dropped, so the next read starts afresh
    assert_eq!(w.observe(TTFT, 12.0, 1.8, None), None);
    assert_eq!(w.observe(TTFT, 14.0, 1.8, Some(c2)), None);
    assert_eq!(w.observe(TTFT, 16.0, 1.8, Some(HistCounts::new())), None);
}

// ── the adapter ─────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_windowed_quantiles_across_polls() {
    let live = Live::new().await;
    let (fe0, wk0) = (fx("frontend_metrics.prom"), fx("worker_metrics.prom"));
    // first poll: the since-start keys are there, the windowed ones not yet
    let p1 = live.poll().await;
    assert!(by(&p1, "dyn.ttft_ms").len() == 2 && windowed(&p1).is_empty());
    // second poll: 3 requests finished with TTFT in (0.1, 0.22] s, 1 with queue time in (0.3, 0.5] s
    let fe1 = bump(&bump(&fe0, TTFT, 0.1, 3.0), ITL, 0.013, 10.0);
    let wk1 = bump(&wk0, QUEUE, 0.3, 1.0);
    let wk1 = bump(&wk1, "vllm:request_prefill_time_seconds", 0.5, 1.0);
    let wk1 = bump(&wk1, "vllm:request_decode_time_seconds", 2.0, 1.0);
    let wk1 = bump(&wk1, "vllm:request_inference_time_seconds", 2.0, 1.0);
    live.frontend(&fe1);
    live.worker(&wk1);
    let p2 = live.poll().await;
    let ttft = by(&p2, "dyn.ttft_window_ms");
    assert_eq!(
        ttft.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from([
            lab(&[("q", "p50")]),
            lab(&[("q", "p95")]),
            lab(&[("q", "p99")])
        ])
    );
    assert!((ttft[&lab(&[("q", "p50")])] - 160.0).abs() < 1e-9);
    assert!(ttft[&lab(&[("q", "p99")])] <= 220.0 + 1e-9);
    // the since-start TTFT still includes the first request (0.302 s): its p95 is above anything in the window
    assert!(by(&p2, "dyn.ttft_ms")[&lab(&[("q", "p95")])] > 220.0);
    let itl = by(&p2, "dyn.itl_window_ms");
    assert!(itl[&lab(&[("q", "p50")])] > 13.0 && itl[&lab(&[("q", "p50")])] <= 24.0);
    let phase = by(&p2, "dyn.phase_ms");
    assert_eq!(phase.len(), 8, "4 phases x p50/p95: {phase:?}");
    assert!((phase[&lab(&[("phase", "queue"), ("q", "p50")])] - 400.0).abs() < 1e-9);
    for ph in ["queue", "prefill", "decode", "inference"] {
        let (p50, p95) = (
            phase[&lab(&[("phase", ph), ("q", "p50")])],
            phase[&lab(&[("phase", ph), ("q", "p95")])],
        );
        assert!(p50 > 0.0 && p50 <= p95, "{ph}");
    }
    assert!(
        p2.iter()
            .filter(|p| WINDOWED_KEYS.contains(&p.key.as_str()))
            .all(|p| p.unit == "ms")
    );
    // third poll, nothing finished: no windowed point (never 0)
    let p3 = live.poll().await;
    assert!(windowed(&p3).is_empty(), "{:?}", windowed(&p3));
    // fourth poll: the frontend restarted (counters back to the capture): a reset, skipped
    live.frontend(&fe0);
    let p4 = live.poll().await;
    assert!(by(&p4, "dyn.ttft_window_ms").is_empty());
    // fifth poll: one more request after the restart, measured from the reset read
    live.frontend(&bump(&fe0, TTFT, 1.0, 1.0));
    let p5 = live.poll().await;
    let t = by(&p5, "dyn.ttft_window_ms");
    let p50 = t[&lab(&[("q", "p50")])];
    assert!(p50 > 1000.0 && p50 <= 2200.0, "{p50}");
    // the worker stops being read: its windows start afresh
    live.f.runner.clear_rules();
    dynamo_support::wire(&live.f.runner, vec![], "");
    let p6 = live.poll().await;
    assert!(by(&p6, "dyn.phase_ms").is_empty());
}

#[tokio::test]
async fn test_a_frontend_outage_restarts_the_windows() {
    let live = Live::new().await;
    let fe0 = fx("frontend_metrics.prom");
    live.poll().await;
    live.f.mock.get("/metrics", Reply::Close);
    assert!(!live.a.metrics().await.unwrap().available);
    // back with more requests: the first read after the gap is a new baseline, not a window over the outage
    live.frontend(&bump(&fe0, TTFT, 0.1, 5.0));
    assert!(by(&live.poll().await, "dyn.ttft_window_ms").is_empty());
    live.frontend(&bump(&bump(&fe0, TTFT, 0.1, 5.0), TTFT, 0.1, 1.0));
    assert_eq!(by(&live.poll().await, "dyn.ttft_window_ms").len(), 3);
}

#[tokio::test]
async fn test_capacity_preemptions_and_acceptance_per_position() {
    let live = Live::new().await;
    let pts = live.poll().await;
    assert_eq!(
        by(&pts, "dyn.capacity"),
        BTreeMap::from([
            (lab(&[("kind", "context_length")]), 262_144.0),
            (lab(&[("kind", "kv_blocks")]), 359.0),
            (lab(&[("kind", "max_batched_tokens")]), 8192.0),
            (lab(&[("kind", "max_num_seqs")]), 64.0),
        ])
    );
    assert_eq!(
        by(&pts, "dyn.preemptions_total"),
        BTreeMap::from([(vec![], 0.0)])
    );
    // 15 drafts: position 0 accepted 15 times, position 1 11 times
    assert_eq!(
        by(&pts, "dyn.spec_accept_pos"),
        BTreeMap::from([
            (lab(&[("pos", "0")]), 1.0),
            (lab(&[("pos", "1")]), 11.0 / 15.0)
        ])
    );
    let unit = |k: &str| pts.iter().find(|p| p.key == k).unwrap().unit.clone();
    assert_eq!(unit("dyn.spec_accept_pos"), "ratio");
    assert_eq!(unit("dyn.preemptions_total"), "1");
    // valid series names, unique, well under the cap
    let names: HashSet<String> = pts.iter().map(series_name).collect();
    assert_eq!(names.len(), pts.len());
    assert!(
        names
            .iter()
            .all(|n| nemo_console::api::observe::KEY_RE.is_match(n)),
        "{names:?}"
    );
    assert!(names.len() < MAX_SERIES_PER_SYSTEM / 4, "{}", names.len());
}

#[tokio::test]
async fn test_without_a_worker_or_families_the_new_keys_are_absent() {
    let f = Fix::new().await;
    dynamo_support::wire(&f.runner, vec![], "");
    f.mock.get("/metrics", Reply::text(200, "# nothing\n"));
    let a = f.make_with(|a| a.window_min_s = 0.0);
    for _ in 0..2 {
        let snap = a.metrics().await.unwrap();
        let keys: HashSet<&str> = snap.points.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, HashSet::from(["dyn.up", "dyn.worker_up"]));
    }
    // the frontend alone: capacity yes, worker keys no
    dynamo_support::http_live(&f.mock, true);
    let snap = a.metrics().await.unwrap();
    let keys: HashSet<&str> = snap.points.iter().map(|p| p.key.as_str()).collect();
    assert!(keys.contains("dyn.capacity"));
    for absent in [
        "dyn.preemptions_total",
        "dyn.spec_accept_pos",
        "dyn.phase_ms",
    ] {
        assert!(!keys.contains(absent), "{absent}");
    }
}

#[test]
fn test_the_container_sets_the_window_from_the_poll_interval() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = nemo_console::config::settings_for_test(serde_json::json!({
        "state_dir": tmp.path().join("s").to_string_lossy(),
        "frontend_dir": tmp.path().join("f").to_string_lossy(),
        "repo_root": tmp.path().join("r").to_string_lossy(),
        "poll_interval_s": 5.0,
    }));
    let ctx = switchyard_support::ctx(
        switchyard_support::FakeRunner::new(),
        &[("dynamo", "http://127.0.0.1:9")],
        &[],
        tmp.path(),
    );
    let a = DynamoAdapter::from_settings(&ctx, &cfg);
    assert!((a.window_min_s - 4.5).abs() < 1e-12);
}
