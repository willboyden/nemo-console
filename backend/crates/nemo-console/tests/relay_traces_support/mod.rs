//! Helpers of the Relay trace explorer tests (addendum 9.12): a temporary capture layout, a Relay adapter whose
//! trace index is refreshed by hand (no background task, so every test is deterministic), and an app state for the
//! HTTP-level tests. Hermetic: fake runner, closed upstream ports, files under a temp dir only.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::cast_possible_truncation,
    clippy::needless_pass_by_value
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, AdapterError, CommandRunner, SystemAdapter};
use nemo_console::adapters::relay::RelayAdapter;
use nemo_console::adapters::relay_traces::scan::Limits;
use nemo_console::adapters::relay_traces::{Request, TraceConfig};
use nemo_console::config::settings_for_test;
use nemo_console::container::{AppState, Overrides, build_container};
use nemo_console::domain::models::SystemId;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Value, json};

use crate::common::{FakeAdapter, FakeRunner, TestReq, TestResp};

pub const API: &str = "/api/v1/systems/relay";

/// `tests/fixtures/relay_traces`: synthetic captures with the structure of real NeMo Relay 0.9 files.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/relay_traces")
}

pub fn fixture_bytes(rel: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(rel)).unwrap()
}

/// The three ATOF fixture files, oldest first, as lines (the `.gz` one decompressed by the code under test's own
/// reader, which the gzip unit tests compare with an independent implementation).
pub fn fixture_lines() -> Vec<String> {
    use std::io::Read as _;
    let mut out = Vec::new();
    let mut gz = String::new();
    nemo_console::services::gzip::GzReader::new(&fixture_bytes("atof/relay-events.jsonl.2.gz")[..])
        .read_to_string(&mut gz)
        .unwrap();
    for text in [
        gz,
        String::from_utf8(fixture_bytes("atof/relay-events.jsonl.1")).unwrap(),
        String::from_utf8(fixture_bytes("atof/relay-events.jsonl")).unwrap(),
    ] {
        out.extend(text.lines().map(str::to_owned));
    }
    out
}

