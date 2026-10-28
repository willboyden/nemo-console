//! Composition root (`AppState`). Port of `container.py`. Contract (PORTING.md section 6.1): everything is built here so
//! tests can inject fake adapters / runner / http, exactly like Python's `build_container(cfg, adapters=, runner=,
//! http=)`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use tokio::sync::Semaphore;

use crate::adapters::base::{AdapterContext, AdapterRef, CommandRunner};
use crate::adapters::{BuildFn, SYSTEM_IDS, build_adapter};
use crate::config::Settings;
use crate::domain::models::{Health, Problem, Severity, SystemId, SystemStatus};
use crate::services::audit::{AuditLog, DenialLog};
use crate::services::auth::AuthService;
use crate::services::common::RecoverLock as _;
use crate::services::config_store::ConfigStore;
use crate::services::engines::{
    CapableProbe, EngineProbe, EngineState, capable_targets, capable_tier_problems, probe_problems,
};
use crate::services::environ::Environ;
use crate::services::events::{Shutdown, SseLimiter};
use crate::services::http::Http;
use crate::services::metrics::MetricsPoller;
use crate::services::missing::MissingAdapter;
use crate::services::runner::SubprocessRunner;
use crate::services::settings::SettingsService;
use crate::services::status::{Enricher, StatusService};
use crate::services::store::{Store, StoreError};
use crate::services::telemetry::{Telemetry, setup_otlp};
use crate::services::topology::TopologyService;

/// The app state every handler sees (Python `Container`).
pub struct AppState {
    pub cfg: Arc<Settings>,
    pub store: Arc<Store>,
    pub auth: AuthService,
    pub audit: AuditLog,
    pub runner: Arc<dyn CommandRunner>,
    pub http: Http,
    /// Registry order.
    pub adapters: IndexMap<SystemId, AdapterRef>,
    pub settings: SettingsService,
    pub status: Arc<StatusService>,
    pub metrics: Arc<MetricsPoller>,
    pub config_store: ConfigStore,
    pub topology: TopologyService,
    pub probe: Arc<EngineProbe>,
    pub sse: Arc<SseLimiter>,
    pub telemetry: Telemetry,
    pub denials: DenialLog,
    pub trust_notice: Option<String>,
    pub validate_sem: Arc<Semaphore>,
    pub capable_probe: Option<Arc<CapableProbe>>,
    pub shutdown: Shutdown,
    pub environ: Environ,
    busy: Mutex<HashMap<SystemId, Arc<tokio::sync::Mutex<()>>>>,
}

impl AppState {
    /// One mutating operation per system at a time (Python `c.lock(system)`).
    #[must_use]
    pub fn lock(&self, system: SystemId) -> Arc<tokio::sync::Mutex<()>> {
        let mut b = match self.busy.lock_recovering() {
            Ok(b) => b,
            Err(p) => p.into_inner(),
        };
        Arc::clone(b.entry(system).or_default())
    }

    /// The adapter of a registered system.
    #[must_use]
    pub fn adapter(&self, system: SystemId) -> Option<&AdapterRef> {
        self.adapters.get(&system)
    }
}

/// What tests may inject (Python keyword arguments of `build_container`).
#[derive(Default)]
pub struct Overrides {
    pub adapters: HashMap<SystemId, AdapterRef>,
    pub runner: Option<Arc<dyn CommandRunner>>,
    pub http: Option<Http>,
    /// Substitute registry (Python tests monkeypatch `_REGISTRY`).
    pub registry: Option<BuildFn>,
    /// Pinned environment (tests); `None` = the process environment.
    pub environ: Option<Environ>,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("http client: {0}")]
    Http(#[from] crate::services::http::HttpError),
}

