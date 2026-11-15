//! Addendum 9.16 (charts, wave 4), the backend part: `GET /systems/guardrails/rails`, the persisted OpenShell sandbox
//! events, `GET /systems/openshell/decisions`, the `/audit` filters and `GET /audit/summary` (Rust-only routes:
//! PORTING.md D-42; the audit filters: D-43). Hermetic: SYNTHETIC config trees, log lines and audit rows written into
//! a temp dir (generic names only; the one captured fixture read is the repo's `openshell/logs.txt`), fake runners,
//! the real Guardrails and OpenShell adapters and fakes for the rest. Nothing runs the `openshell` CLI.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default,
    clippy::format_in_format_args
)]

mod common;
#[path = "fixtures/openshell/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nemo_console::adapters::base::{CommandRunner, SystemAdapter};
use nemo_console::adapters::guardrails::GuardrailsAdapter;
use nemo_console::adapters::guardrails_rails::{RAILS_MAX_CONFIGS, RAILS_MAX_FLOWS};
use nemo_console::adapters::openshell::{EVENT_KIND, OpenShellAdapter, RING};
use nemo_console::adapters::openshell_viz::{
    self as viz, DECISIONS_DEFAULT_LIMIT, DECISIONS_MAX_ROWS, EVENTS_FILE, EVENTS_KEEP,
    EVENTS_REWRITE_AT,
};
use nemo_console::config::settings_for_test;
use nemo_console::container::{AppState, Overrides, build_container};
use nemo_console::domain::models::{Role, SystemEvent, SystemId};
use nemo_console::services::audit::{SUMMARY_DEFAULT_WINDOW, SUMMARY_WINDOWS};
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Map, Value, json};

/// Planted in every text the routes must never copy: prompts, `.co` content, parameters, OCSF message text, command
/// lines, paths, stored audit paths and params, persisted summaries.
const MARK: &str = "ZZMARKERZZ";

const SANDBOX: &str = "sb-one";

struct Lab {
    tmp: tempfile::TempDir,
    st: Arc<AppState>,
    viewer: String,
    admin: String,
    os: Arc<OpenShellAdapter>,
    os_runner: Arc<support::FakeRunner>,
    world: env::SharedWorld,
    logs: Arc<Mutex<(i32, String)>>,
}

fn cfg_root(tmp: &Path) -> PathBuf {
    tmp.join("repo")
        .join("security")
        .join("nemo-guardrails")
        .join("config")
}

fn write(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn sandbox_list(names: &[&str]) -> String {
    Value::Array(
        names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                json!({"name": n, "id": format!("id-{i}"), "phase": "Ready",
                    "created_at": "2026-10-24 11:00:00"})
            })
            .collect(),
    )
    .to_string()
}

fn os_adapter(tmp: &Path, runner: Arc<support::FakeRunner>) -> Arc<OpenShellAdapter> {
    let ctx = support::ctx(runner, &[], &[], &tmp.join("os-state"));
    Arc::new(OpenShellAdapter::new(ctx))
}

fn lab() -> Lab {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg_root(tmp.path())).unwrap();
    let gr_ctx = support::ctx(
        support::FakeRunner::new(),
        &[("guardrails", support::CLOSED)],
        &[("guardrails_config_dir", &cfg_root(tmp.path()))],
        &tmp.path().join("gr-state"),
    );
    let mut w = env::World::default();
    w.sandboxes = sandbox_list(&[SANDBOX, "sb-two"]);
    let (os_runner, world) = env::make_runner(Some(w));
    let logs = Arc::new(Mutex::new((0, String::new())));
    let l2 = Arc::clone(&logs);
    os_runner.on_prefix(
        &["openshell", "-g", "openshell", "logs"],
        support::with(move |_| {
            let (rc, text) = l2.lock().unwrap().clone();
            support::rc(rc, &text, if rc == 0 { "" } else { MARK })
        }),
    );
    let os = os_adapter(tmp.path(), Arc::clone(&os_runner));
    let mut adapters: HashMap<SystemId, Arc<dyn SystemAdapter>> = HashMap::new();
    adapters.insert(
        SystemId::Guardrails,
        Arc::new(GuardrailsAdapter::new(gr_ctx)),
    );
    adapters.insert(
        SystemId::Openshell,
        Arc::clone(&os) as Arc<dyn SystemAdapter>,
    );
    for sid in SystemId::ALL {
        adapters
            .entry(sid)
            .or_insert_with(|| common::FakeAdapter::new(sid, None) as Arc<dyn SystemAdapter>);
    }
    let settings = settings_for_test(json!({
        "state_dir": tmp.path().join("app-state").to_string_lossy(),
        "frontend_dir": tmp.path().join("front").to_string_lossy(),
        "repo_root": tmp.path().join("repo").to_string_lossy(),
        "background_tasks": false, "trust_loopback": true,
    }));
    let st = build_container(
        settings,
        Overrides {
            adapters,
            runner: Some(Arc::new(common::FakeRunner::default()) as Arc<dyn CommandRunner>),
            http: Some(Http::new().unwrap()),
            registry: None,
            environ: Some(Environ::from_pairs(&[])),
        },
    )
    .unwrap();
    let (_, viewer) = st.auth.mint(Role::Viewer, "").unwrap();
    let (_, admin) = st.auth.mint(Role::Admin, "").unwrap();
    Lab {
        tmp,
        st,
        viewer,
        admin,
        os,
        os_runner,
        world,
        logs,
    }
}

impl Lab {
    async fn get(&self, path: &str, key: Option<&str>) -> common::TestResp {
        let mut r = common::TestReq::new(Arc::clone(&self.st), "GET", path).client("192.0.2.10");
        if let Some(k) = key {
            r = r.token(k);
        }
        r.send().await
    }