/// A gzip file holding `data` in stored blocks (valid gzip, no compression): enough to make rotations in a test.
pub fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(60_000).collect()
    };
    for (i, ch) in chunks.iter().enumerate() {
        out.push(u8::from(i + 1 == chunks.len()));
        out.extend_from_slice(&(ch.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(ch.len() as u16)).to_le_bytes());
        out.extend_from_slice(ch);
    }
    out.extend_from_slice(&nemo_console::services::gzip::crc_update(0, data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// Small limits make the bounds testable; everything else as in production.
pub fn config() -> TraceConfig {
    TraceConfig::default()
}

pub fn limits() -> Limits {
    config().limits
}

/// One temporary capture layout and the adapter that reads it.
pub struct Lab {
    pub tmp: tempfile::TempDir,
    pub atof: PathBuf,
    pub atif: PathBuf,
    pub hermes: PathBuf,
    pub cfg: PathBuf,
    pub runner: Arc<FakeRunner>,
    pub a: Arc<RelayAdapter>,
}

impl Lab {
    /// Empty directories.
    pub fn empty(cfg: TraceConfig) -> Lab {
        let tmp = tempfile::tempdir().unwrap();
        let atof = tmp.path().join("atof");
        let atif = tmp.path().join("atif");
        let hermes = tmp.path().join("hermes");
        let cfgd = tmp.path().join("cfg");
        for d in [&atof, &atif, &hermes, &cfgd] {
            std::fs::create_dir_all(d).unwrap();
        }
        let runner = Arc::new(FakeRunner::default());
        let mut paths: IndexMap<String, PathBuf> = IndexMap::new();
        paths.insert("relay_atof_dir".into(), atof.clone());
        paths.insert("relay_atif_dir".into(), atif.clone());
        paths.insert("relay_hermes_dir".into(), hermes.clone());
        paths.insert("relay_plugins".into(), cfgd.join("plugins.toml"));
        paths.insert("relay_config".into(), cfgd.join("config.toml"));
        paths.insert("relay_routes".into(), cfgd.join("routes.toml"));
        let ctx = AdapterContext {
            runner: Arc::clone(&runner) as Arc<dyn CommandRunner>,
            http: Http::new().unwrap(),
            // nothing listens on port 9: no test can reach a real service
            urls: [
                ("relay", "http://127.0.0.1:9"),
                ("litellm", "http://127.0.0.1:9"),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
            paths,
            secret_env: IndexMap::new(),
            state_dir: tmp.path().join("state"),
            environ: Environ::from_pairs(&[]),
        };
        let a = Arc::new(RelayAdapter::with_traces(ctx, cfg));
        Lab {
            tmp,
            atof,
            atif,
            hermes,
            cfg: cfgd,
            runner,
            a,
        }
    }

    /// The committed fixtures (three ATOF files, three trajectories), copied.
    pub fn with_fixtures(cfg: TraceConfig) -> Lab {
        let lab = Lab::empty(cfg);
        for name in [
            "relay-events.jsonl",
            "relay-events.jsonl.1",
            "relay-events.jsonl.2.gz",
        ] {
            std::fs::copy(fixtures().join("atof").join(name), lab.atof.join(name)).unwrap();
        }
        for e in std::fs::read_dir(fixtures().join("atif")).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), lab.atif.join(e.file_name())).unwrap();
        }
        lab
    }

    /// Fixtures, indexed.
    pub fn indexed() -> Lab {
        let lab = Lab::with_fixtures(config());
        lab.refresh();
        lab
    }

    /// One pass of the index, on this thread.
    pub fn refresh(&self) {
        self.a.traces.refresh_blocking();
    }

    pub fn index_state(&self) -> Value {
        self.a.traces.snapshot().index_state()
    }

    pub async fn ask(&self, r: Request) -> Result<Value, AdapterError> {
        self.a.trace_explorer(r).await
    }

    pub fn write(&self, name: &str, lines: &[String]) {
        let mut text = lines.join("\n");
        if !lines.is_empty() {
            text.push('\n');
        }
        std::fs::write(self.atof.join(name), text).unwrap();
    }

    pub fn append(&self, name: &str, text: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.atof.join(name))
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }
}

/// The app with the lab's Relay adapter (fakes for every other system). Loopback requests are admin.
pub fn app(lab: &Lab) -> Arc<AppState> {
    let tmp = lab.tmp.path();
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
        urls.insert(k.to_owned(), json!("http://127.0.0.1:9"));
    }
    let settings = settings_for_test(json!({
        "state_dir": tmp.join("app-state").to_string_lossy(),
        "frontend_dir": tmp.join("front").to_string_lossy(),
        "repo_root": tmp.join("repo").to_string_lossy(),
        "background_tasks": false, "trust_loopback": true, "urls": urls,
    }));
    let mut adapters: HashMap<SystemId, Arc<dyn SystemAdapter>> = HashMap::new();
    for sid in SystemId::ALL {
        if sid == SystemId::Relay {
            adapters.insert(sid, Arc::clone(&lab.a) as Arc<dyn SystemAdapter>);
        } else {
            adapters.insert(sid, FakeAdapter::new(sid, None) as Arc<dyn SystemAdapter>);
        }
    }
    build_container(
        settings,
        Overrides {
            adapters,
            runner: Some(Arc::clone(&lab.runner) as Arc<dyn CommandRunner>),
            http: Some(Http::new().unwrap()),
            registry: None,
            environ: Some(Environ::fixed(HashMap::new())),
        },
    )
    .unwrap()
}

/// GET as the loopback admin.
pub async fn get(state: &Arc<AppState>, path: &str) -> TestResp {
    TestReq::new(Arc::clone(state), "GET", path).send().await
}