/// `build_container(cfg, adapters=, runner=, http=)`.
pub fn build_container(cfg: Settings, ov: Overrides) -> Result<Arc<AppState>, BuildError> {
    let cfg = Arc::new(cfg);
    let store = Arc::new(Store::open(&cfg.state_dir)?);
    let (trust, trust_notice) = cfg.effective_loopback_trust();
    if let Some(n) = &trust_notice {
        tracing::warn!("{n}");
    }
    let environ = ov.environ.unwrap_or_else(Environ::process);
    let settings = SettingsService::new(Arc::clone(&cfg), Arc::clone(&store), environ.clone());
    let runner: Arc<dyn CommandRunner> = ov.runner.unwrap_or_else(|| {
        Arc::new(SubprocessRunner::with_environ(
            &cfg.extra_bin_dirs,
            environ.clone(),
        ))
    });
    let http = match ov.http {
        Some(h) => h,
        None => Http::new()?,
    };
    let urls = settings.effective_urls();
    let ctx = AdapterContext {
        runner: Arc::clone(&runner),
        http: http.clone(),
        urls: urls.clone(),
        paths: cfg.resolved_paths(),
        secret_env: settings.effective_key_env(),
        state_dir: cfg.state_dir.clone(),
        environ: environ.clone(),
    };
    let registry: BuildFn = ov.registry.unwrap_or(build_adapter);
    let mut built: HashMap<SystemId, AdapterRef> = ov.adapters;
    for sid in SYSTEM_IDS {
        if built.contains_key(&sid) {
            continue;
        }
        let a: AdapterRef = match registry(sid, &ctx, &cfg) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("adapter {sid} unavailable: {}", e.py_class());
                Arc::new(MissingAdapter {
                    system: sid,
                    reason: e.py_text(),
                })
            }
        };
        built.insert(sid, a);
    }
    let ordered: IndexMap<SystemId, AdapterRef> = SYSTEM_IDS
        .iter()
        .filter_map(|s| built.get(s).map(|a| (*s, Arc::clone(a))))
        .collect();
    let global_problems = match cfg.repo_root_problem() {
        Some(msg) => vec![Problem::new(
            Severity::Error,
            "console.repo_root.missing",
            msg,
            Some(
                "export NEMO_CONSOLE_REPO_ROOT=/path/to/lab-repo and restart nemo-console"
                    .to_owned(),
            ),
        )],
        None => Vec::new(),
    };
    let probe = Arc::new(EngineProbe::new(Arc::clone(&runner)));
    let litellm = urls
        .get("litellm")
        .cloned()
        .unwrap_or_else(|| "http://127.0.0.1:4000".to_owned());
    let active = if cfg.capable_probe {
        Some(Arc::new(CapableProbe::new(
            http.clone(),
            &litellm,
            &cfg.capable_probe_key_env,
            cfg.capable_probe_interval_s,
            cfg.capable_probe_timeout_s,
            cfg.capable_probe_retry_s,
            environ.clone(),
        )))
    } else {
        None
    };
    let validate_sem = Arc::new(Semaphore::new(2));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let config_store = ConfigStore::new(
        Arc::clone(&store),
        ordered.clone(),
        cfg.config_cap_bytes.max(0) as usize,
        cfg.revisions_keep,
        Arc::clone(&validate_sem),
    );
    let mut enrichers: HashMap<SystemId, Enricher> = HashMap::new();
    enrichers.insert(
        SystemId::Switchyard,
        switchyard_enricher(
            Arc::clone(&cfg),
            ordered.get(&SystemId::Switchyard).cloned(),
            Arc::clone(&probe),
            active.clone(),
        ),
    );
    let status = Arc::new(StatusService::new(
        ordered.clone(),
        cfg.status_ttl_s,
        cfg.adapter_timeout_s,
        global_problems,
        enrichers,
    ));
    setup_otlp(cfg.otlp_endpoint.as_deref());
    let metrics = Arc::new(MetricsPoller::new(
        Arc::clone(&status),
        Arc::clone(&store),
        settings.effective_poll_interval(),
        cfg.ring_seconds,
    ));
    let topology = TopologyService::new(
        Arc::clone(&status),
        http.clone(),
        Arc::clone(&probe),
        &litellm,
        &cfg.topology_engines,
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let sse = Arc::new(SseLimiter::new(cfg.sse_max_connections.max(0) as usize));
    Ok(Arc::new(AppState {
        auth: AuthService {
            store: Arc::clone(&store),
            state_dir: cfg.state_dir.clone(),
            trust_loopback: trust,
        },
        audit: AuditLog {
            store: Arc::clone(&store),
        },
        cfg,
        store,
        runner,
        http,
        adapters: ordered,
        settings,
        status,
        metrics,
        config_store,
        topology,
        probe,
        sse,
        telemetry: Telemetry::new(),
        denials: DenialLog::default(),
        trust_notice,
        validate_sem,
        capable_probe: active,
        shutdown: Shutdown::default(),
        environ,
        busy: Mutex::new(HashMap::new()),
    }))
}

/// The Switchyard status enricher: capable tier dark / not answering (Python `enrich_switchyard`).
fn switchyard_enricher(
    cfg: Arc<Settings>,
    adapter: Option<AdapterRef>,
    probe: Arc<EngineProbe>,
    active: Option<Arc<CapableProbe>>,
) -> Enricher {
    Arc::new(move |st: SystemStatus| {
        let cfg = Arc::clone(&cfg);
        let adapter = adapter.clone();
        let probe = Arc::clone(&probe);
        let active = active.clone();
        Box::pin(async move {
            let Some(a) = adapter else { return Ok(st) };
            let Ok(routes) = a.config_path("routes") else {
                return Ok(st);
            };
            #[allow(clippy::cast_sign_loss)]
            let cap = cfg.config_cap_bytes.max(0) as u64;
            let text = match std::fs::metadata(&routes) {
                Ok(md) if md.len() <= cap => match std::fs::read(&routes).map(String::from_utf8) {
                    Ok(Ok(t)) => t,
                    _ => return Ok(st),
                },
                Ok(_) => String::new(),
                Err(_) => return Ok(st),
            };
            let mut problems = capable_tier_problems(
                &probe,
                &cfg.engine_probes,
                &text,
                &cfg.repo_root,
                &cfg.engine_start,
            )
            .await;
            if let Some(active) = &active {
                let mut running = Vec::new();
                for t in capable_targets(&cfg.engine_probes, &text) {
                    if probe.state(&t.1).await == EngineState::Running {
                        running.push(t);
                    }
                }
                let aliases: Vec<String> = running.iter().map(|(a, _, _)| a.clone()).collect();
                active.schedule(&aliases);
                problems.extend(probe_problems(
                    &active.results(),
                    &running,
                    &cfg.repo_root,
                    &cfg.engine_start,
                ));
            }
            if problems.is_empty() {
                return Ok(st);
            }
            let dark = problems
                .iter()
                .any(|p| p.code == "switchyard.tier.capable_dark");
            let broken = problems
                .iter()
                .any(|p| p.code == "switchyard.tier.capable_broken");
            let mut st = st;
            if (dark || broken) && st.health == Health::Up {
                st.health = Health::Degraded;
            }
            let prefix = if dark {
                "capable tier dark: "
            } else if broken {
                "capable tier not answering: "
            } else {
                ""
            };
            st.summary = format!("{prefix}{}", st.summary);
            st.problems.extend(problems);
            Ok(st)
        })
    })
}