    /// 200 for the viewer, and the answer never carries the marker.
    async fn viewer_json(&self, path: &str) -> Value {
        self.key_json(path, &self.viewer.clone()).await
    }

    async fn admin_json(&self, path: &str) -> Value {
        self.key_json(path, &self.admin.clone()).await
    }

    async fn key_json(&self, path: &str, key: &str) -> Value {
        let r = self.get(path, Some(key)).await;
        assert_eq!(r.status, 200, "{path}: {}", r.text());
        assert!(!r.text().contains(MARK), "{path} leaked: {}", r.text());
        r.json()
    }

    async fn status(&self, path: &str, key: Option<&str>) -> (u16, String) {
        let r = self.get(path, key).await;
        assert!(!r.text().contains(MARK), "{path} leaked: {}", r.text());
        (
            r.status,
            r.json()["error"]["code"].as_str().unwrap_or("").to_owned(),
        )
    }

    fn cfg(&self, name: &str) -> PathBuf {
        let d = cfg_root(self.tmp.path()).join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn set_logs(&self, rc: i32, text: &str) {
        *self.logs.lock().unwrap() = (rc, text.to_owned());
    }

    fn logs_calls(&self) -> usize {
        self.os_runner
            .count(&["openshell", "-g", "openshell", "logs"])
    }
}

fn problem_codes(v: &Value) -> Vec<(Value, String, u64)> {
    v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["config"].clone(),
                p["code"].as_str().unwrap().to_owned(),
                p["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

// ── Guardrails rails ────────────────────────────────────────────────────────────────────────────

const LAB_CONFIG: &str = r#"# a comment ZZMARKERZZ
models:
  - type: embeddings
    engine: openai
    model: ZZMARKERZZ-embed
  - type: main
    engine: openai
    model: org/lab-model
    parameters:
      base_url: http://ZZMARKERZZ:4000/v1
      api_key: ZZMARKERZZ
instructions:
  - type: general
    content: ZZMARKERZZ instructions
prompts:
  - task: self_check_input
    content: "ZZMARKERZZ prompt text"
passthrough: true
rails:
  input:
    flows:
      - self check input
      - "ZZMARKERZZ/../flow"
      - 42
  output:
    flows:
      - self check output
    streaming:
      enabled: true
      chunk_size: 200
      context_size: 50
      stream_first: false
  retrieval:
    flows:
      - check retrieval.v2
"#;

#[tokio::test]
async fn rails_answers_the_pipeline_of_each_config_and_nothing_else() {
    let lab = lab();
    let c = lab.cfg("lab-io");
    write(&c.join("config.yml"), LAB_CONFIG.as_bytes());
    write(
        &c.join("prompts.yml"),
        format!("prompts:\n  - task: x\n    content: {MARK}\n").as_bytes(),
    );
    write(
        &c.join("rails.co"),
        format!("define flow x\n  bot say \"{MARK}\"\n").as_bytes(),
    );
    let old = lab.cfg("old-style");
    write(
        &old.join("config.yaml"),
        b"models:\n  - type: main\n    model: m1\nstreaming: True\n",
    );
    // not a config id (upper case) and a directory without a config file: not listed
    write(&lab.cfg("Upper").join("config.yml"), b"rails: {}\n");
    lab.cfg("no-config");
    let v = lab.viewer_json("/api/v1/systems/guardrails/rails").await;
    assert_eq!(
        v["configs"],
        json!([
            {"id": "lab-io", "main_model": "org/lab-model", "input": ["self check input"],
             "output": ["self check output"],
             "streaming": {"enabled": true, "chunk_size": 200, "context_size": 50},
             "retrieval": ["check retrieval.v2"]},
            {"id": "old-style", "main_model": "m1", "input": [], "output": [],
             "streaming": {"enabled": true, "chunk_size": null, "context_size": null}, "retrieval": []},
        ])
    );
    assert_eq!(
        problem_codes(&v),
        [(json!("lab-io"), "flows.dropped".to_owned(), 2)]
    );
    // admins see the same (there is nothing role-dependent in the answer)
    assert_eq!(lab.admin_json("/api/v1/systems/guardrails/rails").await, v);
}

#[tokio::test]
async fn rails_refuses_hostile_config_files() {
    let lab = lab();
    // over the size cap
    write(
        &lab.cfg("big").join("config.yml"),
        format!("x: \"{}\"\n", MARK.repeat(30_000)).as_bytes(),
    );
    // not UTF-8
    let mut bytes = b"rails:\n  input:\n    flows: [a]\n# ".to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe, 0x00]);
    bytes.extend_from_slice(MARK.as_bytes());
    write(&lab.cfg("binary").join("config.yml"), &bytes);
    // 100,000 flow names (flow style, under the size cap)
    let mut many = String::from("rails:\n  output:\n    flows: [");
    for i in 0..100_000 {
        many.push_str(if i % 2 == 0 { "a," } else { "b," });
    }
    many.push_str("c]\n");
    assert!(many.len() < 256 * 1024);
    write(&lab.cfg("many").join("config.yml"), many.as_bytes());
    // a symlink and a hard link to a file outside the tree
    let outside = lab.tmp.path().join("outside.yml");
    write(
        &outside,
        format!("models:\n  - type: main\n    model: {MARK}\n").as_bytes(),
    );
    std::os::unix::fs::symlink(&outside, lab.cfg("linked").join("config.yml")).unwrap();
    std::fs::hard_link(&outside, lab.cfg("hardlinked").join("config.yml")).unwrap();
    // a YAML error, a list document, an alias bomb, a model that fails the alias check, a long flow name
    write(
        &lab.cfg("broken").join("config.yml"),
        format!("rails: [\n{MARK}").as_bytes(),
    );
    write(&lab.cfg("listdoc").join("config.yml"), b"- a\n- b\n");
    let mut bomb = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
    for i in 0..20 {
        let prev = if i == 0 {
            "a".to_owned()
        } else {
            format!("l{}", i - 1)
        };
        bomb.push_str(&format!(
            "l{i}: &l{i} [*{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}]\n"
        ));
    }
    bomb.push_str("rails:\n  input:\n    flows: *l19\n");
    write(&lab.cfg("bomb").join("config.yml"), bomb.as_bytes());
    write(
        &lab.cfg("odd").join("config.yml"),
        format!(
            "models:\n  - type: main\n    model: \"{MARK} with spaces\"\nrails:\n  input:\n    flows: [\"{}\"]\n",
            "f".repeat(81)
        )
        .as_bytes(),
    );
    let t0 = std::time::Instant::now();
    let v = lab.viewer_json("/api/v1/systems/guardrails/rails").await;
    assert!(t0.elapsed().as_secs() < 20, "{:?}", t0.elapsed());
    let by_id: HashMap<String, Value> = v["configs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["id"].as_str().unwrap().to_owned(), c.clone()))
        .collect();
    assert_eq!(by_id.len(), 9);
    let many = &by_id["many"]["output"];
    assert_eq!(many.as_array().unwrap().len(), RAILS_MAX_FLOWS);
    for (id, cfg) in &by_id {
        if id != "many" {
            assert_eq!(cfg["input"], json!([]), "{id}");
            assert_eq!(cfg["main_model"], Value::Null, "{id}");
        }
    }
    let mut codes = problem_codes(&v);
    codes.sort_by(|a, b| a.0.as_str().cmp(&b.0.as_str()).then(a.1.cmp(&b.1)));
    assert_eq!(
        codes,
        [
            (json!("big"), "config.too_large".to_owned(), 1),
            (json!("binary"), "config.not_utf8".to_owned(), 1),
            // the alias bomb's 9^19 * 10 items are ONE shared node: not expanded, not a flow list of strings
            (json!("bomb"), "flows.dropped".to_owned(), 9),
            (json!("broken"), "config.yaml_error".to_owned(), 1),
            (json!("hardlinked"), "config.unreadable".to_owned(), 1),
            (json!("linked"), "config.unreadable".to_owned(), 1),
            (json!("listdoc"), "config.not_a_mapping".to_owned(), 1),
            (
                json!("many"),
                "flows.capped".to_owned(),
                100_001 - RAILS_MAX_FLOWS as u64
            ),
            (json!("odd"), "flows.dropped".to_owned(), 1),
            (json!("odd"), "main_model.dropped".to_owned(), 1),
        ]
    );
}

#[tokio::test]
async fn rails_caps_the_number_of_configs() {
    let lab = lab();
    for i in 0..(RAILS_MAX_CONFIGS + 6) {
        write(
            &lab.cfg(&format!("c{i:03}")).join("config.yml"),
            b"rails:\n  input:\n    flows: [a]\n",
        );
    }
    let v = lab.viewer_json("/api/v1/systems/guardrails/rails").await;
    assert_eq!(v["configs"].as_array().unwrap().len(), RAILS_MAX_CONFIGS);
    assert_eq!(v["configs"][0]["id"], "c000");
    assert_eq!(
        problem_codes(&v),
        [(Value::Null, "configs.capped".to_owned(), 6)]
    );
}

#[tokio::test]
async fn rails_is_guardrails_only_and_needs_a_key() {
    let lab = lab();
    assert_eq!(
        lab.status("/api/v1/systems/openshell/rails", Some(&lab.viewer))
            .await,
        (404, "not_found".to_owned())
    );
    assert_eq!(
        lab.status("/api/v1/systems/nosuch/rails", Some(&lab.viewer))
            .await,
        (404, "not_found".to_owned())
    );
    assert_eq!(
        lab.status("/api/v1/systems/guardrails/rails", None).await,
        (401, "unauthorized".to_owned())
    );
    // no config dir at all: empty lists
    let v = lab.viewer_json("/api/v1/systems/guardrails/rails").await;
    assert_eq!(v, json!({"configs": [], "problems": []}));
}

// ── OpenShell decisions ─────────────────────────────────────────────────────────────────────────

fn hostile_log() -> String {
    let fixture = support::fx("openshell", "logs.txt");
    format!(
        "{fixture}\
[1792841090.000] [sandbox] [OCSF ] [ocsf] NET:CONNECT [WARN] DENIED api.example.com:443 {MARK} message text\n\
[1792841091.000] [sandbox] [OCSF ] [ocsf] PROCESS:EXEC [WARN] DENIED cmd=/bin/sh -c 'curl {MARK}'\n\
[1792841092.000] [sandbox] [OCSF ] [ocsf] FILE:OPEN [INFO] ALLOWED /home/{MARK}/secret.txt\n\
[1792841093.000] [sandbox] [OCSF ] [ocsf] {MARK}:WEIRD [INFO] BLOCKED [::1]:8080 {MARK}\n\
[1792841094.000] [sandbox] [OCSF ] [ocsf] HTTP:REQUEST [INFO] DENIED {}\n\
[1792841095.000] [sandbox] [OCSF ] [ocsf] DNS:RESOLVE [INFO] ALLOWED {}:53\n\
[1792841096.000] [sandbox] [INFO ] [supervisor] DENIED {MARK} not an OCSF line\n\
[nan] [sandbox] [OCSF ] [ocsf] SSH:OPEN [INFO] ALLOWED\n\
[1792841097.000] [sandbox] [OCSF ] [ocsf] SSH:OPEN [INFO] maybe {MARK}\n\
[1792841098.000] [sandbox] [OCSF ] [ocsf] SSH:CLOSE [INFO] DENIED \u{fffd}{MARK}\u{fffd}\n\
{}\n",
        "a".repeat(121),
        "h".repeat(150),
        format!(
            "[1792841099.000] [sandbox] [OCSF ] [ocsf] SSH:OPEN [INFO] ALLOWED {}",
            MARK.repeat(200_000 / MARK.len())
        ),
    )
}

#[tokio::test]
async fn decisions_parse_ocsf_lines_into_allowlisted_fields_only() {
    let lab = lab();
    lab.set_logs(0, &hostile_log());
    let v = lab
        .viewer_json(&format!(
            "/api/v1/systems/openshell/decisions?sandbox={SANDBOX}"
        ))
        .await;
    let row = |ts: f64, category: &str, verdict: &str, kind: &str, target: Value| json!({"ts": ts, "category": category, "verdict": verdict, "kind": kind, "target": target});
    assert_eq!(
        v,
        json!({
            "sandbox": SANDBOX,
            "rows": [
                row(1_792_841_081.496, "ssh", "allowed", "open", Value::Null),
                row(1_792_841_090.0, "network", "denied", "connect", json!("api.example.com:443")),
                row(1_792_841_091.0, "process", "denied", "exec", Value::Null),
                row(1_792_841_092.0, "filesystem", "allowed", "open", Value::Null),
                // an unknown class and activity are "other"; the target passed the host:port check
                row(1_792_841_093.0, "other", "denied", "other", json!("[::1]:8080")),
                row(1_792_841_094.0, "http", "denied", "request", Value::Null),
                row(1_792_841_095.0, "dns", "allowed", "resolve", Value::Null),
                row(1_792_841_098.0, "ssh", "denied", "close", Value::Null),
                row(1_792_841_099.0, "ssh", "allowed", "open", Value::Null),
            ],
            "truncated": false,
        })
    );
    // the CLI call is the sandbox_logs runner path: `logs NAME -n 2000`, 30 s, 512,000 bytes
    let calls: Vec<support::Call> = lab
        .os_runner
        .call_log()
        .into_iter()
        .filter(|c| c.argv.iter().any(|a| a == "logs"))
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].argv,
        [
            "openshell",
            "-g",
            "openshell",
            "logs",
            SANDBOX,
            "-n",
            "2000"
        ]
    );
    assert!((calls[0].timeout - 30.0).abs() < f64::EPSILON && calls[0].max_bytes == 512_000);
}

