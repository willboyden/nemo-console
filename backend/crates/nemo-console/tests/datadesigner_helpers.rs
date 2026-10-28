//! Test helpers for the Data Designer adapter (port of backend/tests/datadesigner/dd_helpers.py + conftest.py): a
//! fake runner that writes real-shaped artifacts (captured live, in fixtures/datadesigner) and a scripts tree. The
//! generic fakes (prefix-matching runner, gates, mock router) are skillevaluator_helpers.rs, included as `fakes`.
//! Everything offline; the router is a local mock or a CLOSED port.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "skillevaluator_helpers.rs"]
pub mod fakes;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, CommandRunner};
use nemo_console::adapters::datadesigner::DataDesignerAdapter;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use nemo_console::services::pyfmt::json_dumps;
use serde_json::{Value, json};

#[allow(unused_imports)]
pub use fakes::{
    CLOSED, FakeRunner, Gate, Resp, codes, func, gated, mode_of, ok, ok_rc, params, raises, res,
    router,
};

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/datadesigner");
pub const LAUNCHER: &str = "data-designer-local";
pub const EXPORTER: &str = "data-designer-export";
pub const PREVIEW_STAMP: &str = "20261023_222454";

pub fn fxp(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

pub fn fx(name: &str) -> String {
    std::fs::read_to_string(fxp(name)).unwrap()
}

/// The captured records (dd_smoke.jsonl), key order kept.
pub fn rows() -> Vec<Value> {
    fx("dd_smoke.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `csv.DictWriter(buf, fieldnames=sorted(rows[0]))`: excel dialect, QUOTE_MINIMAL, `\r\n` line ends.
pub fn csv_text(rows: &[Value]) -> String {
    let mut fields: Vec<String> = rows[0].as_object().unwrap().keys().cloned().collect();
    fields.sort();
    let quote = |s: &str| {
        if s.contains([',', '"', '\r', '\n']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_owned()
        }
    };
    let mut out = fields
        .iter()
        .map(|f| quote(f))
        .collect::<Vec<_>>()
        .join(",")
        + "\r\n";
    for r in rows {
        let line: Vec<String> = fields.iter().map(|f| quote(&py_str(&r[f]))).collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    out
}

/// `data-designer-export IN.parquet OUT.ext`: the extension picks the format.
pub fn export_responder(rc: i32, rows_override: Option<Vec<Value>>) -> Resp {
    func(move |argv| {
        let out = PathBuf::from(&argv[2]);
        let rs = rows_override.clone().unwrap_or_else(rows);
        match out.extension().and_then(|e| e.to_str()) {
            Some("jsonl") => {
                let text: String = rs.iter().map(|r| json_dumps(r) + "\n").collect();
                std::fs::write(&out, text).unwrap();
            }
            Some("csv") => std::fs::write(&out, csv_text(&rs)).unwrap(),
            Some("json") => std::fs::write(&out, json_dumps(&Value::Array(rs))).unwrap(),
            _ => std::fs::write(&out, "PARQUET").unwrap(),
        }
        res("", rc, "")
    })
}

fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let i = argv.iter().position(|a| a == flag).unwrap();
    &argv[i + 1]
}

/// `data-designer-local preview SCRIPT -n N --save-results -o DIR`: the captured preview_results_* tree.
pub fn writes_preview(rc: i32, stderr: &'static str) -> Resp {
    func(move |argv| {
        let out = Path::new(after(argv, "-o")).join(format!("preview_results_{PREVIEW_STAMP}"));
        std::fs::create_dir_all(out.join("sample_records")).unwrap();
        std::fs::copy(
            fxp("preview-artifacts/dataset.parquet"),
            out.join("dataset.parquet"),
        )
        .unwrap();
        std::fs::copy(
            fxp("preview-artifacts/report.html"),
            out.join("report.html"),
        )
        .unwrap();
        for e in std::fs::read_dir(fxp("sample_records")).unwrap() {
            let p = e.unwrap().path();
            std::fs::copy(&p, out.join("sample_records").join(p.file_name().unwrap())).unwrap();
        }
        res("preview progress lines", rc, stderr)
    })
}

/// `data-designer-local create ... --artifact-path DIR --no-tui`: DIR/<NAME>/ with metadata.json + batch parquets.
pub fn writes_create(
    rc: i32,
    stderr: &'static str,
    batches: usize,
    records: Option<i64>,
    with_metadata: bool,
    with_file_paths: bool,
) -> Resp {
    func(move |argv| {
        let out = Path::new(after(argv, "--artifact-path")).join(after(argv, "--dataset-name"));
        std::fs::create_dir_all(out.join("parquet-files")).unwrap();
        for i in 0..batches.max(1) {
            std::fs::copy(
                fxp("parquet-files/batch_00000.parquet"),
                out.join("parquet-files")
                    .join(format!("batch_{i:05}.parquet")),
            )
            .unwrap();
        }
        if with_metadata {
            let mut md: Value =
                serde_json::from_str(&fx("create-artifacts/metadata.json")).unwrap();
            if let Some(r) = records {
                md["actual_num_records"] = json!(r);
            }
            md["file_paths"] = if with_file_paths {
                json!({"parquet-files": (0..batches).map(|i| format!("parquet-files/batch_{i:05}.parquet")).collect::<Vec<_>>()})
            } else {
                json!({})
            };
            std::fs::write(out.join("metadata.json"), md.to_string()).unwrap();
        }
        std::fs::copy(
            fxp("create-artifacts/builder_config.json"),
            out.join("builder_config.json"),
        )
        .unwrap();
        res("create progress lines", rc, stderr)
    })
}

pub fn create_default() -> Resp {
    writes_create(0, "", 1, None, true, true)
}

pub fn standard_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on(&[LAUNCHER, "--version"], ok("0.9.3\n"));
    r.on(&[LAUNCHER, "preview"], writes_preview(0, ""));
    r.on(&[LAUNCHER, "create"], create_default());
    r.on(
        &[LAUNCHER, "check-models"],
        ok("All 2 models reachable.\nlab-router: ok\nlab-embed: ok\n"),
    );
    r.on(&[LAUNCHER, "validate"], ok("Config is valid.\n"));
    r.on(&[EXPORTER], export_responder(0, None));
    r
}

/// Idempotent: a scripts dir with two valid scripts plus every kind of rejected entry.
pub fn build_scripts(tmp: &Path) -> PathBuf {
    let root = tmp.join("home/Datasets/data-designer/scripts");
    if root.is_dir() {
        return root;
    }
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("reviews_smoke.py"),
        "\"\"\"Smoke config (fixture).\"\"\"\n\n\ndef load_config_builder():\n    raise NotImplementedError\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tiny.yaml"),
        "columns:\n  - name: product_category\n    column_type: sampler\n",
    )
    .unwrap();
    std::fs::write(
        root.join("UPPER.py"),
        "# uppercase names are outside the core's id shape\n",
    )
    .unwrap();
    std::fs::write(root.join("notes.txt"), "wrong extension\n").unwrap();
    std::fs::create_dir(root.join("subdir.py")).unwrap();
    std::fs::write(tmp.join("outside.py"), "secret = True\n").unwrap();
    std::os::unix::fs::symlink(tmp.join("outside.py"), root.join("linked.py")).unwrap();
    root
}

