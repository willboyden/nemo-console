//! Shared fakes for the core tests: adapters, runner, http. Port of `backend/tests/core/conftest.py`.
//! Everything offline: no real docker, network, or user files. Owned by core (workstreams may use it, never change
//! it; PORTING.md section 6.9).
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::missing_panics_doc,
    unreachable_pub
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use nemo_console::adapters::base::{
    AdapterError, CommandResult, CommandRunner, EventStream, RunOpts, RunnerError, SystemAdapter,
};
use nemo_console::config::{Settings, settings_for_test};
use nemo_console::container::{AppState, Overrides, build_container};
use nemo_console::domain::models::*;
use nemo_console::services::common::sha256_hex;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Map, Value, json};

pub const FAKE_SECRET: &str = "sk-FAKEFAKEFAKE1234567890abcdef";
pub const BASE_HOST: &str = "127.0.0.1:8793";

/// Records every argv; answers with `result`.
pub struct FakeRunner {
    pub calls: Mutex<Vec<Vec<String>>>,
    pub result: Mutex<Result<CommandResult, RunnerError>>,
}

impl Default for FakeRunner {
    fn default() -> Self {
        FakeRunner {
            calls: Mutex::new(Vec::new()),
            result: Mutex::new(Ok(CommandResult::new(0, "running\n", ""))),
        }
    }
}

#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(&self, argv: &[String], _opts: RunOpts) -> Result<CommandResult, RunnerError> {
        self.calls.lock().unwrap().push(argv.to_vec());
        self.result.lock().unwrap().clone()
    }
}

impl FakeRunner {
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }

    pub fn set(&self, r: Result<CommandResult, RunnerError>) {
        *self.result.lock().unwrap() = r;
    }
}

type ValidateHook =
    Arc<dyn Fn(&str, &str) -> futures::future::BoxFuture<'static, ValidationResult> + Send + Sync>;
type RunActionHook = Arc<dyn Fn(&str) -> Result<ActionResult, AdapterError> + Send + Sync>;

/// Knobs of the fake adapter (Python attributes set by the tests).
#[derive(Default)]
pub struct Knobs {
    pub health: Option<Health>,
    pub events: Vec<SystemEvent>,
    pub follow: Vec<SystemEvent>,
    pub follow_block: bool,
    pub status_error: Option<AdapterError>,
    pub not_editable: bool,
    pub action_output: Option<String>,
    pub post_health_raise: bool,
    pub no_playground: bool,
    pub empty_catalog: bool,
    pub validate_hook: Option<ValidateHook>,
    pub run_action_hook: Option<RunActionHook>,
    pub warnings: Option<Result<Vec<String>, AdapterError>>,
    pub preview_error: Option<AdapterError>,
    pub playground_url: Option<String>,
    /// `metrics()` panics (with a credential in the message): the poller must contain it (audit finding 3).
    pub metrics_panic: bool,
}

pub struct FakeAdapter {
    pub system: SystemId,
    pub cfg_file: Option<PathBuf>,
    pub k: Mutex<Knobs>,
    pub post_calls: Mutex<Vec<bool>>,
    pub actions_run: Mutex<Vec<(String, Map<String, Value>)>>,
    pub params_applied: Mutex<Vec<Map<String, Value>>>,
}

impl FakeAdapter {
    pub fn new(system: SystemId, cfg_file: Option<PathBuf>) -> Arc<Self> {
        Arc::new(FakeAdapter {
            system,
            cfg_file,
            k: Mutex::new(Knobs::default()),
            post_calls: Mutex::new(Vec::new()),
            actions_run: Mutex::new(Vec::new()),
            params_applied: Mutex::new(Vec::new()),
        })
    }

    pub fn knobs(&self) -> std::sync::MutexGuard<'_, Knobs> {
        self.k.lock().unwrap()
    }
}

#[async_trait]
impl SystemAdapter for FakeAdapter {
    fn id(&self) -> SystemId {
        self.system
    }