#[tokio::test]
async fn decisions_keep_the_newest_rows_and_are_cached() {
    let lab = lab();
    let mut text = String::new();
    for i in 0..(DECISIONS_MAX_ROWS + 500) {
        text.push_str(&format!(
            "[{}.5] [sandbox] [OCSF ] [ocsf] NET:CONNECT [INFO] {} host{i}.example:443 {MARK}\n",
            1_000_000 + i,
            if i % 3 == 0 { "DENIED" } else { "ALLOWED" }
        ));
    }
    lab.set_logs(0, &text);
    let path = format!("/api/v1/systems/openshell/decisions?sandbox={SANDBOX}");
    let v = lab.viewer_json(&path).await;
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), DECISIONS_DEFAULT_LIMIT);
    assert_eq!(v["truncated"], true);
    let last = DECISIONS_MAX_ROWS + 499;
    assert_eq!(
        rows.last().unwrap()["target"],
        format!("host{last}.example:443")
    );
    let all = lab.viewer_json(&format!("{path}&limit=2000")).await;
    let rows = all["rows"].as_array().unwrap();
    assert_eq!(rows.len(), DECISIONS_MAX_ROWS);
    assert_eq!(rows[0]["target"], "host500.example:443"); // the oldest 500 were dropped
    assert_eq!(all["truncated"], true);
    let one = lab.admin_json(&format!("{path}&limit=1")).await;
    assert_eq!(one["rows"].as_array().unwrap().len(), 1);
    // three answers, one CLI call (cached 10 s per sandbox)
    assert_eq!(lab.logs_calls(), 1);
    lab.viewer_json("/api/v1/systems/openshell/decisions?sandbox=sb-two")
        .await;
    assert_eq!(lab.logs_calls(), 2);
}

