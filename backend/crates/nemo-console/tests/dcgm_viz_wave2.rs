//! Addendum 9.14, DCGM: the new per-GPU metric keys and the fleet route `GET /systems/dcgm/fleet` (Rust-only,
//! PORTING.md D-38). Hermetic: the captured exporter scrape plus SYNTHETIC lines for the families it lacks (zeroed
//! UUIDs, no real host names), a fake Prometheus on an ephemeral port (wiremock), closed ports for everything else.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

mod common;
#[path = "fixtures/dcgm/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use env::Env;
use nemo_console::adapters::base::{AdapterError, CommandRunner, SystemAdapter};
use nemo_console::adapters::dcgm::{
    DcgmAdapter, FLEET_MAX_ROWS, FLEET_METRICS, FLEET_WINDOWS, fleet_query, gpu_points_9_14,
    parse_exposition,
};
use nemo_console::config::settings_for_test;
use nemo_console::container::{AppState, Overrides, build_container};
use nemo_console::domain::models::{MetricPoint, Role, SystemId};
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use nemo_console::services::metrics::{MAX_SERIES_PER_SYSTEM, series_name};
use serde_json::{Value, json};
use support::{CLOSED, fx};
use wiremock::matchers::{method, path};
use wiremock::{Mock, Request, Respond, ResponseTemplate};

const SYNTH_LABELS0: &str = r#"gpu="0",UUID="GPU-00000000-0000-0000-0000-000000000000",device="nvidia0",modelName="NVIDIA RTX PRO 6000 Blackwell Max-Q Workstation Edition",hostname="exporter""#;
const SYNTH_LABELS1: &str = r#"gpu="1",UUID="GPU-00000000-0000-0000-0000-000000000001",device="nvidia1",modelName="NVIDIA RTX PRO 6000 Blackwell Workstation Edition",hostname="exporter""#;

/// The captured scrape plus SYNTHETIC lines for the families it does not serve (counters.csv has them commented
/// out): the power limits (management on GPU 0 only; enforced on both), two violation counters in ns, a non-zero
/// memory temperature on GPU 1.
fn extended_scrape() -> String {
    let (l0, l1) = (SYNTH_LABELS0, SYNTH_LABELS1);
    let mut s = fx("dcgm", "metrics.txt").replace(
        "DCGM_FI_DEV_MEMORY_TEMP{gpu=\"1\"",
        "DCGM_FI_DEV_MEMORY_TEMP_CAPTURED{gpu=\"1\"",
    );
    s.push_str(&format!(
        "DCGM_FI_DEV_MEMORY_TEMP{{{l1}}} 61\n\
         DCGM_FI_DEV_POWER_MGMT_LIMIT{{{l0}}} 300\n\
         DCGM_FI_DEV_ENFORCED_POWER_LIMIT{{{l0}}} 290\n\
         DCGM_FI_DEV_ENFORCED_POWER_LIMIT{{{l1}}} 600\n\
         DCGM_FI_DEV_POWER_VIOLATION{{{l0}}} 2500000\n\
         DCGM_FI_DEV_THERMAL_VIOLATION{{{l1}}} 1000\n\
         DCGM_FI_DEV_SYNC_BOOST_VIOLATION{{{l1}}} 0\n"
    ));
    s
}

/// `(key, gpu, extra label value or "")` -> value.
fn by_gpu(points: &[MetricPoint]) -> BTreeMap<(String, String, String), f64> {
    points
        .iter()
        .filter(|p| p.labels.contains_key("gpu"))
        .map(|p| {
            let extra = p
                .labels
                .get("kind")
                .or_else(|| p.labels.get("reason"))
                .cloned()
                .unwrap_or_default();
            ((p.key.clone(), p.labels["gpu"].clone(), extra), p.value)
        })
        .collect()
}

fn k(key: &str, gpu: &str, extra: &str) -> (String, String, String) {
    (key.to_owned(), gpu.to_owned(), extra.to_owned())
}

