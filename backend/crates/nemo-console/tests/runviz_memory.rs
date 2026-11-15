//! Addendum 9.15, D-40: the per-run routes keep their memory bounded however a run's files are shaped (audit F1:
//! one 16 MiB line `[0,0,...]` used to parse into a tree of about 80 times its size). Each scenario runs in a fresh
//! child process (this test binary re-executed with `--exact memory_worker`), so its peak resident set (`VmHWM`,
//! reset through `/proc/self/clear_refs`) belongs to that scenario alone. Hermetic: synthetic files in a temp dir.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)] // the peaks are printed for the record

mod common;

use std::path::Path;
use std::sync::Arc;

use indexmap::IndexMap;
use nemo_console::adapters::aiperf::AiPerfAdapter;
use nemo_console::adapters::base::{AdapterContext, SystemAdapter};
use nemo_console::adapters::datadesigner::DataDesignerAdapter;
use nemo_console::adapters::skillevaluator::SkillEvaluatorAdapter;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::json;

const AP_RUN: &str = "20261101T120000-000-smoke";
const SE_RUN: &str = "20261101T120000-000-demo-skill";
const DD_RUN: &str = "20261101T120000-000-demo.py";
const MIB: usize = 1024 * 1024;
/// The bound every scenario must stay under (peak resident set above the baseline, per request).
const LIMIT_MIB: f64 = 200.0;

const SCENARIOS: [&str; 13] = [
    "ap_requests_one_line_16mib",
    "ap_requests_lines_255kib",
    "ap_requests_lines_at_node_cap",
    "ap_requests_4_concurrent",
    "dd_profile_one_line_16mib",
    "dd_profile_lines_1mib",
    "dd_profile_lines_at_node_cap",
    "ap_summary_8mib_array",
    "ap_summary_1mib_array",
    "ap_summary_at_node_cap",
    "se_tier1_8mib_array",
    "se_tier1_1mib_array",
    "se_tier1_at_node_cap",
];
/// About 95,000 values: just under `runviz::JSON_MAX_NODES`, so it is parsed (the worst case that is).
const AT_CAP: usize = 190 * 1024;

fn status_kib(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = s.lines().find(|l| l.starts_with(field)).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// `[0,0,...,0]` of about `bytes` bytes (inside `prefix` .. `suffix`), streamed to `w` without a big allocation.
fn zeros(w: &mut impl std::io::Write, prefix: &str, bytes: usize, suffix: &str) {
    w.write_all(prefix.as_bytes()).unwrap();
    w.write_all(b"[").unwrap();
    let chunk = "0,".repeat(4096);
    let n = bytes.saturating_sub(prefix.len() + suffix.len() + 3) / 2;
    for _ in 0..n / 4096 {
        w.write_all(chunk.as_bytes()).unwrap();
    }
    for _ in 0..n % 4096 {
        w.write_all(b"0,").unwrap();
    }
    w.write_all(b"0]").unwrap();
    w.write_all(suffix.as_bytes()).unwrap();
}

fn file(p: &Path) -> std::io::BufWriter<std::fs::File> {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::io::BufWriter::new(std::fs::File::create(p).unwrap())
}

fn meta(dir: &Path, run: &str, extra: &serde_json::Value) {
    let mut m = json!({"run_id": run, "verdict": "succeeded"});
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("run.json"), m.to_string()).unwrap();
}

/// Write the scenario's files under `state`.
fn seed(scenario: &str, state: &Path) {
    let ap = state.join("aiperf/runs").join(AP_RUN);
    let se = state.join("skillevaluator/runs").join(SE_RUN);
    let dd = state.join("datadesigner/runs").join(DD_RUN);
    meta(&ap, AP_RUN, &json!({"preset": "smoke"}));
    meta(
        &se,
        SE_RUN,
        &json!({"skill": "demo-skill", "tiers": "tier1"}),
    );
    meta(&dd, DD_RUN, &json!({"script": "demo.py"}));
    let records = ap.join("artifacts/profile_export.jsonl");
    match scenario {
        "ap_requests_one_line_16mib" => {
            let mut w = file(&records);
            zeros(&mut w, "", 16 * MIB - 1, "\n");
        }
        "ap_requests_lines_255kib" | "ap_requests_4_concurrent" => {
            let mut w = file(&records);
            for _ in 0..(16 * MIB / (255 * 1024)) {
                zeros(&mut w, "{\"metrics\":", 255 * 1024, "}\n");
            }
        }
        "ap_requests_lines_at_node_cap" => {
            let mut w = file(&records);
            for _ in 0..(16 * MIB / AT_CAP) {
                zeros(&mut w, "{\"metrics\":", AT_CAP, "}\n");
            }
        }
        "dd_profile_lines_at_node_cap" => {
            let mut w = file(&dd.join("records.jsonl"));
            for _ in 0..300 {
                zeros(&mut w, "{\"a\":", AT_CAP, "}\n");
            }
        }
        "ap_summary_at_node_cap" => {
            let mut w = file(&ap.join("artifacts/profile_export_aiperf.json"));
            zeros(&mut w, "{\"request_latency\":", AT_CAP, "}");
        }
        "se_tier1_at_node_cap" => {
            let mut w = file(&se.join("skillevaluator-tier1.json"));
            zeros(&mut w, "{\"results\":", AT_CAP, "}");
        }
        "dd_profile_one_line_16mib" => {
            let mut w = file(&dd.join("records.jsonl"));
            zeros(&mut w, "{\"a\":", 16 * MIB - 1, "}\n");
        }
        "dd_profile_lines_1mib" => {
            let mut w = file(&dd.join("records.jsonl"));
            for _ in 0..64 {
                zeros(&mut w, "{\"a\":", MIB - 64, "}\n");
            }
        }
        "ap_summary_8mib_array" => {
            let mut w = file(&ap.join("artifacts/profile_export_aiperf.json"));
            zeros(&mut w, "{\"request_latency\":", 8 * MIB - 64, "}");
        }
        "ap_summary_1mib_array" => {
            let mut w = file(&ap.join("artifacts/profile_export_aiperf.json"));
            zeros(&mut w, "{\"request_latency\":", MIB - 64, "}");
        }
        "se_tier1_8mib_array" => {
            let mut w = file(&se.join("skillevaluator-tier1.json"));
            zeros(&mut w, "{\"results\":", 8 * MIB - 64, "}");
        }
        "se_tier1_1mib_array" => {
            let mut w = file(&se.join("skillevaluator-tier1.json"));
            zeros(&mut w, "{\"results\":", MIB - 64, "}");
        }
        other => panic!("unknown scenario {other}"),
    }
}

