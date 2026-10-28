//! The serve path: logging, the lifespan (user dirs, bootstrap key, secret registration, metrics poller), binding,
//! graceful shutdown. Port of `main.py` `run()` / `create_app` lifespan / `ConsoleServer` and `telemetry.py`
//! `configure_logging`. Exit codes like Python: 0 after a clean stop, 3 when startup fails (uvicorn's
//! STARTUP_FAILURE), 1 for invalid settings (an uncaught pydantic ValidationError).

use std::sync::Arc;
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::Settings;
use crate::container::{AppState, Overrides, build_container};
use crate::services::common::RecoverLock as _;
use crate::services::common::redact;
use crate::services::playground::register_secret_env;
use crate::services::pyfmt::json_dumps;

/// Create the Data Designer dirs the console owns (0700; addendum 9.7). ~/.data-designer is NOT created. Never fails.
pub fn ensure_user_dirs(cfg: &Settings) {
    use std::os::unix::fs::DirBuilderExt;
    for p in [&cfg.dd_scripts_dir, &cfg.dd_datasets_root] {
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(p);
    }
}

/// JSON log lines on stderr, scrubbed: {"ts", "level", "logger", "msg"} (Python's JsonFormatter).
struct JsonLayer;

#[derive(Default)]
struct Msg(String);

impl Visit for Msg {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.0);
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for JsonLayer {
    #[allow(clippy::print_stderr)]
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = match *meta.level() {
            tracing::Level::ERROR => "ERROR",
            tracing::Level::WARN => "WARNING",
            tracing::Level::INFO => "INFO",
            _ => "DEBUG",
        };
        if *meta.level() > tracing::Level::INFO {
            return;
        }
        let mut msg = Msg::default();
        event.record(&mut msg);
        // logging.Formatter.formatTime(record, "%Y-%m-%dT%H:%M:%S%z"): LOCAL time with its offset
        let ts = chrono::Local::now()
            .format("%Y-%m-%dT%H:%M:%S%z")
            .to_string();
        let doc = serde_json::json!({
            "ts": ts, "level": level, "logger": meta.target().replace("::", "."), "msg": redact(&msg.0),
        });
        eprintln!("{}", json_dumps(&doc));
    }
}

pub fn configure_logging() {
    let _ = tracing_subscriber::registry().with(JsonLayer).try_init();
    install_panic_hook();
}

/// Audit finding 3: a panic is reported as one JSON log line whose message goes through `redact` (the JsonLayer redacts
/// every message), not the default hook's raw text on stderr (which `open` sends to `console.log`): a panic message
/// can quote a request header or a config value.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(target: "nemo_console", "{}", panic_line(info));
    }));
}

/// The text of the panic log line: location and message (redacted by the log layer).
#[must_use]
pub fn panic_line(info: &std::panic::PanicHookInfo<'_>) -> String {
    let at = info
        .location()
        .map(|l| format!(" at {}:{}", l.file(), l.line()))
        .unwrap_or_default();
    format!(
        "panic{at}: {}",
        crate::services::common::panic_text(info.payload())
    )
}

/// Lifespan start: user dirs, the first-start admin key, credential values registered with the redactor, and the
/// metrics poller (when `background_tasks`).
pub fn startup(state: &Arc<AppState>) -> Option<tokio::task::JoinHandle<()>> {
    ensure_user_dirs(&state.cfg);
    match state.auth.ensure_bootstrap_key() {
        Ok(Some(path)) => tracing::info!(
            "first start: admin API key written to {} (0600); copy it, then delete the file",
            path.display()
        ),
        Ok(None) => {}
        Err(e) => tracing::error!("bootstrap key: {e}"),
    }
    for name in state.settings.effective_key_env().values() {
        register_secret_env(name, &state.environ);
    }
    register_secret_env(&state.cfg.capable_probe_key_env, &state.environ);
    if state.cfg.background_tasks {
        let poller = Arc::clone(&state.metrics);
        return Some(tokio::spawn(poller.run()));
    }
    None
}

/// Lifespan end: stop the poller, flush rollups, stop the active probe.
pub async fn shutdown(state: &Arc<AppState>, poller: Option<tokio::task::JoinHandle<()>>) {
    if let Some(p) = poller {
        p.abort();
        let _ = p.await;
    }
    state.metrics.flush();
    if let Some(probe) = &state.capable_probe {
        probe.aclose().await;
    }
}

/// Which shutdown signal arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitSignal {
    Term,
    Int,
}

async fn signal() -> ExitSignal {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => ExitSignal::Int,
        () = term => ExitSignal::Term,
    }
}

/// `nemo-console` (serve): returns the process exit code.
#[must_use]
pub fn run() -> i32 {
    configure_logging();
    let cfg = match Settings::load() {
        Ok(c) => c,
        Err(e) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("{e}");
            }
            return 1;
        }
    };
    ensure_user_dirs(&cfg);
    let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        return 3;
    };
    let (code, sig) = rt.block_on(serve(cfg));
    drop(rt);
    if sig == Some(ExitSignal::Term) {
        // uvicorn re-raises the signal it shut down for: the process ends KILLED BY SIGTERM (status -15), not exit 0;
        // for SIGINT the re-raised KeyboardInterrupt is suppressed by `run()`, so that one ends with exit 0
        let _ = signal_hook::low_level::emulate_default_handler(nix::libc::SIGTERM);
        return 128 + nix::libc::SIGTERM;
    }
    code
}