    fn label(&self) -> String {
        "Fake".to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        let k = self.knobs();
        if let Some(e) = &k.status_error {
            return Err(e.clone());
        }
        Ok(SystemStatus {
            system: self.system,
            health: k.health.unwrap_or(Health::Up),
            version: None,
            summary: "fake".to_owned(),
            endpoints: vec![],
            components: vec![],
            problems: vec![Problem::new(
                Severity::Warn,
                "fake",
                "Bearer abcdefgh12345678 leaked",
                None,
            )],
            checked_at: now_ts(),
        })
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        #[allow(clippy::panic)]
        if self.knobs().metrics_panic {
            panic!("metrics blew up near Authorization: Bearer sk-panicsecret-0123456789");
        }
        let mut p1 = MetricPoint::new("fake.tokens", 2.0);
        p1.labels.insert("kind".to_owned(), "prompt".to_owned());
        let mut p2 = MetricPoint::new("fake.tokens", 3.0);
        p2.labels.insert("kind".to_owned(), "completion".to_owned());
        Ok(MetricsSnapshot {
            system: self.system,
            ts: now_ts(),
            available: true,
            points: vec![MetricPoint::new("fake.requests_total", 5.0), p1, p2],
            error: None,
        })
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        Ok(self.knobs().events.iter().take(limit).cloned().collect())
    }

    fn follow_events(self: Arc<Self>, _since_ts: Option<f64>) -> EventStream {
        let (evs, block) = {
            let k = self.knobs();
            (k.follow.clone(), k.follow_block)
        };
        Box::pin(async_stream::stream! {
            for e in evs {
                yield Ok(e);
            }
            if block {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        })
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        if self.cfg_file.is_none() {
            return Ok(vec![]);
        }
        let editable = !self.knobs().not_editable;
        Ok(vec![ConfigFileSpec {
            id: "routes".to_owned(),
            label: "Routes".to_owned(),
            description: "d".to_owned(),
            display_path: "routes.toml".to_owned(),
            format: "toml".to_owned(),
            editable,
            why_readonly: if editable {
                None
            } else {
                Some("policy file".to_owned())
            },
            validators: vec!["fake".to_owned()],
            authoritative: true,
            apply_effect: String::new(),
        }])
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        match &self.cfg_file {
            Some(p) if file_id == "routes" => Ok(p.clone()),
            _ => Err(AdapterError::UnknownId(file_id.to_owned())),
        }
    }

    async fn read_config(&self, _file_id: &str) -> Result<ConfigFile, AdapterError> {
        let p = self.cfg_file.clone().unwrap();
        let data = std::fs::read(&p).map_err(|e| AdapterError::FileNotFound(e.to_string()))?;
        Ok(ConfigFile {
            spec: self.config_files()?.remove(0),
            content: String::from_utf8_lossy(&data).into_owned(),
            sha256: "adapter-sha-ignored".to_owned(),
            mtime: 0.0,
        })
    }

    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        let hook = self.knobs().validate_hook.clone();
        if let Some(h) = hook {
            return Ok(h(file_id, content).await);
        }
        let bad = content.contains("INVALID");
        Ok(ValidationResult {
            ok: !bad,
            validator: "fake".to_owned(),
            authoritative: true,
            diagnostics: if bad {
                vec![Diagnostic {
                    severity: Severity::Error,
                    message: "nope".to_owned(),
                    line: None,
                }]
            } else {
                vec![]
            },
            note: None,
        })
    }

    async fn post_apply(&self, _file_id: &str, restart: bool) -> Result<ApplyResult, AdapterError> {
        self.post_calls.lock().unwrap().push(restart);
        let unhealthy = std::fs::read_to_string(self.cfg_file.as_ref().unwrap())
            .unwrap_or_default()
            .contains("BAD");
        if self.knobs().post_health_raise {
            return Err(AdapterError::failed("RuntimeError", "boom"));
        }
        let mut r = ApplyResult::new(!unhealthy, "fake post_apply");
        r.restarted = restart;
        r.post_check = if unhealthy {
            PostCheck::Unhealthy
        } else {
            PostCheck::Healthy
        };
        Ok(r)
    }