fn ctx(state: &Path) -> AdapterContext {
    AdapterContext {
        runner: Arc::new(common::FakeRunner::default()),
        http: Http::new().unwrap(),
        urls: IndexMap::new(),
        paths: IndexMap::new(),
        secret_env: IndexMap::new(),
        state_dir: state.to_path_buf(),
        environ: Environ::from_pairs(&[]),
    }
}

async fn call(scenario: &str, state: &Path) {
    let c = ctx(state);
    let ap = AiPerfAdapter::new(c.clone());
    let r = match scenario {
        s if s.starts_with("ap_requests_4") => {
            let ap = Arc::new(ap);
            let mut tasks = Vec::new();
            for _ in 0..4 {
                let a = Arc::clone(&ap);
                tasks.push(tokio::spawn(async move { a.run_requests(AP_RUN).await }));
            }
            let mut last = None;
            for t in tasks {
                last = Some(t.await.unwrap());
            }
            last.unwrap()
        }
        s if s.starts_with("ap_requests") => ap.run_requests(AP_RUN).await,
        s if s.starts_with("ap_summary") => ap.run_summary(AP_RUN).await,
        s if s.starts_with("se_tier1") => SkillEvaluatorAdapter::new(c).run_summary(SE_RUN).await,
        s if s.starts_with("dd_profile") => {
            DataDesignerAdapter::new(c).run_profile(DD_RUN, true).await
        }
        other => panic!("unknown scenario {other}"),
    };
    let v = r.unwrap();
    println!(
        "ANSWER {scenario} {}",
        nemo_console::services::pyfmt::char_prefix(&v.to_string(), 300)
    );
}

/// The child: runs one scenario and prints its peak. Does nothing unless the parent set `RUNVIZ_MEM_SCENARIO`.
#[test]
fn memory_worker() {
    let Ok(scenario) = std::env::var("RUNVIZ_MEM_SCENARIO") else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    seed(&scenario, &state);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // reset the peak to what is resident now, then measure one request
    std::fs::write("/proc/self/clear_refs", "5").unwrap();
    let base = status_kib("VmRSS:");
    rt.block_on(call(&scenario, &state));
    let peak = status_kib("VmHWM:");
    #[allow(clippy::cast_precision_loss)]
    let mib = peak.saturating_sub(base) as f64 / 1024.0;
    println!("PEAK {scenario} {mib:.1} MiB");
    assert!(
        mib < LIMIT_MIB,
        "{scenario}: peak {mib:.1} MiB over {LIMIT_MIB} MiB"
    );
}

#[test]
fn per_request_memory_stays_bounded() {
    if std::env::var("RUNVIZ_MEM_SCENARIO").is_ok() {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let mut failed = Vec::new();
    for s in SCENARIOS {
        let out = std::process::Command::new(&exe)
            .args([
                "--exact",
                "memory_worker",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RUNVIZ_MEM_SCENARIO", s)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let peak = text
            .lines()
            .find(|l| l.starts_with("PEAK "))
            .unwrap_or("PEAK (none)")
            .to_owned();
        let answer = text
            .lines()
            .find_map(|l| l.find("ANSWER ").map(|i| l[i..].to_owned()))
            .unwrap_or_default();
        eprintln!("{peak}\n  {answer}");
        if !out.status.success() {
            failed.push(format!(
                "{s}: {}\n{}",
                peak,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