// ── the new per-GPU keys ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_new_gpu_keys_from_the_captured_scrape() {
    let env = Env::new().await;
    let snap = env.adapter().metrics().await.unwrap();
    let by = by_gpu(&snap.points);
    let raw = parse_exposition(&fx("dcgm", "metrics.txt"));
    let raw_of = |fam: &str, gpu: &str| {
        raw[fam]
            .iter()
            .find(|(l, _)| l["gpu"] == gpu)
            .map(|(_, v)| *v)
            .unwrap()
    };
    assert_eq!(
        by[&k("dcgm.fb_reserved_b", "0", "")],
        raw_of("DCGM_FI_DEV_FB_RESERVED", "0") * 1_048_576.0
    );
    assert_eq!(
        by[&k("dcgm.mem_clock_mhz", "1", "")],
        raw_of("DCGM_FI_DEV_MEM_CLOCK", "1")
    );
    assert_eq!(by[&k("dcgm.enc_util", "0", "")], 0.0);
    assert_eq!(by[&k("dcgm.dec_util", "1", "")], 0.0);
    assert_eq!(
        by[&k("dcgm.energy_mj_total", "0", "")],
        raw_of("DCGM_FI_DEV_TOTAL_ENERGY_CONSUMPTION", "0")
    );
    assert_eq!(by[&k("dcgm.row_remap_failure", "1", "")], 0.0);
    for g in ["0", "1"] {
        for kind in ["correctable", "uncorrectable"] {
            assert_eq!(by[&k("dcgm.remapped_rows", g, kind)], 0.0);
        }
    }
    let units: HashMap<&str, &str> = snap
        .points
        .iter()
        .map(|p| (p.key.as_str(), p.unit.as_str()))
        .collect();
    assert_eq!(units["dcgm.fb_reserved_b"], "B");
    assert_eq!(units["dcgm.mem_clock_mhz"], "MHz");
    assert_eq!(units["dcgm.enc_util"], "ratio");
    assert_eq!(units["dcgm.energy_mj_total"], "mJ");
    assert_eq!(units["dcgm.remapped_rows"], "1");
    // what the exporter does not serve (or serves as 0 C memory temperature) is absent, never a fabricated 0
    for absent in [
        "dcgm.power_limit_w",
        "dcgm.violation_us_total",
        "dcgm.mem_temp_c",
    ] {
        assert!(!units.contains_key(absent), "{absent}");
    }
}

#[test]
fn test_power_limit_violations_and_memory_temperature_from_synthetic_families() {
    let pts = gpu_points_9_14(&parse_exposition(&extended_scrape()));
    let by = by_gpu(&pts);
    // the management limit first, else the enforced one, per GPU
    assert_eq!(by[&k("dcgm.power_limit_w", "0", "")], 300.0);
    assert_eq!(by[&k("dcgm.power_limit_w", "1", "")], 600.0);
    assert_eq!(
        pts.iter().filter(|p| p.key == "dcgm.power_limit_w").count(),
        2
    );
    // ns -> us
    assert_eq!(by[&k("dcgm.violation_us_total", "0", "power")], 2500.0);
    assert_eq!(by[&k("dcgm.violation_us_total", "1", "thermal")], 1.0);
    assert_eq!(by[&k("dcgm.violation_us_total", "1", "sync_boost")], 0.0);
    assert!(!by.contains_key(&k("dcgm.violation_us_total", "0", "board")));
    // a real memory temperature is reported, the captured 0 on GPU 0 is not
    assert_eq!(by[&k("dcgm.mem_temp_c", "1", "")], 61.0);
    assert!(!by.contains_key(&k("dcgm.mem_temp_c", "0", "")));
    let reasons: HashSet<&str> = pts
        .iter()
        .filter_map(|p| p.labels.get("reason").map(String::as_str))
        .collect();
    assert_eq!(reasons, HashSet::from(["power", "thermal", "sync_boost"]));
    for p in &pts {
        let keys: Vec<&str> = p.labels.keys().map(String::as_str).collect();
        assert_eq!(&keys[..2], ["gpu", "model"], "{}", p.key);
        assert!(keys.len() <= 3);
    }
    let blob = serde_json::to_string(&pts).unwrap();
    assert!(!blob.contains("GPU-") && !blob.contains("UUID") && !blob.contains("nvidia0"));
}