/// Seconds the server waits for open connections after the shutdown signal (`shutdown_grace_s`, 1..=60): it must
/// stay below `nemo-console stop`'s STOP_TIMEOUT_S (Python: uvicorn's `timeout_graceful_shutdown`).
#[must_use]
pub fn graceful_shutdown_s(cfg: &Settings) -> u64 {
    u64::try_from(cfg.shutdown_grace_s.max(1)).unwrap_or(1)
}

/// The exit hook (Python `ConsoleServer.handle_exit`): every open SSE stream ends first (`event: shutdown`), then the
/// server stops accepting and waits [`graceful_shutdown_s`] for the rest.
pub fn on_exit_signal(state: &AppState) {
    state.shutdown.begin();
}

/// uvicorn's own log lines (logger `uvicorn.error`), so an operator reading the console log sees the same story.
macro_rules! uv_info {
    ($($t:tt)*) => { tracing::info!(target: "uvicorn.error", $($t)*) };
}

/// Transport limits (audit finding 4). uvicorn closed an idle keep-alive connection after 5 s; hyper has no such
/// default, so: headers (of the first request, and of every next one on a kept-alive connection) must arrive within
/// `header_read_timeout`, and at most `max_connections` connections are served at once (one over the cap is closed at
/// once; SSE streams have their own cap, `sse_max_connections`). The request BODY has its own total timeout
/// (`api::BODY_READ_TIMEOUT_MS`).
#[derive(Debug, Clone, Copy)]
pub struct ServeLimits {
    pub header_read_timeout: Duration,
    pub max_connections: usize,
}

impl ServeLimits {
    pub const DEFAULT: ServeLimits = ServeLimits {
        header_read_timeout: Duration::from_secs(5),
        max_connections: 256,
    };
}

/// Serve `app` on `listener` until `shutdown` resolves, then wait for the open connections (the caller bounds that
/// wait). Each request carries its peer address (`ConnectInfo`), which the loopback-trust and audit code read.
pub async fn serve_connections(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    limits: ServeLimits,
    shutdown: impl std::future::Future<Output = ()>,
) {
    use tower::ServiceExt as _;
    let slots = Arc::new(tokio::sync::Semaphore::new(limits.max_connections.max(1)));
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(_) => {
                    // EMFILE / ECONNABORTED: back off instead of spinning
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
            drop(stream); // at capacity: closed at once
            continue;
        };
        let app = app.clone();
        let svc =
            hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut()
                    .insert(axum::extract::ConnectInfo(peer));
                app.clone().oneshot(req.map(axum::body::Body::new))
            });
        let conn = hyper::server::conn::http1::Builder::new()
            .timer(hyper_util::rt::TokioTimer::new())
            .header_read_timeout(limits.header_read_timeout)
            .keep_alive(true)
            .serve_connection(hyper_util::rt::TokioIo::new(stream), svc);
        let conn = graceful.watch(conn);
        tokio::spawn(async move {
            let _ = conn.await;
            drop(permit);
        });
    }
    drop(listener);
    graceful.shutdown().await;
}

async fn serve(cfg: Settings) -> (i32, Option<ExitSignal>) {
    let grace = graceful_shutdown_s(&cfg);
    let (host, port) = (cfg.host.clone(), cfg.port);
    let state = match build_container(cfg, Overrides::default()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("startup failed: {e}");
            return (3, None);
        }
    };
    let pid = std::process::id();
    uv_info!("Started server process [{pid}]");
    // uvicorn runs the app's lifespan BEFORE it binds: a failed bind still leaves the bootstrap key and the database
    uv_info!("Waiting for application startup.");
    let poller = startup(&state);
    uv_info!("Application startup complete.");
    let bound = match u16::try_from(port) {
        Ok(p) => tokio::net::TcpListener::bind((host.as_str(), p))
            .await
            .map_err(|e| (p, e)),
        Err(_) => Err((0, std::io::Error::from_raw_os_error(nix::libc::EINVAL))),
    };
    let listener = match bound {
        Ok(l) => l,
        Err((_, e)) => {
            let code = e.raw_os_error().unwrap_or(0);
            let reason = nix::errno::Errno::from_raw(code).desc().to_lowercase();
            tracing::error!(
                target: "uvicorn.error",
                "[Errno {code}] error while attempting to bind on address ({}, {port}): [errno {code}] {reason}",
                crate::services::pyfmt::str_repr(&host)
            );
            uv_info!("Waiting for application shutdown.");
            shutdown(&state, poller).await;
            uv_info!("Application shutdown complete.");
            return (3, None);
        }
    };
    let shown = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    uv_info!("Uvicorn running on http://{shown}:{port} (Press CTRL+C to quit)");
    let app = crate::api::app(Arc::clone(&state));
    let st = Arc::clone(&state);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let got = Arc::new(std::sync::Mutex::new(None::<ExitSignal>));
    let got_w = Arc::clone(&got);
    let graceful = async move {
        let sig = signal().await;
        if let Ok(mut g) = got_w.lock_recovering() {
            *g = Some(sig);
        }
        uv_info!("Shutting down");
        // every open SSE stream ends cleanly first (`event: shutdown`)
        on_exit_signal(&st);
        let _ = done_tx.send(());
    };
    let server = serve_connections(listener, app, ServeLimits::DEFAULT, graceful);
    tokio::pin!(server);
    tokio::select! {
        () = &mut server => {}
        _ = async {
            let _ = done_rx.await;
            tokio::time::sleep(Duration::from_secs(grace)).await;
        } => {
            tracing::warn!("closing the connections still open after {grace} s");
        }
    }
    uv_info!("Waiting for application shutdown.");
    shutdown(&state, poller).await;
    uv_info!("Application shutdown complete.");
    uv_info!("Finished server process [{pid}]");
    let sig = got.lock_recovering().ok().and_then(|g| *g);
    (0, sig)
}