#[tokio::test]
async fn decisions_validate_the_query_and_the_live_list() {
    let lab = lab();
    lab.set_logs(0, "");
    let d = "/api/v1/systems/openshell/decisions";
    let v = Some(lab.viewer.as_str());
    for (q, want) in [
        (String::new(), 422),
        ("?sandbox=".to_owned(), 422),
        ("?sandbox=-bad".to_owned(), 422),
        ("?sandbox=a/b".to_owned(), 422),
        (format!("?sandbox={}", "a".repeat(129)), 422),
        (format!("?sandbox={SANDBOX}&limit=0"), 422),
        (format!("?sandbox={SANDBOX}&limit=2001"), 422),
        (format!("?sandbox={SANDBOX}&limit=abc"), 422),
        (format!("?sandbox={SANDBOX}&limit=1.5"), 422),
        ("?sandbox=not-live".to_owned(), 404),
    ] {
        let (status, code) = lab.status(&format!("{d}{q}"), v).await;
        assert_eq!(status, want, "{q}");
        assert_eq!(
            code,
            if want == 404 {
                "not_found"
            } else {
                "invalid_request"
            },
            "{q}"
        );
    }
    assert_eq!(lab.logs_calls(), 0); // nothing ran for a refused request
    assert_eq!(
        lab.status(
            &format!("/api/v1/systems/guardrails/decisions?sandbox={SANDBOX}"),
            v
        )
        .await,
        (404, "not_found".to_owned())
    );
    assert_eq!(
        lab.status(&format!("{d}?sandbox={SANDBOX}"), None).await,
        (401, "unauthorized".to_owned())
    );
    // the sandbox list fails: no name is live
    lab.world.lock().unwrap().sandbox_rc = 1;
    lab.os.invalidate_snapshot().await;
    assert_eq!(
        lab.status(&format!("{d}?sandbox={SANDBOX}"), v).await.0,
        404
    );
}

