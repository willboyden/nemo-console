//! The DCGM test environment (Python tests/dcgm/conftest.py): a fake runner answering with the captured docker output,
//! a fake exporter and a fake Prometheus (wiremock, ephemeral ports) whose answers a test can change at any time, and
//! a counters.csv copied from the live file.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub
)]

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nemo_console::adapters::dcgm::{DcgmAdapter, DcgmTiming};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::support::{FakeRunner, fixed, fx, ok, starts};

pub const CONTAINER: &str = "ai-lab-observability-dcgm-exporter-1";
pub const IMG: &str = "nvcr.io/nvidia/cloud-native/dcgm:4.6.1-1-ubuntu24.04@sha256:48464e50aa76671f5b204264cb4999739591112bf74330a727523f5655c6d7f2";

pub fn is_inspect(a: &[String]) -> bool {
    starts(a, &["docker", "inspect"])
}
pub fn is_labels_inspect(a: &[String]) -> bool {
    is_inspect(a) && a.join(" ").contains("com.docker.compose.project")
}
pub fn is_image_inspect(a: &[String]) -> bool {
    starts(a, &["docker", "image", "inspect"])
}
pub fn is_restart(a: &[String]) -> bool {
    starts(a, &["docker", "restart"])
}
pub fn is_logs(a: &[String]) -> bool {
    starts(a, &["docker", "logs"])
}
pub fn is_run(a: &[String]) -> bool {
    starts(a, &["docker", "run"])
}
pub fn is_rm(a: &[String]) -> bool {
    starts(a, &["docker", "rm", "-f"])
}

/// The runner fixture of conftest.py.
pub fn standard_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on(
        is_labels_inspect,
        fixed(ok("ai-lab-observability|dcgm-exporter\n")),
    );
    r.on(is_inspect, fixed(ok(&fx("dcgm", "inspect.txt"))));
    r.on(is_image_inspect, fixed(ok("sha256:48464e50aa76\n")));
    r.on(is_restart, fixed(ok(&format!("{CONTAINER}\n"))));
    r.on(is_logs, fixed(ok("time=2026-10-24T11:13:31.052Z level=INFO msg=\"Starting dcgm-exporter\" Version=4.8.4\n")));
    r
}

/// A fixed answer, or a queue of answers consumed first (`(status, body)`).
/// (queued answers, the answer after the queue)
type AnswerState = (VecDeque<(u16, String)>, (u16, String));

#[derive(Clone)]
pub struct Answer(pub Arc<Mutex<AnswerState>>);

impl Answer {
    pub fn new(status: u16, body: &str) -> Answer {
        Answer(Arc::new(Mutex::new((
            VecDeque::new(),
            (status, body.to_owned()),
        ))))
    }
    pub fn set(&self, status: u16, body: &str) {
        let mut g = self.0.lock().unwrap();
        g.0.clear();
        g.1 = (status, body.to_owned());
    }
    pub fn queue(&self, items: &[(u16, &str)]) {
        let mut g = self.0.lock().unwrap();
        for (s, b) in items {
            g.0.push_back((*s, (*b).to_owned()));
        }
    }
}

impl Respond for Answer {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let mut g = self.0.lock().unwrap();
        let (s, b) = g.0.pop_front().unwrap_or_else(|| g.1.clone());
        ResponseTemplate::new(s).set_body_string(b)
    }
}

/// conftest.prom_handler: the query decides the fixture; overrides replace a fixture by needle.
#[derive(Clone, Default)]
pub struct Prom(pub Arc<Mutex<HashMap<String, String>>>);

pub const QUERY_FIXTURES: [(&str, &str); 4] = [
    ("up{", "prom_up.json"),
    ("count by", "prom_count.json"),
    ("max by", "prom_tempmax.json"),
    ("sum by", "prom_power.json"),
];

impl Prom {
    pub fn set(&self, needle: &str, body: &str) {
        self.0
            .lock()
            .unwrap()
            .insert(needle.to_owned(), body.to_owned());
    }
}