    fn param_catalog(&self) -> Result<Vec<ParamSpec>, AdapterError> {
        if self.knobs().empty_catalog {
            return Ok(vec![]);
        }
        let mut gpu = ParamSpec::new("gpu_util", "GPU util", "h", ParamType::Float);
        gpu.min = Some(0.1);
        gpu.max = Some(0.95);
        let mut mode = ParamSpec::new("mode", "Mode", "h", ParamType::Enum);
        mode.choices = Some(vec!["a".to_owned(), "b".to_owned()]);
        let mut n = ParamSpec::new("n", "N", "h", ParamType::Int);
        n.min = Some(1.0);
        n.max = Some(10.0);
        Ok(vec![gpu, mode, n])
    }

    async fn preview_params(&self, values: &Map<String, Value>) -> Result<String, AdapterError> {
        if let Some(e) = self.knobs().preview_error.clone() {
            return Err(e);
        }
        Ok(format!("rendered {}", Value::Object(values.clone())))
    }

    async fn apply_params(&self, values: &Map<String, Value>) -> Result<ApplyResult, AdapterError> {
        self.params_applied.lock().unwrap().push(values.clone());
        Ok(ApplyResult::new(true, "applied"))
    }

    fn param_warnings(
        &self,
        _values: &Map<String, Value>,
    ) -> Option<Result<Vec<String>, AdapterError>> {
        self.knobs().warnings.clone()
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let mut status = ActionSpec::new("status", "Status", "d", Danger::Safe);
        status.role = Role::Viewer;
        let safe_admin = ActionSpec::new("safe_admin", "Safe admin", "d", Danger::Safe);
        let mut restart = ActionSpec::new("restart", "Restart", "d", Danger::Disruptive);
        let mut t = ParamSpec::new("timeout", "t", "h", ParamType::Int);
        t.min = Some(1.0);
        t.max = Some(60.0);
        restart.params = vec![t];
        let reset = ActionSpec::new("reset", "Reset", "d", Danger::Destructive);
        Ok(vec![status, safe_admin, restart, reset])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let hook = self.knobs().run_action_hook.clone();
        if let Some(h) = hook {
            return h(action_id);
        }
        if !self.actions()?.iter().any(|a| a.id == action_id) {
            return Err(AdapterError::UnknownId(action_id.to_owned()));
        }
        self.actions_run
            .lock()
            .unwrap()
            .push((action_id.to_owned(), params.clone()));
        let out = self
            .knobs()
            .action_output
            .clone()
            .unwrap_or_else(|| "done".to_owned());
        Ok(ActionResult::new(true, out))
    }

    fn playground(&self) -> Result<Option<PlaygroundSpec>, AdapterError> {
        if self.knobs().no_playground {
            return Ok(None);
        }
        Ok(Some(PlaygroundSpec {
            wire: "openai_chat".to_owned(),
            models: vec!["m1".to_owned()],
            default_model: "m1".to_owned(),
            note: String::new(),
        }))
    }

    fn playground_url(&self) -> Option<String> {
        // Python's conftest points at :4010 and respx intercepts it; here the default is a CLOSED port so a test can
        // never reach a live Switchyard. Tests that need an upstream set `playground_url` to a wiremock server.
        Some(
            self.knobs()
                .playground_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:9/v1/chat/completions".to_owned()),
        )
    }
}

/// One test environment (Python `Env`): temp dirs, settings, fakes, the built app state.
pub struct Env {
    pub environ: Environ,
    pub tmp: tempfile::TempDir,
    pub cfg_file: PathBuf,
    pub settings: Settings,
    pub runner: Arc<FakeRunner>,
    pub http: Http,
    pub adapters: HashMap<SystemId, Arc<FakeAdapter>>,
    pub state: Arc<AppState>,
}

impl Env {
    /// Python's `env` fixture. `litellm`: base URL of a mock router (the topology probe calls it).
    pub fn new(litellm: Option<&str>) -> Env {
        Env::with_settings(litellm, json!({}))
    }