#[tokio::test]
async fn test_new_keys_come_after_the_old_ones_and_fit_the_series_cap() {
    let env = Env::new().await;
    env.metrics.set(200, &extended_scrape());
    let snap = env.adapter().metrics().await.unwrap();
    let new_keys: HashSet<String> = gpu_points_9_14(&parse_exposition(&extended_scrape()))
        .into_iter()
        .map(|p| p.key)
        .collect();
    let first_new = snap
        .points
        .iter()
        .position(|p| new_keys.contains(&p.key))
        .unwrap();
    assert!(
        snap.points[first_new..]
            .iter()
            .all(|p| new_keys.contains(&p.key)),
        "the 9.14 keys are appended last (the series cap keeps the older ones first)"
    );
    // every point is a key the series route accepts; this host (2 GPUs, 6 Prometheus hosts) uses well under the cap
    let names: HashSet<String> = snap.points.iter().map(series_name).collect();
    assert_eq!(names.len(), snap.points.len());
    assert!(
        names
            .iter()
            .all(|n| nemo_console::api::observe::KEY_RE.is_match(n)),
        "{names:?}"
    );
    assert!(names.len() <= MAX_SERIES_PER_SYSTEM / 2, "{}", names.len());
}

// ── the fleet route: adapter ────────────────────────────────────────────────────────────────────────

/// A fake Prometheus `query_range`: records every request's query parameters and answers the configured
/// (status, body).
#[derive(Clone)]
struct Range {
    seen: Arc<Mutex<Vec<HashMap<String, String>>>>,
    answer: Arc<Mutex<(u16, String)>>,
    delay_ms: Arc<Mutex<u64>>,
}

impl Range {
    fn new(body: &Value) -> Range {
        Range {
            seen: Arc::new(Mutex::new(Vec::new())),
            answer: Arc::new(Mutex::new((200, body.to_string()))),
            delay_ms: Arc::new(Mutex::new(0)),
        }
    }
    fn set(&self, status: u16, body: &str) {
        *self.answer.lock().unwrap() = (status, body.to_owned());
    }
    fn seen(&self) -> Vec<HashMap<String, String>> {
        self.seen.lock().unwrap().clone()
    }
}

impl Respond for Range {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let q: HashMap<String, String> = req
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        self.seen.lock().unwrap().push(q);
        let (s, b) = self.answer.lock().unwrap().clone();
        let delay = *self.delay_ms.lock().unwrap();
        ResponseTemplate::new(s)
            .set_body_string(b)
            .set_delay(std::time::Duration::from_millis(delay))
    }
}

/// One `query_range` series as Prometheus groups it (`max by (job, host, gpu, modelName)`).
fn series(job: &str, host: Option<&str>, gpu: &str, model: &str, values: &[(i64, &str)]) -> Value {
    let mut metric = json!({"job": job, "gpu": gpu, "modelName": model});
    if let Some(h) = host {
        metric["host"] = json!(h);
    }
    let values: Vec<Value> = values.iter().map(|(t, v)| json!([t, v])).collect();
    json!({"metric": metric, "values": values})
}

fn matrix(result: Vec<Value>) -> Value {
    json!({"status": "success", "data": {"resultType": "matrix", "result": result}})
}

