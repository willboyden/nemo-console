//! The guardrails test environment (Python tests/guardrails/gr_helpers.py + conftest.py): a prefix-matching fake runner
//! answering with the live deployment's captured docker output, a config tree copied from the REAL lab config plus the
//! awkward files, and a fake guardrails server (wiremock, ephemeral port).
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nemo_console::adapters::guardrails::{GuardrailsAdapter, GuardrailsTiming, PROBES};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::support::{CLOSED, FakeRunner, fixed, fixtures, fx, ok, with};

pub const CONTAINER: &str = "ai-lab-nemo-guardrails";

/// The live deployment as measured: all three containers running, the server healthy on an internal network.
pub fn standard_runner(isolation: Option<&str>, internal: Option<&str>) -> Arc<FakeRunner> {
    let iso = isolation.map_or_else(|| fx("guardrails", "inspect_isolation.txt"), str::to_owned);
    let r = FakeRunner::new();
    r.on_prefix(
        &["docker", "inspect", "--format"],
        with(move |argv| {
            if argv[3].starts_with("{{range") {
                return ok(&iso); // the network/port isolation probe
            }
            ok(&if argv.last().unwrap() == CONTAINER {
                fx("guardrails", "inspect_server.txt")
            } else {
                fx("guardrails", "inspect_gw.txt")
            })
        }),
    );
    r.on_prefix(
        &["docker", "network", "inspect"],
        fixed(ok(&internal.map_or_else(
            || fx("guardrails", "network_internal.txt"),
            str::to_owned,
        ))),
    );
    r.on_prefix(
        &["guardrails-local", "--version"],
        fixed(ok(&fx("guardrails", "version.txt"))),
    );
    r.on_prefix(
        &["guardrails-local", "validate"],
        fixed(ok(&fx("guardrails", "validate_ok.stdout"))),
    );
    r
}

/// security/nemo-guardrails/config with the real lab-input-output config plus the awkward files.
pub fn build_tree(tmp: &Path) -> PathBuf {
    let root = tmp.join("repo").join("security").join("nemo-guardrails");
    let cfg = root.join("config");
    if cfg.is_dir() {
        return root;
    }
    crate::support::copy_tree(&fixtures("guardrails").join("config-tree"), &cfg);
    std::fs::write(
        root.join("docker-compose.yml"),
        "services:\n  guardrails:\n    command: [\"server\", \"--config\", \"/config\", \"--default-config-id\", \"lab-input-output\", \"--port\", \"8000\"]\n",
    )
    .unwrap();
    let c = cfg.join("lab-input-output");
    std::fs::write(c.join("actions.py"), "def hello():\n    return 1\n").unwrap();
    std::fs::write(c.join("Notes.md"), "upper-case names are not valid ids\n").unwrap();
    std::fs::create_dir(c.join("kb")).unwrap();
    std::fs::write(c.join("kb").join("faq.md"), "# faq\n").unwrap();
    let outside = tmp.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.yml"), "x: 1\n").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.yml"), c.join("linked.yml")).unwrap();
    std::fs::create_dir(cfg.join("no-config")).unwrap();
    std::fs::write(
        cfg.join("no-config").join("README.md"),
        "no config.yml here\n",
    )
    .unwrap();
    std::fs::create_dir(cfg.join("_hidden")).unwrap();
    std::fs::write(cfg.join("_hidden").join("config.yml"), "models: []\n").unwrap();
    root
}

/// A settable (status, body) answer.
#[derive(Clone)]
pub struct Fixed(pub Arc<Mutex<(u16, String)>>);

impl Fixed {
    pub fn new(status: u16, body: &str) -> Fixed {
        Fixed(Arc::new(Mutex::new((status, body.to_owned()))))
    }
    pub fn set(&self, status: u16, body: &str) {
        *self.0.lock().unwrap() = (status, body.to_owned());
    }
}

impl Respond for Fixed {
    fn respond(&self, _r: &Request) -> ResponseTemplate {
        let g = self.0.lock().unwrap();
        ResponseTemplate::new(g.0).set_body_string(g.1.clone())
    }
}