    pub fn with_settings(litellm: Option<&str>, extra: Value) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_file = tmp.path().join("cfg").join("routes.toml");
        std::fs::create_dir(cfg_file.parent().unwrap()).unwrap();
        std::fs::write(&cfg_file, "a = 1\n").unwrap();
        set_mode(&cfg_file, 0o640);
        let mut init = json!({
            "state_dir": tmp.path().join("state").to_string_lossy(),
            "frontend_dir": tmp.path().join("front").to_string_lossy(),
            "repo_root": tmp.path().join("repo").to_string_lossy(),
            "background_tasks": false, "trust_loopback": true, "sse_max_connections": 2,
            "sse_idle_timeout_s": 0.3, "sse_heartbeat_s": 0.1,
        });
        // every upstream defaults to a CLOSED port: a test can never reach a live lab service (the defaults are the
        // real :4000/:4010/... of this host); tests that need one pass a wiremock URL
        let closed = "http://127.0.0.1:9";
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
            urls.insert(k.to_owned(), Value::String(closed.to_owned()));
        }
        if let Some(u) = litellm {
            urls.insert("litellm".to_owned(), Value::String(u.to_owned()));
        }
        init["urls"] = Value::Object(urls);
        if let (Value::Object(base), Value::Object(more)) = (&mut init, extra) {
            for (k, v) in more {
                base.insert(k, v);
            }
        }
        let settings = settings_for_test(init);
        Env::build(tmp, cfg_file, settings, Environ::fixed(HashMap::new()))
    }

    /// Same environment with these process variables (Python `monkeypatch.setenv`); rebuilds the app state.
    pub fn with_env(mut self, pairs: &[(&str, &str)]) -> Env {
        self.environ = Environ::from_pairs(pairs);
        self.state = Env::state_for(
            &self.settings,
            &self.adapters,
            &self.runner,
            &self.http,
            &self.environ,
        );
        self
    }

    pub fn build(
        tmp: tempfile::TempDir,
        cfg_file: PathBuf,
        settings: Settings,
        environ: Environ,
    ) -> Env {
        let runner = Arc::new(FakeRunner::default());
        let http = Http::new().unwrap();
        let mut adapters = HashMap::new();
        for sid in SystemId::ALL {
            let f = if sid == SystemId::Switchyard {
                Some(cfg_file.clone())
            } else {
                None
            };
            adapters.insert(sid, FakeAdapter::new(sid, f));
        }
        let state = Env::state_for(&settings, &adapters, &runner, &http, &environ);
        Env {
            environ,
            tmp,
            cfg_file,
            settings,
            runner,
            http,
            adapters,
            state,
        }
    }

    pub fn state_for(
        settings: &Settings,
        adapters: &HashMap<SystemId, Arc<FakeAdapter>>,
        runner: &Arc<FakeRunner>,
        http: &Http,
        environ: &Environ,
    ) -> Arc<AppState> {
        let ov = Overrides {
            adapters: adapters
                .iter()
                .map(|(k, v)| (*k, Arc::clone(v) as Arc<dyn SystemAdapter>))
                .collect(),
            runner: Some(Arc::clone(runner) as Arc<dyn CommandRunner>),
            http: Some(http.clone()),
            registry: None,
            environ: Some(environ.clone()),
        };
        build_container(settings.clone(), ov).unwrap()
    }

    pub fn a(&self, sid: SystemId) -> &Arc<FakeAdapter> {
        &self.adapters[&sid]
    }

    pub fn sha(&self) -> String {
        sha256_hex(&std::fs::read(&self.cfg_file).unwrap())
    }

    pub fn tmp(&self) -> &Path {
        self.tmp.path()
    }

    /// A request builder with Python's defaults: client 127.0.0.1, `X-Console: 1`, Host 127.0.0.1:8793.
    pub fn req(&self, method: &str, path: &str) -> TestReq {
        TestReq::new(Arc::clone(&self.state), method, path)
    }

    pub fn get(&self, path: &str) -> TestReq {
        self.req("GET", path)
    }

    pub fn post(&self, path: &str, body: Value) -> TestReq {
        self.req("POST", path).json(body)
    }

    pub fn put(&self, path: &str, body: Value) -> TestReq {
        self.req("PUT", path).json(body)
    }
}

pub fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

pub fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

pub fn apply_body(content: &str, base: &str) -> Value {
    json!({"content": content, "base_sha256": base, "restart": true, "confirm": true})
}