/// GET `/api/v1/systems/relay<sub>` as the loopback admin, expecting 200.
pub async fn ok(state: &Arc<AppState>, sub: &str) -> Value {
    let r = get(state, &format!("{API}{sub}")).await;
    assert_eq!(r.status, 200, "{sub}: {}", r.text());
    r.json()
}

// ── building events for the tests that need their own ───────────────────────────────────────────

/// A UUID-shaped id from a number.
pub fn uid(n: u64) -> String {
    format!("0b0b0b0b-0000-7000-8000-{n:012x}")
}

/// An RFC 3339 timestamp `secs` seconds (with a fraction) after 2026-11-10T00:00:00Z.
pub fn at(secs: f64) -> String {
    let base = 1_794_268_800.0 + secs;
    let whole = base.floor() as i64;
    let micros = ((base - base.floor()) * 1e6).round() as i64;
    let (d, s) = (whole.div_euclid(86_400), whole.rem_euclid(86_400));
    // days since 1970-01-01 -> civil date
    let z = d + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{micros:06}+00:00",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// The epoch seconds of [`at`].
pub fn at_secs(secs: f64) -> f64 {
    1_794_268_800.0 + secs
}

/// One scope event line.
pub fn scope(
    phase: &str,
    category: &str,
    name: &str,
    id: u64,
    parent: Option<u64>,
    secs: f64,
    extra: Value,
) -> String {
    let mut v = json!({
        "atof_version": "0.1", "attributes": [], "kind": "scope", "scope_category": phase, "category": category,
        "name": name, "uuid": uid(id), "parent_uuid": parent.map(uid), "timestamp": at(secs),
        "category_profile": null, "data": null, "data_schema": null, "metadata": null,
    });
    if let (Value::Object(base), Value::Object(more)) = (&mut v, extra) {
        for (k, x) in more {
            base.insert(k, x);
        }
    }
    v.to_string()
}

/// One mark event line.
pub fn mark(name: &str, id: u64, parent: Option<u64>, secs: f64, data: Value) -> String {
    json!({
        "atof_version": "0.1", "kind": "mark", "category": null, "category_profile": null, "name": name,
        "uuid": uid(id), "parent_uuid": parent.map(uid), "timestamp": at(secs), "data": data, "data_schema": null,
        "metadata": null,
    })
    .to_string()
}

/// The start and end lines of one LLM span (`openai.chat_completions`): the request body and headers on the start,
/// the answer on the end.
pub fn llm_pair(
    id: u64,
    parent: Option<u64>,
    t0: f64,
    t1: f64,
    headers: Value,
    request: Value,
    response: Value,
) -> [String; 2] {
    [
        scope(
            "start",
            "llm",
            "openai.chat_completions",
            id,
            parent,
            t0,
            json!({"data": {"content": request, "headers": headers}}),
        ),
        scope(
            "end",
            "llm",
            "openai.chat_completions",
            id,
            parent,
            t1,
            json!({"data": response}),
        ),
    ]
}

/// An ATIF trajectory document with id `id` (as `trajectory_id` and `session_id`) and two steps: a user message
/// `text`, then an agent step with one tool call and its observation.
pub fn traj_doc(id: &str, text: &str) -> Value {
    json!({
        "schema_version": "ATIF-v1.7",
        "session_id": id,
        "trajectory_id": id,
        "agent": {"name": "example-agent", "version": "1.0.0", "model_name": "local-small"},
        "steps": [
            {"step_id": 1, "source": "user", "timestamp": at(0.0), "message": text},
            {"step_id": 2, "source": "agent", "timestamp": at(1.0), "model_name": "local-small", "message": "ok",
             "tool_calls": [{"tool_call_id": "call_1", "function_name": "read_file", "arguments": {"path": "a.txt"}}],
             "observation": {"results": [{"source_call_id": "call_1", "content": "file body"}]},
             "metrics": {"prompt_tokens": 10, "completion_tokens": 2}},
        ],
        "final_metrics": {"total_prompt_tokens": 10, "total_completion_tokens": 2},
    })
}