/// Two hosts (this one: job `dcgm`, no `host` label; a Spark), a series without a GPU index (dropped) and a NaN.
fn two_hosts() -> Value {
    matrix(vec![
        series(
            "dcgm-sparks",
            Some("node1"),
            "0",
            "NVIDIA GB10",
            &[(1_800_000_000, "12"), (1_800_000_300, "NaN")],
        ),
        series(
            "dcgm",
            None,
            "1",
            "NVIDIA RTX PRO 6000 Blackwell Workstation Edition",
            &[(1_800_000_000, "50"), (1_800_000_300, "75")],
        ),
        series(
            "dcgm",
            None,
            "0",
            "NVIDIA RTX PRO 6000 Blackwell Max-Q Workstation Edition",
            &[(1_800_000_000, "45")],
        ),
        series(
            "dcgm",
            None,
            "GPU-00000000-0000-0000-0000-000000000000",
            "x",
            &[(1_800_000_000, "1")],
        ),
    ])
}

async fn fleet_env(body: &Value) -> (Env, Range) {
    let env = Env::new().await;
    let r = Range::new(body);
    Mock::given(method("GET"))
        .and(path("/api/v1/query_range"))
        .respond_with(r.clone())
        .mount(&env.server)
        .await;
    (env, r)
}

const JOBS: &str = "dcgm|dcgm-aitop100|dcgm-sparks";

