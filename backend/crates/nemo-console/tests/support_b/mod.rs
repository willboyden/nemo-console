//! Helpers for the ported core tests of fork B (capable tier/probe, dynamo policy, followups): a per-argv runner,
//! an adapter wrapper with async hooks, and an app-state rebuild with another runner/settings/adapters.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    unreachable_pub
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use nemo_console::adapters::base::{
    AdapterError, CommandResult, CommandRunner, EventStream, RunOpts, RunnerError, SystemAdapter,
};
use nemo_console::config::Settings;
use nemo_console::container::{AppState, Overrides, build_container};
use nemo_console::domain::models::*;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use serde_json::{Map, Value};

use crate::common::{Env, FakeAdapter};

type RunHook = dyn Fn(&[String]) -> Result<CommandResult, RunnerError> + Send + Sync;

/// A runner that answers per argv (Python tests replace `env.runner.run`).
pub struct HookRunner {
    pub calls: Mutex<Vec<Vec<String>>>,
    hook: Box<RunHook>,
}

impl HookRunner {
    pub fn new(
        hook: impl Fn(&[String]) -> Result<CommandResult, RunnerError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(HookRunner {
            calls: Mutex::new(Vec::new()),
            hook: Box::new(hook),
        })
    }

    /// `docker inspect` answers by container (the last argv element); "running" for any other.
    pub fn states(states: &[(&str, &str)]) -> Arc<Self> {
        let map: HashMap<String, String> = states
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        HookRunner::new(move |argv| {
            let st = map
                .get(argv.last().map_or("", String::as_str))
                .map_or("running", String::as_str);
            Ok(CommandResult::new(0, &format!("{st}\n"), ""))
        })
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl CommandRunner for HookRunner {
    async fn run(&self, argv: &[String], _opts: RunOpts) -> Result<CommandResult, RunnerError> {
        self.calls.lock().unwrap().push(argv.to_vec());
        (self.hook)(argv)
    }
}

/// Build an app state like `Env::state_for`, with any runner, settings and adapter set (Python
/// `build_container(cfg, adapters=..., runner=..., http=...)`).
pub fn state_with(
    env: &Env,
    settings: &Settings,
    adapters: HashMap<SystemId, Arc<dyn SystemAdapter>>,
    runner: Arc<dyn CommandRunner>,
    environ: &Environ,
) -> Arc<AppState> {
    build_container(
        settings.clone(),
        Overrides {
            adapters,
            runner: Some(runner),
            http: Some(env.http.clone()),
            registry: None,
            environ: Some(environ.clone()),
        },
    )
    .unwrap()
}

/// The env's fake adapters as trait objects.
pub fn fakes(env: &Env) -> HashMap<SystemId, Arc<dyn SystemAdapter>> {
    env.adapters
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v) as Arc<dyn SystemAdapter>))
        .collect()
}

/// Rebuild `env.state` with another runner (fresh caches, same fakes, settings and environment).
pub fn use_runner(env: &mut Env, runner: Arc<dyn CommandRunner>) {
    let adapters = fakes(env);
    let environ = env.environ.clone();
    let settings = env.settings.clone();
    env.state = state_with(env, &settings, adapters, runner, &environ);
}

type ActionHook =
    dyn Fn(String) -> BoxFuture<'static, Result<ActionResult, AdapterError>> + Send + Sync;
type PreviewHook =
    dyn Fn(Map<String, Value>) -> BoxFuture<'static, Result<String, AdapterError>> + Send + Sync;
type WarnHook = dyn Fn(&Map<String, Value>) -> Result<Vec<String>, AdapterError> + Send + Sync;
type ReadHook = dyn Fn(&FakeAdapter) -> Result<ConfigFile, AdapterError> + Send + Sync;

/// A FakeAdapter with some methods replaced (Python tests assign methods on the fake instance).
pub struct HookAdapter {
    pub inner: Arc<FakeAdapter>,
    pub run_action: Option<Box<ActionHook>>,
    pub preview: Option<Box<PreviewHook>>,
    pub warnings: Option<Box<WarnHook>>,
    pub read_config: Option<Box<ReadHook>>,
}

impl HookAdapter {
    pub fn new(inner: Arc<FakeAdapter>) -> Self {
        HookAdapter {
            inner,
            run_action: None,
            preview: None,
            warnings: None,
            read_config: None,
        }
    }
}

#[async_trait]
impl SystemAdapter for HookAdapter {
    fn id(&self) -> SystemId {
        self.inner.id()
    }
    fn label(&self) -> String {
        self.inner.label()
    }
    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        self.inner.status().await
    }
    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        self.inner.metrics().await
    }
    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        self.inner.recent_events(limit).await
    }
    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        Arc::clone(&self.inner).follow_events(since_ts)
    }
    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        self.inner.config_files()
    }
    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        self.inner.config_path(file_id)
    }
    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        match &self.read_config {
            Some(h) => h(&self.inner),
            None => self.inner.read_config(file_id).await,
        }
    }
    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        self.inner.validate_config(file_id, content).await
    }
    async fn post_apply(&self, file_id: &str, restart: bool) -> Result<ApplyResult, AdapterError> {
        self.inner.post_apply(file_id, restart).await
    }
    fn param_catalog(&self) -> Result<Vec<ParamSpec>, AdapterError> {
        self.inner.param_catalog()
    }
    async fn preview_params(&self, values: &Map<String, Value>) -> Result<String, AdapterError> {
        match &self.preview {
            Some(h) => h(values.clone()).await,
            None => self.inner.preview_params(values).await,
        }
    }
    async fn apply_params(&self, values: &Map<String, Value>) -> Result<ApplyResult, AdapterError> {
        self.inner.apply_params(values).await
    }
    fn param_warnings(
        &self,
        values: &Map<String, Value>,
    ) -> Option<Result<Vec<String>, AdapterError>> {
        match &self.warnings {
            Some(h) => Some(h(values)),
            None => self.inner.param_warnings(values),
        }
    }
    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        self.inner.actions()
    }
    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        match &self.run_action {
            Some(h) => h(action_id.to_owned()).await,
            None => self.inner.run_action(action_id, params).await,
        }
    }
    fn playground(&self) -> Result<Option<PlaygroundSpec>, AdapterError> {
        self.inner.playground()
    }
    fn playground_url(&self) -> Option<String> {
        self.inner.playground_url()
    }
}

/// Http client for tests.
pub fn http() -> Http {
    Http::new().unwrap()
}
