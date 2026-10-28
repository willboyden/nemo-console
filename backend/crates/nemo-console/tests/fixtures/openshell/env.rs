//! The OpenShell test environment (Python tests/openshell/os_helpers.py + conftest.py): a fake runner answering with
//! CAPTURED CLI/docker output from a mutable "world" the tests change between polls, and a runner directory laid out
//! like clients/data-designer/runner. Everything offline: no CLI, no docker, no network.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nemo_console::adapters::openshell::{OpenShellAdapter, OpenShellTiming};

use crate::support::{FakeRunner, fixed, fixtures, fx, ok, rc, with};

pub const GW: [&str; 3] = ["openshell", "-g", "openshell"];

pub fn live_sandbox_list() -> String {
    fx("openshell", "sandbox-list.json")
}

/// The mutable "machine" behind the fake runner: tests change it between polls.
#[derive(Clone)]
pub struct World {
    pub inspect_out: String,
    pub inspect_err: String,
    pub inspect_rc: i32,
    pub internal: String,
    pub status_out: String,
    pub sandboxes: String,
    pub sandbox_rc: i32,
    pub cli_version: String,
    pub cli_rc: i32,
}

impl Default for World {
    fn default() -> Self {
        World {
            inspect_out: fx("openshell", "docker-inspect-running.txt"),
            inspect_err: String::new(),
            inspect_rc: 0,
            internal: "true".into(),
            status_out: fx("openshell", "status-connected.raw.txt"),
            sandboxes: "[]".into(),
            sandbox_rc: 0,
            cli_version: fx("openshell", "version.txt"),
            cli_rc: 0,
        }
    }
}

pub type SharedWorld = Arc<Mutex<World>>;

pub fn make_runner(w: Option<World>) -> (Arc<FakeRunner>, SharedWorld) {
    let w = Arc::new(Mutex::new(w.unwrap_or_default()));
    let r = FakeRunner::new();
    let w1 = Arc::clone(&w);
    r.on_prefix(
        &["openshell", "--version"],
        with(move |_| {
            let w = w1.lock().unwrap();
            rc(w.cli_rc, &w.cli_version, "")
        }),
    );
    let w2 = Arc::clone(&w);
    r.on_prefix(
        &["docker", "inspect", "--format"],
        with(move |_| {
            let w = w2.lock().unwrap();
            rc(w.inspect_rc, &w.inspect_out, &w.inspect_err)
        }),
    );
    let w3 = Arc::clone(&w);
    r.on_prefix(
        &["docker", "network", "inspect"],
        with(move |_| {
            let w = w3.lock().unwrap();
            if w.internal == "true" || w.internal == "false" {
                ok(&format!("{}\n", w.internal))
            } else {
                rc(
                    1,
                    "",
                    "Error response from daemon: network openshell-docker not found",
                )
            }
        }),
    );
    let w4 = Arc::clone(&w);
    r.on_prefix(
        &[GW[0], GW[1], GW[2], "status"],
        with(move |_| ok(&w4.lock().unwrap().status_out)),
    );
    let w5 = Arc::clone(&w);
    r.on_prefix(
        &[GW[0], GW[1], GW[2], "sandbox", "list"],
        with(move |_| {
            let w = w5.lock().unwrap();
            rc(
                w.sandbox_rc,
                &w.sandboxes,
                if w.sandbox_rc == 0 {
                    ""
                } else {
                    "transport error"
                },
            )
        }),
    );
    r.on_prefix(
        &[GW[0], GW[1], GW[2], "provider", "list"],
        fixed(ok(&fx("openshell", "provider-list.json"))),
    );
    r.on_prefix(
        &["openshell", "doctor", "check"],
        fixed(ok(&fx("openshell", "doctor-check.txt"))),
    );
    (r, w)
}

/// A copy of the lab's runner files (gateway.toml, policy.yaml, profile, compose) under tmp.
pub fn runner_dir(tmp: &Path) -> PathBuf {
    let rd = tmp.join("runner");
    if rd.is_dir() {
        return rd;
    }
    std::fs::create_dir_all(rd.join("config")).unwrap();
    for rel in [
        "config/gateway.toml",
        "policy.yaml",
        "profile-lab-router.yaml",
        "docker-compose.yml",
    ] {
        std::fs::copy(fixtures("openshell").join("runner").join(rel), rd.join(rel)).unwrap();
    }
    rd
}

pub struct OsEnv {
    pub tmp: tempfile::TempDir,
}

impl OsEnv {
    pub fn new() -> OsEnv {
        OsEnv {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    pub fn rd(&self) -> PathBuf {
        runner_dir(self.tmp.path())
    }

    /// `make_adapter(world, with_paths)` (follow and poll intervals 0.01 s).
    pub fn make(
        &self,
        world: Option<World>,
        with_paths: bool,
    ) -> (Arc<OpenShellAdapter>, Arc<FakeRunner>, SharedWorld) {
        let (runner, w) = make_runner(world);
        let a = self.adapter(Arc::clone(&runner), with_paths);
        (a, runner, w)
    }

    pub fn adapter(&self, runner: Arc<FakeRunner>, with_paths: bool) -> Arc<OpenShellAdapter> {
        let mut owned: Vec<(&str, PathBuf)> = Vec::new();
        if with_paths {
            let rd = self.rd();
            owned = vec![
                ("openshell_runner_dir", rd.clone()),
                (
                    "openshell_gateway_config",
                    rd.join("config").join("gateway.toml"),
                ),
                ("openshell_policy", rd.join("policy.yaml")),
                ("openshell_compose", rd.join("docker-compose.yml")),
            ];
        }
        let paths: Vec<(&str, &Path)> = owned.iter().map(|(k, v)| (*k, v.as_path())).collect();
        let ctx = crate::support::ctx(runner, &[], &paths, &self.tmp.path().join("state"));
        let a = OpenShellAdapter::new(ctx);
        a.set_timing(OpenShellTiming {
            follow_interval: 0.01,
            poll_interval: 0.01,
            ..OpenShellTiming::default()
        });
        Arc::new(a)
    }
}