#[tokio::test]
async fn decisions_of_a_failed_cli_call_are_an_empty_answer_with_a_problem() {
    let lab = lab();
    lab.set_logs(1, &format!("Error: {MARK}"));
    let v = lab
        .viewer_json(&format!(
            "/api/v1/systems/openshell/decisions?sandbox={SANDBOX}"
        ))
        .await;
    assert_eq!(
        v,
        json!({"sandbox": SANDBOX, "rows": [], "truncated": false,
            "problem": {"code": "openshell.logs.failed",
                "message": "the openshell logs command failed for this sandbox"}})
    );
}

#[test]
fn decision_lines_are_looked_at_only_up_to_a_prefix() {
    // a valid head with a megabyte of message text after it is still one row; nothing of the tail is kept
    let line = format!(
        "[5] [sandbox] [OCSF ] [ocsf] SSH:OPEN [INFO] ALLOWED {}",
        "x ".repeat(500_000)
    );
    let (rows, more) = viz::decisions(&line);
    assert!(!more && rows.len() == 1 && rows[0]["target"] == "x");
    // 100,000 non-OCSF lines: linear, no row
    let junk = "[1] [x] junk\n".repeat(100_000);
    assert!(viz::decisions(&junk).0.is_empty());
}

// ── OpenShell persisted events ──────────────────────────────────────────────────────────────────

fn events_file(tmp: &Path) -> PathBuf {
    tmp.join("os-state").join("openshell").join(EVENTS_FILE)
}

