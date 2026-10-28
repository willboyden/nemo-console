//! Relay test helpers (port of backend/tests/relay/{conftest,relay_helpers}.py): the fake home layout, the standard
//! prefix runner, and the two fake upstreams (gateway :4040 and LiteLLM :4000 in the Python tests).
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use nemo_console::adapters::base::CommandResult;
use nemo_console::adapters::relay::RelayAdapter;
use serde_json::json;

use crate::switchyard_support::{FakeHttp, PrefixRunner, Reply, ctx, fixture, fixture_dir, rc};

pub fn fx(name: &str) -> String {
    fixture("relay", name)
}

pub fn fx_bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture_dir("relay").join(name)).unwrap()
}

pub fn okr(stdout: &str) -> CommandResult {
    rc(0, stdout, "")
}

/// `standard_runner(show)`.
pub fn standard_runner(show: Option<&str>) -> Arc<PrefixRunner> {
    let r = PrefixRunner::new();
    r.on(
        &["systemctl", "--user", "--timestamp=unix", "show"],
        okr(&show.map_or_else(|| fx("systemctl_show_active.txt"), str::to_owned)),
    );
    r.on(&["nemo-relay", "--version"], okr(&fx("version.txt")));
    r.on(
        &["nemo-relay", "plugins", "list", "--json"],
        okr(&fx("plugins_list.json")),
    );
    r.on(
        &["nemo-relay", "plugins", "validate"],
        okr(&fx("plugins_validate.json")),
    );
    r.on(&["nemo-relay", "agents", "--json"], okr(&fx("agents.json")));
    r.on(
        &["nemo-relay", "doctor", "--offline", "--json"],
        okr(&fx("doctor_offline.json")),
    );
    r.on(
        &["systemctl", "--user", "status"],
        okr("* nemo-relay-gateway.service - fake\n  Active: active (running)\n"),
    );
    r.on(&["systemctl", "--user", "restart"], okr(""));
    r
}

/// Python `build_tree`: the fake home layout; returns the ctx.paths mapping.
pub fn build_tree(
    tmp: &Path,
    plugins_toml: Option<&str>,
    atof_lines: Option<&str>,
) -> Vec<(&'static str, PathBuf)> {
    let cfg = tmp.join("cfg");
    let atof = tmp.join("atof");
    let atif = tmp.join("atif");
    for d in [&cfg, &atof, &atif] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(cfg.join("routes.toml"), fx("routes.toml")).unwrap();
    std::fs::write(
        cfg.join("plugins.toml"),
        plugins_toml.map_or_else(|| fx("plugins.toml"), str::to_owned),
    )
    .unwrap();
    std::fs::write(cfg.join("config.toml"), fx("config.toml")).unwrap();
    // older than the fixture unit's start time
    let f = std::fs::File::options()
        .write(true)
        .open(cfg.join("plugins.toml"))
        .unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000))
        .unwrap();
    std::fs::write(atof.join("relay-events.jsonl"), "").unwrap();
    std::fs::write(
        atof.join("relay-events.jsonl.1"),
        atof_lines.map_or_else(|| fx("atof_sample.jsonl"), str::to_owned),
    )
    .unwrap();
    std::fs::write(atif.join("nemo-relay-atif-aaa.json"), "{}").unwrap();
    std::fs::write(atif.join("nemo-relay-atif-bbb.json"), "{}").unwrap();
    std::fs::write(atif.join("notes.txt"), "not a trajectory").unwrap();
    vec![
        ("relay_routes", cfg.join("routes.toml")),
        ("relay_plugins", cfg.join("plugins.toml")),
        ("relay_config", cfg.join("config.toml")),
        ("relay_atof_dir", atof),
        ("relay_atif_dir", atif),
    ]
}

/// The conftest `router` fixture: the gateway and its upstream, both fakes.
pub struct Router {
    pub gateway: FakeHttp,
    pub litellm: FakeHttp,
}

impl Router {
    pub async fn new() -> Router {
        let gateway = FakeHttp::start().await;
        let litellm = FakeHttp::start().await;
        gateway.get("/health", Reply::status(404));
        litellm.get("/health/liveliness", Reply::json(200, &json!("I'm alive!")));
        // kept answering so a regression that polls it again shows up as a call count
        gateway.get(
            "/v1/models",
            Reply::json(
                401,
                &json!({"error": {"message": "Authentication Error, No api key passed in."}}),
            ),
        );
        Router { gateway, litellm }
    }
}

/// One `make_adapter(...)` result: the adapter, its runner, its paths and the temp dir that holds them.
pub struct Made {
    pub a: Arc<RelayAdapter>,
    pub run: Arc<PrefixRunner>,
    pub paths: Vec<(&'static str, PathBuf)>,
    pub tmp: tempfile::TempDir,
}

impl Made {
    pub fn path(&self, key: &str) -> PathBuf {
        self.paths
            .iter()
            .find(|(k, _)| *k == key)
            .unwrap()
            .1
            .clone()
    }
}

/// The conftest `make_adapter` fixture (poll_interval 0, follow_interval 0.01).
pub fn make_adapter(
    router: &Router,
    runner: Option<Arc<PrefixRunner>>,
    plugins_toml: Option<&str>,
    atof_lines: Option<&str>,
    tweak: impl FnOnce(&mut RelayAdapter),
) -> Made {
    let tmp = tempfile::tempdir().unwrap();
    let run = runner.unwrap_or_else(|| standard_runner(None));
    let paths = build_tree(tmp.path(), plugins_toml, atof_lines);
    let c = ctx(
        run.clone(),
        &[
            ("relay", &router.gateway.uri()),
            ("litellm", &router.litellm.uri()),
        ],
        &paths,
        &tmp.path().join("state"),
    );
    let mut a = RelayAdapter::new(c);
    a.poll_interval = 0.0;
    a.follow_interval = 0.01;
    tweak(&mut a);
    Made {
        a: Arc::new(a),
        run,
        paths,
        tmp,
    }
}

/// A second adapter over the same tree with changed paths (Python tests reassign `a.ctx.paths[...]`).
pub fn with_path(m: &Made, key: &'static str, p: PathBuf) -> Arc<RelayAdapter> {
    let mut c = m.a.ctx.clone();
    c.paths.insert(key.to_owned(), p);
    let mut a = RelayAdapter::new(c);
    a.poll_interval = 0.0;
    a.follow_interval = 0.01;
    Arc::new(a)
}