/// Options of Python's `make_ctx(...)`.
#[derive(Clone, Copy)]
pub struct Opts<'a> {
    pub scripts: bool,
    pub providers: bool,
    pub key_file: bool,
    pub repo: Option<&'a Path>,
}

impl Default for Opts<'_> {
    fn default() -> Self {
        Opts {
            scripts: true,
            providers: true,
            key_file: true,
            repo: None,
        }
    }
}

pub fn make_ctx(
    tmp: &Path,
    runner: Arc<dyn CommandRunner>,
    litellm: &str,
    o: Opts<'_>,
) -> AdapterContext {
    let mut paths = IndexMap::new();
    paths.insert("datasets_root".to_owned(), tmp.join("home/Datasets"));
    let cfg = tmp.join("home/.data-designer");
    paths.insert("dd_config_dir".to_owned(), cfg.clone());
    let key = tmp.join("home/.config/data-designer/litellm.key");
    paths.insert("dd_key_file".to_owned(), key.clone());
    if o.scripts {
        paths.insert("scripts_dir".to_owned(), build_scripts(tmp));
    }
    if o.providers {
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("model_providers.yaml"), fx("model_providers.yaml")).unwrap();
        std::fs::write(cfg.join("model_configs.yaml"), fx("model_configs.yaml")).unwrap();
    }
    if o.key_file {
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "sk-FAKE-not-a-real-key-0123456789\n").unwrap();
    }
    if let Some(r) = o.repo {
        paths.insert("repo".to_owned(), r.to_path_buf());
    }
    let mut urls = IndexMap::new();
    urls.insert("litellm".to_owned(), litellm.to_owned());
    AdapterContext {
        runner,
        http: Http::new().unwrap(),
        urls,
        paths,
        secret_env: IndexMap::new(),
        state_dir: tmp.join("state"),
        environ: Environ::fixed(HashMap::new()),
    }
}

/// Python's `make_adapter` fixture.
pub struct Fx {
    pub tmp: tempfile::TempDir,
    pub router: wiremock::MockServer,
}

impl Fx {
    pub async fn new() -> Fx {
        Fx {
            tmp: tempfile::tempdir().unwrap(),
            router: router(200).await,
        }
    }

    pub fn path(&self) -> PathBuf {
        nemo_console::services::support::realpath(self.tmp.path())
    }

    pub fn adapter(&self, runner: &Arc<FakeRunner>) -> DataDesignerAdapter {
        self.adapter_with(runner, Opts::default())
    }

    pub fn adapter_with(&self, runner: &Arc<FakeRunner>, o: Opts<'_>) -> DataDesignerAdapter {
        self.adapter_url(runner, o, &self.router.uri())
    }

    pub fn adapter_url(
        &self,
        runner: &Arc<FakeRunner>,
        o: Opts<'_>,
        url: &str,
    ) -> DataDesignerAdapter {
        let a = DataDesignerAdapter::new(make_ctx(
            &self.path(),
            Arc::clone(runner) as Arc<dyn CommandRunner>,
            url,
            o,
        ));
        a.set_follow_interval(0.01);
        a
    }
}

pub fn run_json(a: &DataDesignerAdapter, run_id: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(a.runs_dir().join(run_id).join("run.json")).unwrap(),
    )
    .unwrap()
}

pub fn datasets_root(a: &DataDesignerAdapter) -> PathBuf {
    a.ctx().paths["datasets_root"].clone()
}

pub async fn finish(a: &DataDesignerAdapter) {
    assert!(a.live().wait(5.0).await, "the build did not finish");
}

/// The run id of `Started dataset build <id> (...)` (the background task may already be done when the test looks).
pub fn started_run_id(output: &str) -> String {
    let rest = output.split("Started dataset build ").nth(1).unwrap();
    rest.split(' ').next().unwrap().to_owned()
}