impl Respond for Prom {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let q: String = req
            .url
            .query_pairs()
            .find(|(k, _)| k == "query")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        let over = self.0.lock().unwrap();
        for (needle, name) in QUERY_FIXTURES {
            if q.contains(needle) {
                let body = over
                    .get(needle)
                    .cloned()
                    .unwrap_or_else(|| fx("dcgm", name));
                return ResponseTemplate::new(200).set_body_string(body);
            }
        }
        ResponseTemplate::new(400).set_body_string("unknown query")
    }
}

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub runner: Arc<FakeRunner>,
    pub server: MockServer,
    pub metrics: Answer,
    pub pprof: Answer,
    pub prom: Prom,
    pub counters: PathBuf,
}

impl Env {
    pub async fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("obs").join("dcgm");
        std::fs::create_dir_all(&d).unwrap();
        let counters = d.join("counters.csv");
        std::fs::write(&counters, fx("dcgm", "counters.good.csv")).unwrap();
        let server = MockServer::start().await;
        let metrics = Answer::new(200, &fx("dcgm", "metrics.txt"));
        let pprof = Answer::new(404, "404 page not found\n");
        let prom = Prom::default();
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(metrics.clone())
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/debug/pprof/"))
            .respond_with(pprof.clone())
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/query"))
            .respond_with(prom.clone())
            .mount(&server)
            .await;
        Env {
            tmp,
            runner: standard_runner(),
            server,
            metrics,
            pprof,
            prom,
            counters,
        }
    }

    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// `make_adapter(run=None)` with the test timings (follow/poll 0.01 s, no settle, 0.3 s apply budget).
    pub fn adapter(&self) -> Arc<DcgmAdapter> {
        self.adapter_with(Arc::clone(&self.runner), &self.url(), &self.url())
    }

    pub fn adapter_with(
        &self,
        runner: Arc<FakeRunner>,
        exporter: &str,
        prom: &str,
    ) -> Arc<DcgmAdapter> {
        let obs = self
            .counters
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let compose = obs.join("docker-compose.yml");
        let ctx = crate::support::ctx(
            runner,
            &[("dcgm", exporter), ("prometheus", prom)],
            &[
                ("dcgm_counters", &self.counters),
                ("observability_dir", &obs),
                ("observability_compose", &compose),
            ],
            &self.tmp.path().join("state"),
        );
        let a = DcgmAdapter::new(ctx);
        a.set_timing(DcgmTiming {
            follow_interval: 0.01,
            poll_interval: 0.01,
            apply_timeout: 0.3,
            settle: 0.0,
        });
        Arc::new(a)
    }

    pub fn state(&self) -> PathBuf {
        self.tmp.path().join("state")
    }
}

/// conftest.xid_metrics: the real scrape plus an XID series on GPU 1 (SYNTHETIC).
pub fn xid_metrics(code: i64, extra: &str) -> String {
    format!(
        "{}DCGM_FI_DEV_XID_ERRORS{{gpu=\"1\",UUID=\"GPU-00000000-0000-0000-0000-000000000001\",pci_bus_id=\"00000000:F1:00.0\",device=\"nvidia1\",modelName=\"NVIDIA RTX PRO 6000 Blackwell Workstation Edition\",hostname=\"exporter\",DCGM_FI_DRIVER_VERSION=\"595.99.02\"}} {code}\n{extra}",
        fx("dcgm", "metrics.txt")
    )
}

/// conftest.diag_doc: a level-2-shaped document built from the REAL level-1 fixture's structure (SYNTHETIC).
pub fn diag_doc(statuses: &[(&str, &str)], warning: Option<&str>) -> String {
    let mut d: serde_json::Value =
        serde_json::from_str(&fx("dcgm", "diag_level1_gpu0.json")).unwrap();
    let mut tests = Vec::new();
    for (name, st) in statuses {
        let mut res = serde_json::json!({"entity_group": "GPU", "entity_group_id": 1, "entity_id": 0, "status": st});
        if let Some(w) = warning
            && *st != "Pass"
        {
            res["warnings"] = serde_json::json!([{"warning": w}]);
        }
        tests.push(
            serde_json::json!({"name": name, "results": [res], "test_summary": {"status": st}}),
        );
    }
    d["DCGM Diagnostic"]["test_categories"] =
        serde_json::json!([{"category": "Hardware", "tests": tests}]);
    serde_json::to_string(&d).unwrap()
}