#[tokio::test]
async fn test_fleet_two_hosts() {
    let (env, prom) = fleet_env(&two_hosts()).await;
    let a = env.adapter();
    let doc = a.fleet("util", "24h").await.unwrap();
    assert_eq!(doc["metric"], "util");
    assert_eq!(doc["unit"], "ratio");
    assert_eq!(doc["window"], "24h");
    assert_eq!(doc["step_s"], 300);
    let (from, to) = (doc["from"].as_i64().unwrap(), doc["to"].as_i64().unwrap());
    assert_eq!(to - from, 86_400);
    assert_eq!(to % 300, 0);
    assert_eq!(doc["truncated"], false);
    assert!(doc.get("problem").is_none());
    let rows = doc["rows"].as_array().unwrap();
    let ids: Vec<(String, String, String)> = rows
        .iter()
        .map(|r| {
            (
                r["host"].as_str().unwrap().to_owned(),
                r["job"].as_str().unwrap().to_owned(),
                r["gpu"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let id = |h: &str, j: &str, g: &str| (h.to_owned(), j.to_owned(), g.to_owned());
    assert_eq!(
        ids,
        [
            id("local", "dcgm", "0"),
            id("local", "dcgm", "1"),
            id("node1", "dcgm-sparks", "0")
        ]
    );
    assert!(
        rows[0]["model"]
            .as_str()
            .unwrap()
            .starts_with("RTX-PRO-6000-Blackwell-Max-Q")
    );
    assert_eq!(
        rows[1]["points"],
        json!([[1_800_000_000_i64, 0.5], [1_800_000_300_i64, 0.75]])
    );
    // a NaN sample is left out, never drawn as 0
    assert_eq!(rows[2]["points"], json!([[1_800_000_000_i64, 0.12]]));
    for r in rows {
        let keys: BTreeMap<&str, ()> = r
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (k.as_str(), ()))
            .collect();
        assert_eq!(
            keys.keys().copied().collect::<Vec<_>>(),
            ["gpu", "host", "job", "model", "points"]
        );
    }
    assert!(!doc.to_string().contains("GPU-"));
    // the one Prometheus request: the table's expression over the configured jobs, the window's step
    let seen = prom.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["query"], fleet_query("DCGM_FI_DEV_GPU_UTIL", JOBS));
    assert_eq!(seen[0]["step"], "300");
    assert_eq!(seen[0]["start"], from.to_string());
    assert_eq!(seen[0]["end"], to.to_string());
}

#[tokio::test]
async fn test_fleet_promql_is_table_text_and_configured_job_names_only() {
    let (env, prom) = fleet_env(&matrix(vec![])).await;
    let a = env.adapter();
    // a job name outside the job alphabet never reaches the expression
    a.configure(
        "ai-lab-observability-dcgm-exporter-1",
        "nvcr.io/nvidia/cloud-native/dcgm:4.6.1-1-ubuntu24.04",
        &[
            "dcgm".to_owned(),
            "dcgm-sparks".to_owned(),
            "x\"}) or vector(1) #".to_owned(),
        ],
        None,
    );
    for (metric, family, _, _) in FLEET_METRICS {
        for (window, _, step) in FLEET_WINDOWS {
            a.fleet(metric, window).await.unwrap();
            let q = prom.seen().last().cloned().unwrap();
            assert_eq!(
                q["query"],
                format!(
                    "max by (job, host, gpu, modelName) ({family}{{job=~\"dcgm|dcgm-sparks\"}})"
                )
            );
            assert_eq!(q["step"], step.to_string());
            assert_eq!(q.len(), 4, "{q:?}");
        }
    }
    assert_eq!(prom.seen().len(), FLEET_METRICS.len() * FLEET_WINDOWS.len());
    // unknown names are refused before anything is asked
    let n = prom.seen().len();
    for (m, w) in [
        ("util\"}", "24h"),
        ("gpu_util", "24h"),
        ("util", "2h"),
        ("util", ""),
    ] {
        assert!(
            matches!(a.fleet(m, w).await, Err(AdapterError::Invalid(_))),
            "{m} {w}"
        );
    }
    assert_eq!(prom.seen().len(), n);
}

#[tokio::test]
async fn test_fleet_prometheus_down_or_wrong_is_200_with_a_problem() {
    let (env, prom) = fleet_env(&two_hosts()).await;
    // nothing listens
    let a = env.adapter_with(Arc::clone(&env.runner), &env.url(), CLOSED);
    let doc = a.fleet("temp", "1h").await.unwrap();
    assert_eq!(doc["rows"], json!([]));
    assert_eq!(doc["problem"]["code"], "dcgm.prometheus.unreachable");
    assert!(
        doc["problem"]["message"]
            .as_str()
            .unwrap()
            .contains("ConnectError")
    );
    assert_eq!(doc["step_s"], 60);
    assert_eq!(doc["unit"], "C");
    // an error status, then a document that is not a success (the failure cache off, so each is asked)
    let a = env.adapter();
    a.set_fleet_fail_ttl(0.0);
    prom.set(500, "boom");
    let doc = a.fleet("temp", "1h").await.unwrap();
    assert_eq!(doc["problem"]["code"], "dcgm.prometheus.bad_answer");
    assert!(
        doc["problem"]["message"]
            .as_str()
            .unwrap()
            .contains("HTTP 500")
    );
    prom.set(200, r#"{"status":"error","error":"bad"}"#);
    assert_eq!(
        a.fleet("temp", "1h").await.unwrap()["problem"]["code"],
        "dcgm.prometheus.bad_answer"
    );
    prom.set(200, "not json");
    assert_eq!(
        a.fleet("temp", "1h").await.unwrap()["problem"]["code"],
        "dcgm.prometheus.bad_answer"
    );
    // Prometheus is back on the next call
    prom.set(200, &two_hosts().to_string());
    let doc = a.fleet("temp", "1h").await.unwrap();
    assert!(doc.get("problem").is_none());
    assert_eq!(doc["rows"].as_array().unwrap().len(), 3);
    assert_eq!(doc["rows"][1]["points"][1][1], 75.0);
    assert_eq!(prom.seen().len(), 4);
}

#[tokio::test]
async fn test_fleet_without_configured_jobs_asks_nothing() {
    let (env, prom) = fleet_env(&two_hosts()).await;
    let a = env.adapter();
    a.configure(
        "c",
        "nvcr.io/nvidia/cloud-native/dcgm:4.6.1-1-ubuntu24.04",
        &[],
        None,
    );
    let doc = a.fleet("power", "7d").await.unwrap();
    assert_eq!(doc["rows"], json!([]));
    assert_eq!(doc["problem"]["code"], "dcgm.fleet.no_jobs");
    assert_eq!(doc["step_s"], 1800);
    assert!(prom.seen().is_empty());
}

#[tokio::test]
async fn test_fleet_keeps_at_most_64_rows_ordered_by_host_then_gpu() {
    let mut result = Vec::new();
    // 35 hosts x 2 GPUs = 70 rows, given in reverse order, GPU 10 after GPU 9 numerically on one host
    for h in (0..35).rev() {
        for g in ["1", "0"] {
            result.push(series(
                "dcgm-sparks",
                Some(&format!("node{h:02}")),
                g,
                "NVIDIA GB10",
                &[(1_800_000_000, "1")],
            ));
        }
    }
    result.push(series("dcgm", None, "10", "m", &[(1_800_000_000, "1")]));
    result.push(series("dcgm", None, "9", "m", &[(1_800_000_000, "1")]));
    let (env, _prom) = fleet_env(&matrix(result)).await;
    let doc = env.adapter().fleet("fb_used", "1h").await.unwrap();
    let rows = doc["rows"].as_array().unwrap();
    assert_eq!(rows.len(), FLEET_MAX_ROWS);
    assert_eq!(doc["truncated"], true);
    let order: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{}/{}",
                r["host"].as_str().unwrap(),
                r["gpu"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(&order[..4], ["local/9", "local/10", "node00/0", "node00/1"]);
    assert_eq!(order.last().unwrap(), "node30/1");
    // MiB -> B, like dcgm.fb_used_b
    assert_eq!(rows[0]["points"][0][1], 1_048_576.0);
    assert_eq!(doc["unit"], "B");
}

#[tokio::test]
async fn test_fleet_answers_are_cached_per_metric_and_window() {
    let (env, prom) = fleet_env(&two_hosts()).await;
    let a = env.adapter();
    let first = a.fleet("util", "24h").await.unwrap();
    prom.set(200, &matrix(vec![]).to_string());
    assert_eq!(a.fleet("util", "24h").await.unwrap(), first);
    assert_eq!(prom.seen().len(), 1);
    // another window or metric is its own entry
    assert_eq!(a.fleet("util", "1h").await.unwrap()["rows"], json!([]));
    assert_eq!(a.fleet("temp", "24h").await.unwrap()["rows"], json!([]));
    assert_eq!(prom.seen().len(), 3);
    // an expired entry is asked again
    a.set_fleet_cache_ttl(0.0);
    assert_eq!(a.fleet("util", "24h").await.unwrap()["rows"], json!([]));
    assert_eq!(prom.seen().len(), 4);
}

#[tokio::test]
async fn test_fleet_failures_are_single_flight_and_briefly_cached() {
    let (env, prom) = fleet_env(&two_hosts()).await;
    let a = env.adapter();
    prom.set(500, "down");
    *prom.delay_ms.lock().unwrap() = 400;
    // five concurrent requests while Prometheus is slow and failing: one request reaches it
    let t = std::time::Instant::now();
    let (r1, r2, r3, r4, r5) = tokio::join!(
        a.fleet("util", "24h"),
        a.fleet("temp", "24h"),
        a.fleet("power", "1h"),
        a.fleet("fb_used", "7d"),
        a.fleet("util", "24h"),
    );
    let elapsed = t.elapsed().as_secs_f64();
    for r in [r1, r2, r3, r4, r5] {
        assert_eq!(r.unwrap()["problem"]["code"], "dcgm.prometheus.bad_answer");
    }
    assert_eq!(prom.seen().len(), 1);
    assert!(elapsed < 1.5, "{elapsed}");
    // within the failure cache, nothing is asked
    assert_eq!(
        a.fleet("temp", "1h").await.unwrap()["problem"]["code"],
        "dcgm.prometheus.bad_answer"
    );
    assert_eq!(prom.seen().len(), 1);
    // once it expires Prometheus is asked again, and a success clears the failure for every key
    *prom.delay_ms.lock().unwrap() = 0;
    prom.set(200, &two_hosts().to_string());
    a.set_fleet_fail_ttl(0.0);
    assert!(
        a.fleet("temp", "1h")
            .await
            .unwrap()
            .get("problem")
            .is_none()
    );
    a.set_fleet_fail_ttl(60.0);
    assert!(
        a.fleet("power", "1h")
            .await
            .unwrap()
            .get("problem")
            .is_none()
    );
    assert_eq!(prom.seen().len(), 3);
}

#[tokio::test]
async fn test_fleet_oversized_answer_is_refused() {
    let (env, prom) = fleet_env(&two_hosts()).await;
    let a = env.adapter();
    prom.set(
        200,
        &"x".repeat(nemo_console::adapters::dcgm::FLEET_MAX_BYTES + 1),
    );
    let doc = a.fleet("util", "1h").await.unwrap();
    assert_eq!(doc["problem"]["code"], "dcgm.prometheus.bad_answer");
    assert!(
        doc["problem"]["message"]
            .as_str()
            .unwrap()
            .contains("too large")
    );
    assert_eq!(doc["rows"], json!([]));
}

#[tokio::test]
async fn test_fleet_many_series_give_64_rows_quickly() {
    // 1,500 series of a week at 30-minute steps (337 points each): only the first 1,024 are looked at and only the
    // 64 kept rows get points built
    let values: Vec<(i64, String)> = (0..337)
        .map(|i| (1_800_000_000 + i * 1800, format!("{i}")))
        .collect();
    let pairs: Vec<(i64, &str)> = values.iter().map(|(t, v)| (*t, v.as_str())).collect();
    let mut result = Vec::new();
    for h in 0..750 {
        for g in ["0", "1"] {
            result.push(series(
                "dcgm-sparks",
                Some(&format!("node{h:04}")),
                g,
                "NVIDIA GB10",
                &pairs,
            ));
        }
    }
    let (env, _prom) = fleet_env(&matrix(result)).await;
    let t = std::time::Instant::now();
    let doc = env.adapter().fleet("power", "7d").await.unwrap();
    assert!(t.elapsed().as_secs_f64() < 5.0, "{:?}", t.elapsed());
    let rows = doc["rows"].as_array().unwrap();
    assert_eq!(rows.len(), FLEET_MAX_ROWS);
    assert_eq!(doc["truncated"], true);
    assert_eq!(rows[0]["host"], "node0000");
    assert_eq!(rows[63]["host"], "node0031");
    assert_eq!(rows[0]["points"].as_array().unwrap().len(), 337);
}

// ── the fleet route: HTTP ───────────────────────────────────────────────────────────────────────────

/// The app with this DCGM adapter (fakes for every other system). Loopback requests are admin.
fn app(env: &Env, dcgm: Arc<DcgmAdapter>) -> Arc<AppState> {
    let tmp = env.tmp.path();
    let mut urls = serde_json::Map::new();
    for k in [
        "switchyard",
        "relay",
        "dynamo",
        "dynamo_worker_metrics",
        "litellm",
        "dcgm",
        "prometheus",
        "guardrails",
    ] {
        urls.insert(k.to_owned(), json!(CLOSED));
    }
    let settings = settings_for_test(json!({
        "state_dir": tmp.join("app-state").to_string_lossy(),
        "frontend_dir": tmp.join("front").to_string_lossy(),
        "repo_root": tmp.join("repo").to_string_lossy(),
        "background_tasks": false, "trust_loopback": true, "urls": urls,
    }));
    let mut adapters: HashMap<SystemId, Arc<dyn SystemAdapter>> = HashMap::new();
    for sid in SystemId::ALL {
        if sid == SystemId::Dcgm {
            adapters.insert(sid, Arc::clone(&dcgm) as Arc<dyn SystemAdapter>);
        } else {
            adapters.insert(
                sid,
                common::FakeAdapter::new(sid, None) as Arc<dyn SystemAdapter>,
            );
        }
    }
    build_container(
        settings,
        Overrides {
            adapters,
            runner: Some(Arc::clone(&env.runner) as Arc<dyn CommandRunner>),
            http: Some(Http::new().unwrap()),
            registry: None,
            environ: Some(Environ::fixed(HashMap::new())),
        },
    )
    .unwrap()
}

async fn get(st: &Arc<AppState>, path: &str, key: Option<&str>) -> common::TestResp {
    let mut r = common::TestReq::new(Arc::clone(st), "GET", path).client("192.0.2.10");
    if let Some(k) = key {
        r = r.token(k);
    }
    r.send().await
}

#[tokio::test]
async fn test_fleet_route_roles_404_and_422() {
    let (env, prom) = fleet_env(&two_hosts()).await;
    let st = app(&env, env.adapter());
    let (_, viewer) = st.auth.mint(Role::Viewer, "").unwrap();
    let url = "/api/v1/systems/dcgm/fleet";
    // a viewer reads it; the default window is 24h
    let r = get(&st, &format!("{url}?metric=util"), Some(&viewer)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let doc = r.json();
    assert_eq!(doc["window"], "24h");
    assert_eq!(doc["rows"].as_array().unwrap().len(), 3);
    // no key from another address: 401
    assert_eq!(
        get(&st, &format!("{url}?metric=util"), None).await.status,
        401
    );
    // every other system: 404, before the query is looked at
    for sid in SystemId::ALL {
        if sid == SystemId::Dcgm {
            continue;
        }
        for q in ["metric=util", "metric=bogus"] {
            let r = get(
                &st,
                &format!("/api/v1/systems/{}/fleet?{q}", sid.as_str()),
                Some(&viewer),
            )
            .await;
            assert_eq!(r.status, 404, "{}", sid.as_str());
            assert_eq!(r.code(), "not_found");
        }
    }
    // 422 for an unknown metric or window; nothing reaches Prometheus
    let before = prom.seen().len();
    for q in [
        "",
        "metric=",
        "metric=bogus",
        "metric=util%22%7D%20or%20vector(1)",
        "metric=UTIL",
        "metric=util&window=2h",
        "metric=util&window=",
        "metric=util&window=15m",
    ] {
        let r = get(&st, &format!("{url}?{q}"), Some(&viewer)).await;
        assert_eq!(r.status, 422, "{q}: {}", r.text());
        assert_eq!(r.code(), "invalid_request");
    }
    let r = get(&st, &format!("{url}?metric=x"), Some(&viewer)).await;
    assert!(
        r.text().contains("['fb_used', 'power', 'temp', 'util']"),
        "{}",
        r.text()
    );
    let r = get(&st, &format!("{url}?metric=util&window=x"), Some(&viewer)).await;
    assert!(r.text().contains("['1h', '24h', '7d']"), "{}", r.text());
    assert_eq!(prom.seen().len(), before);
    // Prometheus down is still a 200
    let st = app(
        &env,
        env.adapter_with(Arc::clone(&env.runner), &env.url(), CLOSED),
    );
    let r = get(&st, &format!("{url}?metric=power&window=7d"), Some(&viewer)).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["problem"]["code"], "dcgm.prometheus.unreachable");
    assert_eq!(r.json()["rows"], json!([]));
}

#[test]
fn test_the_openapi_entry_of_the_fleet_route_matches_the_code() {
    let doc: Value = serde_json::from_str(nemo_console::api::admin::OPENAPI_JSON).unwrap();
    let params = doc["paths"]["/api/v1/systems/{system_id}/fleet"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .clone();
    let param = |name: &str| {
        params
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("no {name} parameter"))
            .clone()
    };
    let metrics: Vec<&str> = FLEET_METRICS.iter().map(|m| m.0).collect();
    let windows: Vec<&str> = FLEET_WINDOWS.iter().map(|w| w.0).collect();
    assert_eq!(param("metric")["schema"]["enum"], json!(metrics));
    assert_eq!(param("metric")["in"], "query");
    assert_eq!(param("window")["schema"]["enum"], json!(windows));
    assert_eq!(
        param("window")["schema"]["default"],
        nemo_console::adapters::dcgm::FLEET_DEFAULT_WINDOW
    );
    assert_eq!(param("system_id")["in"], "path");
}