pub struct TestReq {
    state: Arc<AppState>,
    method: String,
    path: String,
    client: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    csrf: bool,
}

pub struct TestResp {
    pub status: u16,
    pub headers: axum::http::HeaderMap,
    pub body: Vec<u8>,
}

impl TestResp {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.text()))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }

    pub fn code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }
}

impl TestReq {
    pub fn new(state: Arc<AppState>, method: &str, path: &str) -> TestReq {
        TestReq {
            state,
            method: method.to_owned(),
            path: path.to_owned(),
            client: "127.0.0.1".to_owned(),
            headers: vec![("host".to_owned(), BASE_HOST.to_owned())],
            body: Vec::new(),
            csrf: true,
        }
    }

    pub fn client(mut self, host: &str) -> Self {
        host.clone_into(&mut self.client);
        self
    }

    pub fn token(self, key: &str) -> Self {
        self.header("authorization", &format!("Bearer {key}"))
    }

    pub fn no_csrf(mut self) -> Self {
        self.csrf = false;
        self
    }

    /// Set (replace) a header.
    pub fn header(mut self, k: &str, v: &str) -> Self {
        let k = k.to_ascii_lowercase();
        self.headers.retain(|(n, _)| *n != k);
        self.headers.push((k, v.to_owned()));
        self
    }

    pub fn json(mut self, v: Value) -> Self {
        self.body = serde_json::to_vec(&v).unwrap();
        self.header("content-type", "application/json")
    }

    pub fn raw(mut self, body: &[u8]) -> Self {
        self.body = body.to_vec();
        self
    }

    pub async fn send(self) -> TestResp {
        // like httpx: characters that are not legal in a URI are percent-encoded, `%` escapes are kept
        const URI_UNSAFE: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'<')
            .add(b'>')
            .add(b'`')
            .add(b'{')
            .add(b'}')
            .add(b'|')
            .add(b'\\')
            .add(b'^');
        let uri = percent_encoding::utf8_percent_encode(&self.path, URI_UNSAFE).to_string();
        let mut b = Request::builder()
            .method(self.method.as_str())
            .uri(uri.as_str());
        for (k, v) in &self.headers {
            b = b.header(k.as_str(), v.as_str());
        }
        if self.csrf {
            b = b.header("x-console", "1");
        }
        if !self.body.is_empty() {
            b = b.header("content-length", self.body.len().to_string());
        }
        let req = b.body(Body::from(self.body)).unwrap();
        let resp = nemo_console::api::handle(self.state, Some(self.client), req).await;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp
            .into_body()
            .collect()
            .await
            .map(|c| c.to_bytes().to_vec())
            .unwrap_or_default();
        TestResp {
            status,
            headers,
            body,
        }
    }
}

/// The five security headers every response must carry (Python `assert_secure`).
pub fn assert_secure(r: &TestResp) {
    for h in [
        "content-security-policy",
        "x-content-type-options",
        "referrer-policy",
        "x-frame-options",
        "permissions-policy",
    ] {
        assert!(r.headers.contains_key(h), "missing {h} on {}", r.status);
    }
    assert!(
        r.header("content-security-policy")
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    assert_eq!(r.header("x-content-type-options").unwrap(), "nosniff");
}

// ── fork A helpers (appended) ───────────────────────────────────────────────────────────────────

/// A wiremock "LiteLLM router" whose `/health/liveliness` answers 200 (Python: respx mock in the `env` fixture),
/// and an Env pointed at it. Every other URL stays at its default; no test may call a real service.
pub async fn env_with_router() -> (Env, wiremock::MockServer) {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/health/liveliness"))
        .respond_with(wiremock::ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let env = Env::new(Some(&server.uri()));
    (env, server)
}

/// Log capture for one test (Python `caplog`): every tracing event emitted on THIS thread while the guard lives.
#[derive(Clone, Default)]
pub struct LogCapture(pub Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogCapture {
    /// Start capturing (DEBUG and up) on the current thread; drop the guard to stop.
    pub fn start() -> (LogCapture, tracing::subscriber::DefaultGuard) {
        let cap = LogCapture::default();
        let writer = cap.clone();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let guard = tracing::subscriber::set_default(sub);
        (cap, guard)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}