fn file_lines(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn sandbox_events_are_persisted_privately_and_read_back_after_a_restart() {
    let lab = lab();
    let a = &lab.os;
    let first = a.recent_events(100).await.unwrap();
    assert_eq!(first.len(), 2); // both sandboxes "present"
    lab.world.lock().unwrap().sandboxes = sandbox_list(&[SANDBOX]); // sb-two deleted
    a.invalidate_snapshot().await;
    let evs = a.recent_events(100).await.unwrap();
    assert_eq!(evs[0].data["action"], "deleted");
    a.flush_persisted().await;
    let f = events_file(lab.tmp.path());
    assert_eq!(
        std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(f.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let lines = file_lines(&f);
    assert_eq!(lines.len(), 3);
    assert_eq!(a.persisted_lines(), 3);
    for l in &lines {
        let keys: Vec<&str> = l["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["name", "id", "phase", "action", "previous_phase"]);
        assert_eq!(l["kind"], EVENT_KIND);
    }

    // a restart with the gateway stopped: the events come back from the file, newest first
    let mut w = env::World::default();
    w.inspect_out = support::fx("openshell", "docker-inspect-stopped.HAND-WRITTEN.txt");
    let (r2, _) = env::make_runner(Some(w));
    let b = os_adapter(lab.tmp.path(), r2);
    let back = b.recent_events(100).await.unwrap();
    let want: Vec<(String, String)> = evs
        .iter()
        .map(|e| {
            (
                e.data["name"].as_str().unwrap().to_owned(),
                e.data["action"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let got: Vec<(String, String)> = back
        .iter()
        .map(|e| {
            (
                e.data["name"].as_str().unwrap().to_owned(),
                e.data["action"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(got, want);
    assert_eq!(back[0].summary, evs[0].summary);

    // a restart with the gateway up: the remembered sandboxes are the baseline (no second "present"), and a
    // sandbox deleted while the console was down is reported once
    let mut w = env::World::default();
    w.sandboxes = sandbox_list(&[]);
    let (r3, _) = env::make_runner(Some(w));
    let c = os_adapter(lab.tmp.path(), r3);
    let evs = c.recent_events(100).await.unwrap();
    assert_eq!(evs.len(), 4);
    assert_eq!(evs[0].data["action"], "deleted");
    assert_eq!(evs[0].data["name"], SANDBOX);
    c.flush_persisted().await;
    assert_eq!(file_lines(&f).len(), 4);
}

fn ev(i: usize) -> SystemEvent {
    let mut data = Map::new();
    data.insert("name".into(), json!(format!("sb-{i}")));
    data.insert("id".into(), json!(format!("id-{i}")));
    data.insert("phase".into(), json!("Ready"));
    data.insert("action".into(), json!("created"));
    data.insert("previous_phase".into(), Value::Null);
    #[allow(clippy::cast_precision_loss)]
    let ts = 1_000.0 + i as f64;
    viz::checked_event(ts, EVENT_KIND, &data).unwrap()
}

#[test]
fn the_events_file_is_bounded_and_rewritten_past_its_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("openshell");
    let batch: Vec<SystemEvent> = (0..EVENTS_REWRITE_AT).map(ev).collect();
    assert_eq!(viz::append(&dir, &batch, 0).unwrap(), EVENTS_REWRITE_AT); // at the limit: no rewrite
    let n = viz::append(&dir, &[ev(EVENTS_REWRITE_AT)], EVENTS_REWRITE_AT).unwrap();
    assert_eq!(n, EVENTS_KEEP);
    let lines = file_lines(&dir.join(EVENTS_FILE));
    assert_eq!(lines.len(), EVENTS_KEEP);
    assert_eq!(
        lines.last().unwrap()["data"]["name"],
        format!("sb-{EVENTS_REWRITE_AT}")
    );
    assert_eq!(
        lines[0]["data"]["name"],
        format!("sb-{}", EVENTS_REWRITE_AT + 1 - EVENTS_KEEP)
    );
    let mode = std::fs::metadata(dir.join(EVENTS_FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // no temp file is left behind
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    let back = viz::load(&dir);
    assert_eq!(back.lines, EVENTS_KEEP);
    assert_eq!(back.events.len(), EVENTS_KEEP);
}

#[tokio::test]
async fn the_adapter_rewrites_the_file_when_it_grows_past_the_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("os-state").join("openshell");
    let batch: Vec<SystemEvent> = (0..EVENTS_REWRITE_AT - 1).map(ev).collect();
    viz::append(&dir, &batch, 0).unwrap();
    let mut w = env::World::default();
    w.sandboxes = sandbox_list(&["late-one", "late-two"]);
    let (r, _) = env::make_runner(Some(w));
    let a = os_adapter(tmp.path(), r);
    let evs = a.recent_events(500).await.unwrap();
    assert_eq!(evs.len(), RING);
    a.flush_persisted().await;
    assert_eq!(a.persisted_lines(), EVENTS_KEEP);
    let lines = file_lines(&dir.join(EVENTS_FILE));
    assert_eq!(lines.len(), EVENTS_KEEP);
    // the newest lines are this poll's: the two new sandboxes and the remembered ones that are gone
    assert!(
        lines
            .iter()
            .rev()
            .take(202)
            .any(|l| l["data"]["name"] == "late-two" && l["data"]["action"] == "created")
    );
}

#[test]
fn a_hostile_events_file_is_read_as_data() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("openshell");
    std::fs::create_dir_all(&dir).unwrap();
    let good = serde_json::to_string(&ev(1)).unwrap();
    let mut planted = serde_json::to_value(ev(2)).unwrap();
    planted["summary"] = json!(MARK);
    planted["data"]["extra"] = json!(MARK);
    let bad_name = {
        let mut v = serde_json::to_value(ev(3)).unwrap();
        v["data"]["name"] = json!(format!("../{MARK}"));
        v
    };
    let bad_kind = {
        let mut v = serde_json::to_value(ev(4)).unwrap();
        v["kind"] = json!("other.kind");
        v
    };
    let bad_action = {
        let mut v = serde_json::to_value(ev(5)).unwrap();
        v["data"]["action"] = json!(MARK);
        v
    };
    let mut text = Vec::new();
    for l in [
        good.clone(),
        planted.to_string(),
        bad_name.to_string(),
        bad_kind.to_string(),
        bad_action.to_string(),
        format!("{{\"ts\": 1e999, \"system\": \"openshell\", \"kind\": \"{EVENT_KIND}\"}}"),
        format!("not json {MARK}"),
        format!("{{\"pad\": \"{}\"}}", MARK.repeat(1000)),
    ] {
        text.extend_from_slice(l.as_bytes());
        text.push(b'\n');
    }
    text.extend_from_slice(&[0xff, 0xfe, b'\n']);
    std::fs::write(dir.join(EVENTS_FILE), &text).unwrap();
    let back = viz::load(&dir);
    assert_eq!(back.lines, 9);
    assert_eq!(back.events.len(), 2);
    let all = serde_json::to_string(&back.events).unwrap();
    assert!(!all.contains(MARK), "{all}");
    assert_eq!(back.events[1].summary, "sandbox sb-2 created (Ready)");

    // a symlinked file, a hard link, a symlinked directory: nothing is read
    let elsewhere = tmp.path().join("elsewhere.jsonl");
    std::fs::write(&elsewhere, format!("{good}\n")).unwrap();
    std::fs::remove_file(dir.join(EVENTS_FILE)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join(EVENTS_FILE)).unwrap();
    assert!(viz::load(&dir).events.is_empty());
    assert!(viz::append(&dir, &[ev(9)], 0).is_err()); // never written through the link
    assert_eq!(
        std::fs::read_to_string(&elsewhere).unwrap(),
        format!("{good}\n")
    );
    std::fs::remove_file(dir.join(EVENTS_FILE)).unwrap();
    std::fs::hard_link(&elsewhere, dir.join(EVENTS_FILE)).unwrap();
    assert!(viz::load(&dir).events.is_empty());
    assert!(viz::append(&dir, &[ev(9)], 0).is_err());
    let linked_dir = tmp.path().join("linked");
    std::os::unix::fs::symlink(&dir, &linked_dir).unwrap();
    assert!(viz::load(&linked_dir).events.is_empty());
}

// ── audit filters and summary ───────────────────────────────────────────────────────────────────

fn audit_row(lab: &Lab, ts: f64, actor: &str, path: &str, system: Option<&str>, status: i64) {
    lab.st
        .audit
        .store
        .execute(
            "INSERT INTO audit(ts, actor, role, method, path, system, status, params) VALUES (?,?,?,?,?,?,?,?)",
            rusqlite::params![
                ts,
                actor,
                "admin",
                "POST",
                path,
                system,
                status,
                json!({"note": MARK}).to_string()
            ],
        )
        .unwrap();
}

fn seed_audit(lab: &Lab) -> f64 {
    let now = nemo_console::domain::models::now_ts();
    let day = |d: f64| now - d * 86_400.0;
    audit_row(
        lab,
        day(0.1),
        "key:aaa",
        "/api/v1/systems/dynamo/actions/x",
        Some("dynamo"),
        200,
    );
    audit_row(
        lab,
        day(0.2),
        "key:aaa",
        "/api/v1/systems/dynamo/actions/x",
        Some("dynamo"),
        500,
    );
    audit_row(lab, day(0.3), "key:bbb", "/api/v1/settings", None, 403);
    audit_row(
        lab,
        day(0.4),
        &format!("{MARK} with spaces"),
        &format!("/api/v1/systems/bogus/{MARK}"),
        Some("bogus"),
        404,
    );
    audit_row(
        lab,
        day(3.0),
        "loopback",
        "/api/v1/systems/relay/config/x/apply",
        Some("relay"),
        200,
    );
    audit_row(
        lab,
        day(20.0),
        "key:aaa",
        "/api/v1/systems/relay/config/x/apply",
        Some("relay"),
        401,
    );
    audit_row(lab, day(40.0), "key:aaa", "/api/v1/settings", None, 200);
    now
}

#[tokio::test]
async fn audit_filters_select_rows_and_are_admin_only() {
    let lab = lab();
    let now = seed_audit(&lab);
    let ids = |v: &Value| -> Vec<i64> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_i64().unwrap())
            .collect()
    };
    let get = |q: String| {
        let lab = &lab;
        async move {
            let r = lab
                .get(&format!("/api/v1/audit{q}"), Some(&lab.admin))
                .await;
            assert_eq!(r.status, 200, "{q}: {}", r.text());
            r.json()
        }
    };
    assert_eq!(ids(&get(String::new()).await), [7, 6, 5, 4, 3, 2, 1]);
    let since = now - 86_400.0;
    assert_eq!(ids(&get(format!("?since={since}")).await), [4, 3, 2, 1]);
    assert_eq!(
        ids(&get(format!("?until={}", now - 2.0 * 86_400.0)).await),
        [7, 6, 5]
    );
    assert_eq!(
        ids(&get(format!(
            "?since={}&until={}",
            now - 25.0 * 86_400.0,
            now - 86_400.0
        ))
        .await),
        [6, 5]
    );
    assert_eq!(ids(&get("?actor=key:aaa".to_owned()).await), [7, 6, 2, 1]);
    assert_eq!(ids(&get("?status=error".to_owned()).await), [6, 4, 3, 2]);
    assert_eq!(ids(&get("?status=ok".to_owned()).await), [7, 5, 1]);
    assert_eq!(
        ids(&get("?actor=key:aaa&status=ok&system=dynamo&limit=5".to_owned()).await),
        [1]
    );
    // empty values mean "no filter"
    assert_eq!(
        ids(&get("?since=&until=&actor=&status=".to_owned()).await).len(),
        7
    );
    for q in [
        "?since=abc",
        "?since=-1",
        "?until=1e12",
        "?since=nan",
        "?since=inf",
        "?since=10&until=5",
        "?actor=a%20b",
        "?actor=-x",
        "?status=maybe",
        "?status=OK",
    ] {
        let (status, code) = lab
            .status(&format!("/api/v1/audit{q}"), Some(&lab.admin))
            .await;
        assert_eq!((status, code.as_str()), (422, "invalid_request"), "{q}");
    }
    // a viewer is refused by the route AND by the handler
    assert_eq!(
        lab.status("/api/v1/audit?status=ok", Some(&lab.viewer))
            .await,
        (403, "forbidden".to_owned())
    );
    assert_eq!(
        lab.status("/api/v1/audit?status=ok", None).await,
        (401, "unauthorized".to_owned())
    );
}

#[tokio::test]
async fn audit_summary_counts_without_paths_or_params() {
    let lab = lab();
    let now = seed_audit(&lab);
    let v = lab.admin_json("/api/v1/audit/summary?window=7d").await;
    assert_eq!(v["window"], "7d");
    assert_eq!(v["bucket_s"], 21_600);
    assert!((v["to"].as_f64().unwrap() - now).abs() < 60.0);
    assert!(
        (v["to"].as_f64().unwrap() - v["from"].as_f64().unwrap() - 7.0 * 86_400.0).abs() < 1e-6
    );
    let total: i64 = v["by_day"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["count"].as_i64().unwrap())
        .sum();
    assert_eq!(total, 5);
    let systems: Vec<Value> = v["by_day"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["system"].clone())
        .collect();
    assert!(systems.contains(&json!("dynamo")) && systems.contains(&json!("other")));
    assert!(systems.contains(&Value::Null) && systems.contains(&json!("relay")));
    for r in v["by_day"].as_array().unwrap() {
        let d = r["day"].as_str().unwrap();
        assert!(
            d.len() == 10 && d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-',
            "{d}"
        );
    }
    let mut actors: Vec<(Value, Value, i64)> = v["by_actor"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["actor"].clone(),
                r["system"].clone(),
                r["count"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(actors[0], (json!("key:aaa"), json!("dynamo"), 2)); // the busiest first
    actors.sort_by_key(|a| a.0.to_string() + &a.1.to_string());
    assert_eq!(
        actors,
        [
            (json!("key:aaa"), json!("dynamo"), 2),
            (json!("key:bbb"), Value::Null, 1),
            (json!("loopback"), json!("relay"), 1),
            (Value::Null, json!("other"), 1), // an actor that fails the check is null
        ]
    );
    let errors: i64 = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            assert_eq!(e["ts_bucket"].as_i64().unwrap() % 21_600, 0);
            e["count"].as_i64().unwrap()
        })
        .sum();
    assert_eq!(errors, 3);
    assert_eq!(v["denied_total"], 1);
    assert_eq!(v["truncated"], false);
    let all = lab.admin_json("/api/v1/audit/summary?window=30d").await;
    assert_eq!(all["denied_total"], 2);
    assert_eq!(all["bucket_s"], 86_400);
    let day = lab.admin_json("/api/v1/audit/summary?window=24h").await;
    assert_eq!(
        day["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["count"].as_i64().unwrap())
            .sum::<i64>(),
        3
    );
    let dflt = lab.admin_json("/api/v1/audit/summary").await;
    assert_eq!(dflt["window"], SUMMARY_DEFAULT_WINDOW);
    for q in ["?window=1h", "?window=", "?window=7D"] {
        assert_eq!(
            lab.status(&format!("/api/v1/audit/summary{q}"), Some(&lab.admin))
                .await,
            (422, "invalid_request".to_owned()),
            "{q}"
        );
    }
    assert_eq!(
        lab.status("/api/v1/audit/summary", Some(&lab.viewer)).await,
        (403, "forbidden".to_owned())
    );
    assert_eq!(
        lab.status("/api/v1/audit/summary", None).await,
        (401, "unauthorized".to_owned())
    );
}

#[tokio::test]
async fn audit_handlers_check_the_role_themselves() {
    // the pipeline's route table says admin; the handlers check again (defence in depth): call them directly with a
    // viewer principal, bypassing the route guard
    let lab = lab();
    for (path, h) in [
        ("/api/v1/audit", "audit"),
        ("/api/v1/audit/summary", "summary"),
    ] {
        let req = nemo_console::api::Req {
            state: Arc::clone(&lab.st),
            method: "GET".into(),
            path: path.into(),
            query: Vec::new(),
            headers: axum::http::HeaderMap::new(),
            principal: nemo_console::services::auth::Principal {
                name: "key:viewer".into(),
                role: nemo_console::services::auth::PrincipalRole::Viewer,
            },
            params: HashMap::new(),
            body: nemo_console::api::bodies::RawBody::Absent,
        };
        let r = if h == "audit" {
            nemo_console::api::admin::audit(req).await
        } else {
            nemo_console::api::admin::audit_summary(req).await
        };
        assert_eq!(r.err().map(|e| e.status), Some(403), "{path}");
    }
}

#[tokio::test]
async fn a_summary_over_many_actors_is_capped() {
    let lab = lab();
    let now = nemo_console::domain::models::now_ts();
    for i in 0..300 {
        for _ in 0..(i % 3 + 1) {
            audit_row(
                &lab,
                now - 10.0,
                &format!("key:k{i:03}"),
                "/api/v1/settings",
                None,
                200,
            );
        }
    }
    let v = lab.admin_json("/api/v1/audit/summary?window=24h").await;
    let rows = v["by_actor"].as_array().unwrap();
    assert_eq!(rows.len(), 200);
    assert_eq!(v["truncated"], true);
    assert!(rows.iter().take(100).all(|r| r["count"] == 3));
}

// ── OpenAPI ─────────────────────────────────────────────────────────────────────────────────────

#[test]
fn the_openapi_entries_of_the_wave4_routes_match_the_code() {
    let doc: Value = serde_json::from_str(nemo_console::api::admin::OPENAPI_JSON).unwrap();
    let names = |path: &str| -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = doc["paths"][path]["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["name"].as_str().unwrap().to_owned(),
                    p["in"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        v.sort();
        v
    };
    let q = |n: &str| (n.to_owned(), "query".to_owned());
    let sys = ("system_id".to_owned(), "path".to_owned());
    assert_eq!(
        names("/api/v1/systems/{system_id}/rails"),
        std::slice::from_ref(&sys)
    );
    assert_eq!(
        names("/api/v1/systems/{system_id}/decisions"),
        [q("limit"), q("sandbox"), sys]
    );
    assert_eq!(names("/api/v1/audit/summary"), [q("window")]);
    assert_eq!(
        names("/api/v1/audit"),
        [
            q("actor"),
            q("limit"),
            q("since"),
            q("status"),
            q("system"),
            q("until")
        ]
    );
    let dec = &doc["paths"]["/api/v1/systems/{system_id}/decisions"]["get"]["parameters"];
    let limit = dec
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "limit")
        .unwrap();
    assert_eq!(limit["schema"]["maximum"], DECISIONS_MAX_ROWS);
    assert_eq!(limit["schema"]["default"], DECISIONS_DEFAULT_LIMIT);
    let win = &doc["paths"]["/api/v1/audit/summary"]["get"]["parameters"][0]["schema"];
    let windows: Vec<&str> = SUMMARY_WINDOWS.iter().map(|(w, _, _)| *w).collect();
    assert_eq!(win["enum"], json!(windows));
    assert_eq!(win["default"], SUMMARY_DEFAULT_WINDOW);
    let audit = &doc["paths"]["/api/v1/audit"]["get"]["parameters"];
    let status = audit
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "status")
        .unwrap();
    assert_eq!(status["schema"]["anyOf"][0]["enum"], json!(["ok", "error"]));
    for (path, role) in [
        (
            "/api/v1/systems/{system_id}/rails",
            nemo_console::api::RoleDep::Viewer,
        ),
        (
            "/api/v1/systems/{system_id}/decisions",
            nemo_console::api::RoleDep::Viewer,
        ),
        ("/api/v1/audit/summary", nemo_console::api::RoleDep::Admin),
        ("/api/v1/audit", nemo_console::api::RoleDep::Admin),
    ] {
        let def = nemo_console::api::ROUTES
            .iter()
            .find(|r| r.path == path)
            .unwrap();
        assert_eq!(def.methods, ["GET"]);
        assert_eq!(def.role, role, "{path}");
        assert!(
            doc["paths"][path]["get"]["description"]
                .as_str()
                .unwrap()
                .contains("9.16"),
            "{path}"
        );
    }
}