/// `_checks_responder`: answers each probe with the verdict wanted for it and records the request bodies; or a fixed
/// error answer.
#[derive(Clone, Default)]
pub struct Checks {
    pub bodies: Arc<Mutex<Vec<Value>>>,
    pub verdicts: Arc<Mutex<Vec<(String, String)>>>,
    pub fixed: Arc<Mutex<Option<(u16, String)>>>,
}

impl Checks {
    pub fn verdicts(&self, v: &[(&str, &str)]) {
        *self.verdicts.lock().unwrap() = v
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect();
    }
}

impl Respond for Checks {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        if let Some((s, b)) = self.fixed.lock().unwrap().clone() {
            return ResponseTemplate::new(s).set_body_string(b);
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        self.bodies.lock().unwrap().push(body.clone());
        let text = body["messages"][0]["content"].as_str().unwrap().to_owned();
        let name = PROBES.iter().find(|(_, p, _)| *p == text).unwrap().0;
        let status = self
            .verdicts
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1
            .clone();
        let src = if status == "passed" {
            fx("guardrails", "checks-passed.json")
        } else {
            fx("guardrails", "checks-blocked.json")
        };
        let out = src
            .replace("\"passed\"", &format!("\"{status}\""))
            .replace("\"blocked\"", &format!("\"{status}\""));
        ResponseTemplate::new(200).set_body_string(out)
    }
}

pub struct GrEnv {
    pub tmp: tempfile::TempDir,
    pub server: MockServer,
    pub health: Fixed,
    pub configs: Fixed,
    pub checks: Checks,
    pub root: PathBuf,
}

impl GrEnv {
    pub async fn new() -> GrEnv {
        let tmp = tempfile::tempdir().unwrap();
        let root = build_tree(tmp.path());
        let server = MockServer::start().await;
        let health = Fixed::new(200, &fx("guardrails", "health.json"));
        let configs = Fixed::new(200, &fx("guardrails", "configs.json"));
        let checks = Checks::default();
        Mock::given(method("GET"))
            .and(path("/v1/health"))
            .respond_with(health.clone())
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/rails/configs"))
            .respond_with(configs.clone())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/checks"))
            .respond_with(checks.clone())
            .mount(&server)
            .await;
        GrEnv {
            tmp,
            server,
            health,
            configs,
            checks,
            root,
        }
    }

    pub fn tmp(&self) -> &Path {
        self.tmp.path()
    }

    /// `make_adapter(runner)` (poll budget 0.3 s, poll 0.02 s, follow 0.01 s); `down`: the server refuses
    /// connections (Python `side_effect=httpx.ConnectError`).
    pub fn make_with(
        &self,
        runner: Option<Arc<FakeRunner>>,
        down: bool,
    ) -> (Arc<GuardrailsAdapter>, Arc<FakeRunner>) {
        let run = runner.unwrap_or_else(|| standard_runner(None, None));
        let base = if down {
            CLOSED.to_owned()
        } else {
            self.server.uri()
        };
        let compose = self.root.join("docker-compose.yml");
        let cfgdir = self.root.join("config");
        let ctx = crate::support::ctx(
            Arc::clone(&run),
            &[("guardrails", &base)],
            &[
                ("guardrails_dir", &self.root),
                ("guardrails_compose", &compose),
                ("guardrails_config_dir", &cfgdir),
            ],
            &self.tmp().join("state"),
        );
        let a = GuardrailsAdapter::new(ctx);
        a.set_timing(GuardrailsTiming {
            poll_seconds: 0.3,
            poll_interval: 0.02,
            follow_interval: 0.01,
        });
        (Arc::new(a), run)
    }

    pub fn make(
        &self,
        runner: Option<Arc<FakeRunner>>,
    ) -> (Arc<GuardrailsAdapter>, Arc<FakeRunner>) {
        self.make_with(runner, false)
    }

    pub async fn post_count(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .count()
    }
}
