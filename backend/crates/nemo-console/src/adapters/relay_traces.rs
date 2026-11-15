//! Relay trace explorer (addendum 9.12): an in-memory index of what NeMo Relay captured on this machine, and the
//! read-only views over it (traces, sessions, analytics, trajectories, capture coverage, and content on demand).
//!
//! How the pieces fit:
//!   * `scan` reads the ATOF directory in passes (new files, grown files; never the same content twice) and feeds
//!     `rows`, which turns events into fixed-size, identifier-only rows. `atif` does the same for trajectory files.
//!   * After a pass that changed something, `derive` groups the rows into traces and sessions and a new
//!     [`snapshot::Snapshot`] is published: one pointer swap. Requests only ever read the last published snapshot,
//!     so no request waits for a scan and no scan waits for a request.
//!   * The passes run on ONE background task (a blocking thread per pass), started with the server when background
//!     tasks are on and stopped with it. Nothing is written to disk.
//!   * Content (prompts, answers, tool arguments and results) is not in the index. The two content requests read
//!     the source line again (`content`, `atif`), for admins only, capped, scrubbed and with a header allowlist.
//!
//! Threat model rows I-9 and R-25.

pub mod analytics;
pub mod atif;
pub mod capture;
pub mod chunkvec;
pub mod content;
pub mod derive;
pub mod event;
pub mod ids;
pub mod lenient;
pub mod query;
pub mod reader;
pub mod rows;
pub mod scan;
pub mod secrets;
pub mod snapshot;
pub mod text;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use serde_json::{Value, json};

use crate::adapters::base::AdapterError;
use crate::config::Settings;
use crate::domain::models::now_ts;
use crate::services::common::RecoverLock as _;
use crate::services::http::Http;

use analytics::AnalyticsQuery;
use atif::{TrajDirs, TrajScan, TrajSet};
use content::ContentQuery;
use derive::Labels;
use query::{Found, SessionQuery, TraceQuery, TrajQuery};
use scan::{Limits, Scan};
use snapshot::{Core, Info, Snapshot, Source};

pub const DEFAULT_SCAN_BYTES: i64 = 16 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_SPANS: i64 = 300_000;
pub const DEFAULT_MAX_LINE_BYTES: i64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_ATIF_BYTES: i64 = 256 * 1024 * 1024;
pub const DEFAULT_REFRESH_S: f64 = 5.0;
/// Content reads (and trajectory file reads) that may run at once; more wait their turn.
const CONTENT_READS: usize = 2;
/// A reachability answer is reused for this long.
const PROBE_TTL: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT_S: f64 = 0.3;

/// The settings of the index (`relay_trace_*`).
#[derive(Clone, Copy, Debug)]
pub struct TraceConfig {
    /// `relay_trace_index = on`.
    pub enabled: bool,
    /// `relay_trace_content = admin`.
    pub content: bool,
    pub limits: Limits,
    pub refresh_s: f64,
}

impl Default for TraceConfig {
    fn default() -> Self {
        TraceConfig {
            enabled: true,
            content: true,
            limits: Limits {
                scan_bytes: DEFAULT_SCAN_BYTES.unsigned_abs(),
                max_spans: usize::try_from(DEFAULT_MAX_SPANS).unwrap_or(300_000),
                max_line_bytes: usize::try_from(DEFAULT_MAX_LINE_BYTES).unwrap_or(1 << 26),
                max_atif_bytes: DEFAULT_MAX_ATIF_BYTES.unsigned_abs(),
                checkpoint_every: scan::CHECKPOINT_EVERY,
            },
            refresh_s: DEFAULT_REFRESH_S,
        }
    }
}

impl TraceConfig {
    #[must_use]
    pub fn from_settings(cfg: &Settings) -> TraceConfig {
        let d = TraceConfig::default();
        TraceConfig {
            enabled: cfg.relay_trace_index == "on",
            content: cfg.relay_trace_content == "admin",
            limits: Limits {
                scan_bytes: u64::try_from(cfg.relay_trace_scan_bytes)
                    .unwrap_or(d.limits.scan_bytes),
                max_spans: usize::try_from(cfg.relay_trace_max_spans).unwrap_or(d.limits.max_spans),
                max_line_bytes: usize::try_from(cfg.relay_trace_max_line_bytes)
                    .unwrap_or(d.limits.max_line_bytes),
                max_atif_bytes: u64::try_from(cfg.relay_trace_max_atif_bytes)
                    .unwrap_or(d.limits.max_atif_bytes),
                checkpoint_every: d.limits.checkpoint_every,
            },
            refresh_s: cfg.relay_trace_refresh_s,
        }
    }

    fn content_word(&self) -> &'static str {
        if self.content { "admin" } else { "off" }
    }
}

/// The directories the index reads (from `ctx.paths`; a missing one is an empty source).
#[derive(Clone, Debug, Default)]
pub struct TraceDirs {
    pub atof: Option<PathBuf>,
    pub atif: Option<PathBuf>,
    pub hermes: Option<PathBuf>,
}

impl TraceDirs {
    #[must_use]
    pub fn from_paths(paths: &IndexMap<String, PathBuf>) -> TraceDirs {
        TraceDirs {
            atof: paths.get("relay_atof_dir").cloned(),
            atif: paths.get("relay_atif_dir").cloned(),
            hermes: paths.get("relay_hermes_dir").cloned(),
        }
    }

    fn traj(&self) -> TrajDirs {
        TrajDirs {
            gateway: self.atif.clone(),
            hermes: self.hermes.clone(),
        }
    }
}

/// One read of the trace explorer: the routes of addendum 9.12, already validated by the API layer.
#[derive(Clone, Debug)]
pub enum Request {
    Index,
    Traces(TraceQuery),
    Trace {
        trace_id: String,
    },
    SpanContent {
        trace_id: String,
        span_id: String,
        query: ContentQuery,
    },
    Sessions(SessionQuery),
    Session {
        key: String,
    },
    Analytics(AnalyticsQuery),
    Trajectories(TrajQuery),
    Trajectory {
        id: String,
        source: Option<String>,
    },
    StepContent {
        id: String,
        source: Option<String>,
        step_id: i64,
        max_chars: usize,
    },
    /// Answered by the adapter (it knows the Relay version and reads `plugins.toml`), see [`TraceIndex::capture`].
    Capture,
}

/// What the scan keeps between passes.
struct Work {
    scan: Scan,
    traj: TrajScan,
    passes: u64,
    /// What the last published rows were built from: when a pass changes none of it, the rows are reused.
    stamp: (u64, u64, u64, usize, u64),
}

/// The index service: the scan state, the published snapshot and the background task.
pub struct TraceIndex {
    pub cfg: TraceConfig,
    pub dirs: TraceDirs,
    /// Symbols of the fixed names the views compare against (interned once, before the first event).
    labels: Labels,
    current: RwLock<Arc<Snapshot>>,
    work: Mutex<Work>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    cancel: AtomicBool,
    busy: AtomicBool,
    generation: AtomicU64,
    content_slots: Arc<tokio::sync::Semaphore>,
    probes: Mutex<HashMap<String, (Instant, bool)>>,
}

fn unknown(what: &str) -> AdapterError {
    AdapterError::UnknownId(what.to_owned())
}

/// The content switch is off (`relay_trace_content = off`): 403 `content_disabled`.
fn content_disabled() -> AdapterError {
    AdapterError::failed(
        "PermissionError",
        "content access is switched off (relay_trace_content = off)",
    )
}

/// The source can no longer be read: 409 `content_unavailable`.
fn content_unavailable(what: &str) -> AdapterError {
    AdapterError::FileNotFound(format!(
        "the source {what} can no longer be read (the file was rotated, rewritten or removed)"
    ))
}

impl TraceIndex {
    #[must_use]
    pub fn new(cfg: TraceConfig, dirs: TraceDirs) -> Arc<TraceIndex> {
        let mut scan = Scan::new(cfg.limits);
        let labels = Labels::intern(&mut scan.ing);
        let mut first = Snapshot::empty("disabled", cfg.limits, cfg.content_word());
        first.info.problems.push(if cfg.enabled {
            (
                "relay.traces.not_started",
                "The trace index is not running: background tasks are off (NEMO_CONSOLE_BACKGROUND_TASKS=false)."
                    .to_owned(),
            )
        } else {
            (
                "relay.traces.disabled",
                "The trace index is switched off (relay_trace_index = off).".to_owned(),
            )
        });
        Arc::new(TraceIndex {
            cfg,
            dirs,
            labels,
            current: RwLock::new(Arc::new(first)),
            work: Mutex::new(Work {
                scan,
                traj: TrajScan::default(),
                passes: 0,
                stamp: (u64::MAX, 0, 0, 0, 0),
            }),
            task: Mutex::new(None),
            cancel: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            content_slots: Arc::new(tokio::sync::Semaphore::new(CONTENT_READS)),
            probes: Mutex::new(HashMap::new()),
        })
    }

    /// The last published snapshot. The lock is held for one pointer copy.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        match self.current.read() {
            Ok(g) => Arc::clone(&g),
            Err(p) => Arc::clone(&p.into_inner()),
        }
    }

    fn publish(&self, snap: Snapshot) {
        let snap = Arc::new(snap);
        match self.current.write() {
            Ok(mut g) => *g = snap,
            Err(p) => *p.into_inner() = snap,
        }
    }

    /// Build a snapshot from the scan as it is now. `done`: the pass is complete (else the state is "building").
    fn snapshot_of(
        &self,
        scan: &Scan,
        labels: &Labels,
        traj: &Arc<TrajSet>,
        reuse: Option<&Arc<Core>>,
        first_pass_done: bool,
        build_ms: Option<u64>,
    ) -> Snapshot {
        let core = match reuse {
            Some(c) => Arc::clone(c),
            None => {
                let mut d = derive::derive(&scan.ing, labels);
                let sources: Vec<Option<Source>> = scan
                    .lineages
                    .iter()
                    .map(|l| {
                        l.file.as_ref().filter(|_| l.head_len > 0).map(|f| Source {
                            name: f.name.clone(),
                            gz: f.gz,
                            head_len: l.head_len,
                            head_hash: l.head_hash,
                            points: l.gz_points.clone().filter(|_| f.gz),
                        })
                    })
                    .collect();
                let mut core = Core {
                    store: scan.ing.store.clone(),
                    syms: scan.ing.syms.frozen(),
                    odd: scan.ing.ids.frozen(),
                    d: derive::Derived::default(),
                    sources,
                    trajectories: Arc::clone(traj),
                };
                // which traces have a trajectory: the ones a trajectory's steps point into
                std::mem::swap(&mut core.d, &mut d);
                let linked: Vec<u32> = traj
                    .items
                    .iter()
                    .filter_map(|t| {
                        let id = query::trajectory_trace(&core, &t.links)?;
                        core.trace_by_id(id).map(|(ti, _)| ti)
                    })
                    .collect();
                for ti in linked {
                    if let Some(t) = core.d.traces.get_mut(ti as usize) {
                        t.has_trajectory = true;
                    }
                }
                self.generation.fetch_add(1, Ordering::Relaxed);
                Arc::new(core)
            }
        };
        let c = &scan.ing.counters;
        let mut problems: Vec<(&'static str, String)> = Vec::new();
        if c.oversize_lines > 0 {
            problems.push((
                "relay.traces.lines_skipped",
                format!(
                    "{} line(s) longer than the line limit ({} bytes, relay_trace_max_line_bytes) were skipped.",
                    c.oversize_lines, scan.limits.max_line_bytes
                ),
            ));
        }
        if c.malformed_lines > 0 {
            problems.push((
                "relay.traces.malformed_lines",
                format!(
                    "{} line(s) were not readable JSON events and were skipped.",
                    c.malformed_lines
                ),
            ));
        }
        if c.no_timestamp + c.bad_id > 0 {
            problems.push((
                "relay.traces.events_ignored",
                format!(
                    "{} event(s) had no usable timestamp or id and were ignored.",
                    c.no_timestamp + c.bad_id
                ),
            ));
        }
        if scan.budget_skipped + scan.budget_cut > 0 {
            problems.push((
                "relay.traces.scan_budget",
                format!(
                    "The scan budget ({} bytes, relay_trace_scan_bytes) was used up: {} older file(s) were not read, {} read only in part.",
                    scan.limits.scan_bytes, scan.budget_skipped, scan.budget_cut
                ),
            ));
        }
        if scan.evicted_spans > 0 {
            problems.push((
                "relay.traces.span_limit",
                format!(
                    "The span limit ({}, relay_trace_max_spans) was reached: the oldest traces were dropped ({} spans, {} older file(s) not read).",
                    scan.limits.max_spans, scan.evicted_spans, scan.cap_skipped
                ),
            ));
        }
        if scan.evicted_marks > 0 {
            problems.push((
                "relay.traces.mark_limit",
                format!(
                    "The mark limit ({} = {} per span of relay_trace_max_spans) was reached: the oldest marks were dropped ({}), marks that belong to no span first.",
                    scan.limits.max_spans.saturating_mul(scan::MARKS_PER_SPAN),
                    scan::MARKS_PER_SPAN,
                    scan.evicted_marks
                ),
            ));
        }
        if scan.file_errors > 0 {
            problems.push((
                "relay.traces.file_errors",
                format!(
                    "{} file read(s) failed (unreadable, not a regular file, or corrupt).",
                    scan.file_errors
                ),
            ));
        }
        if traj.too_large + traj.unreadable > 0 {
            problems.push((
                "relay.traces.trajectories_skipped",
                format!(
                    "{} trajectory file(s) were skipped: {} over the size limit (relay_trace_max_atif_bytes), {} not readable.",
                    traj.too_large + traj.unreadable,
                    traj.too_large,
                    traj.unreadable
                ),
            ));
        }
        if scan.ing.ids.dropped > 0 {
            problems.push((
                "relay.traces.ids_dropped",
                format!(
                    "{} event(s) with an id that is not a UUID were ignored: the table of such ids is full (it grows with relay_trace_max_spans).",
                    scan.ing.ids.dropped
                ),
            ));
        }
        if scan.ing.syms.dropped > 0 {
            problems.push((
                "relay.traces.labels_dropped",
                format!(
                    "{} distinct label(s) beyond the table limit read as unknown.",
                    scan.ing.syms.dropped
                ),
            ));
        }
        let state = if !first_pass_done {
            "building"
        } else if c.events == 0 && traj.items.is_empty() {
            "empty"
        } else {
            "ready"
        };
        Snapshot {
            core,
            info: Info {
                state,
                generation: self.generation.load(Ordering::Relaxed),
                last_refresh_ts: first_pass_done.then(now_ts),
                build_ms,
                files: scan.files.clone(),
                events: c.events,
                chunk_marks: c.chunk_marks,
                resource_last_us: c.resource_last_us,
                // labels or ids left out are data left out too
                truncated: scan.truncated || scan.ing.syms.dropped > 0 || scan.ing.ids.dropped > 0,
                limits: scan.limits,
                content: self.cfg.content_word(),
                problems,
            },
        }
    }

    fn stamp(scan: &Scan, traj: &Arc<TrajSet>) -> (u64, u64, u64, usize, u64) {
        use std::hash::{Hash as _, Hasher as _};
        // where each content can be read now: a rename or a removal changes what the content route can do
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for l in &scan.lineages {
            l.file.as_ref().map(|f| (&f.name, f.gz)).hash(&mut h);
            l.head_len.hash(&mut h);
            // new seek points for a compressed file make its content reads faster: publish them
            l.gz_points
                .as_ref()
                .map(|p| (p.key, p.points.len()))
                .hash(&mut h);
        }
        (
            // a new resource sample alone changes no trace: the snapshot is published without a new derive
            scan.ing
                .counters
                .events
                .saturating_sub(scan.ing.counters.resource_marks),
            scan.ing.counters.duplicates,
            scan.evicted_spans + scan.evicted_marks,
            Arc::as_ptr(traj) as usize,
            h.finish(),
        )
    }

    /// One pass: look at the directories, read what is new, publish. Runs on the caller's thread (the background
    /// task calls it on a blocking thread; tests call it directly). One pass at a time.
    pub fn refresh_blocking(&self) {
        if !self.cfg.enabled || self.cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut guard) = self.work.lock_recovering() else {
            return;
        };
        self.busy.store(true, Ordering::Relaxed);
        let t0 = Instant::now();
        let w = &mut *guard;
        let first_pass_done = w.passes > 0;
        let labels = self.labels;
        let traj_before = Arc::clone(&w.traj.set);
        let build_before = self.snapshot().info.build_ms;
        // partial results while a long read is going on: the newest file is read first, so the UI has data early
        let mut progress = |scan: &Scan| {
            let snap = self.snapshot_of(
                scan,
                &labels,
                &traj_before,
                None,
                first_pass_done,
                build_before,
            );
            self.publish(snap);
        };
        let complete = w
            .scan
            .pass_atof(self.dirs.atof.as_deref(), &self.cancel, &mut progress);
        if complete {
            w.scan.evict_if_over(false);
            // the traces are there: show them before the trajectory files are read
            if !first_pass_done {
                let snap = self.snapshot_of(&w.scan, &labels, &traj_before, None, false, None);
                self.publish(snap);
            }
            w.traj.refresh(
                &self.dirs.traj(),
                self.cfg.limits.max_atif_bytes,
                &self.cancel,
            );
        }
        if !self.cancel.load(Ordering::Relaxed) && complete {
            w.passes += 1;
            let stamp = Self::stamp(&w.scan, &w.traj.set);
            let last = self.snapshot();
            let reuse =
                (stamp == w.stamp && last.info.state != "disabled").then(|| Arc::clone(&last.core));
            w.stamp = stamp;
            let ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
            let snap = self.snapshot_of(
                &w.scan,
                &labels,
                &w.traj.set,
                reuse.as_ref(),
                true,
                Some(ms),
            );
            if w.passes == 1 {
                tracing::info!(
                    "relay trace index built in {} ms: {} events, {} spans, {} traces, {} trajectories",
                    ms,
                    snap.info.events,
                    snap.core.d.spans,
                    snap.core.d.traces.len(),
                    snap.core.trajectories.items.len()
                );
            }
            self.publish(snap);
        }
        self.busy.store(false, Ordering::Relaxed);
    }

    /// Start the background task (once). Without it the index stays empty and says why.
    pub fn start(self: &Arc<Self>) {
        if !self.cfg.enabled {
            return;
        }
        let Ok(mut task) = self.task.lock_recovering() else {
            return;
        };
        if task.is_some() {
            return;
        }
        self.publish(Snapshot::empty(
            "building",
            self.cfg.limits,
            self.cfg.content_word(),
        ));
        let me = Arc::clone(self);
        *task = Some(tokio::spawn(async move {
            loop {
                let worker = Arc::clone(&me);
                // a pass that panics is logged once and the next pass goes ahead
                if tokio::task::spawn_blocking(move || worker.refresh_blocking())
                    .await
                    .is_err()
                {
                    tracing::error!("relay trace index: a pass failed");
                    me.busy.store(false, Ordering::Relaxed);
                }
                if me.cancel.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(Duration::from_secs_f64(me.cfg.refresh_s.max(0.05))).await;
            }
        }));
    }

    /// True when the background task was started.
    #[must_use]
    pub fn has_task(&self) -> bool {
        self.task
            .lock_recovering()
            .map(|t| t.is_some())
            .unwrap_or(false)
    }

    /// Stop the background task and wait for a pass that is running to notice (it checks at every line).
    pub async fn aclose(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let handle = self.task.lock_recovering().ok().and_then(|mut t| t.take());
        if let Some(h) = handle {
            h.abort();
            let _ = h.await;
        }
        for _ in 0..300 {
            if !self.busy.load(Ordering::Relaxed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn blocking<T: Send + 'static>(
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, AdapterError> {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|_| AdapterError::failed("RuntimeError", "trace query failed"))
    }

    /// Answer one request from the last published snapshot. With the index switched off (`relay_trace_index =
    /// off`) every answer says so: the lists are empty and carry `state = "disabled"`, and a detail is not found
    /// with a message that names the switch.
    pub async fn handle(self: &Arc<Self>, req: Request) -> Result<Value, AdapterError> {
        if self.cfg.enabled {
            return self.answer(req).await;
        }
        let list = matches!(
            req,
            Request::Traces(_)
                | Request::Sessions(_)
                | Request::Analytics(_)
                | Request::Trajectories(_)
        );
        match self.answer(req).await {
            Ok(mut v) if list => {
                if let Some(m) = v.as_object_mut() {
                    m.insert("state".to_owned(), json!("disabled"));
                }
                Ok(v)
            }
            Err(AdapterError::UnknownId(what)) => Err(AdapterError::UnknownId(format!(
                "{what}: the trace index is switched off (relay_trace_index = off)"
            ))),
            other => other,
        }
    }

    async fn answer(self: &Arc<Self>, req: Request) -> Result<Value, AdapterError> {
        let snap = self.snapshot();
        #[allow(clippy::cast_possible_truncation)]
        let now_us = (now_ts() * 1e6) as i64;
        match req {
            Request::Index => Ok(snap.index_state()),
            Request::Capture => Err(AdapterError::NoSuchEndpoint),
            Request::Traces(q) => Self::blocking(move || query::traces(&snap, &q, now_us)).await,
            Request::Sessions(q) => {
                Self::blocking(move || query::sessions(&snap, &q, now_us)).await
            }
            Request::Trajectories(q) => {
                Self::blocking(move || query::trajectories(&snap, &q)).await
            }
            Request::Analytics(q) => {
                let labels = self.labels;
                Self::blocking(move || analytics::analytics(&snap, &q, &labels, now_us)).await
            }
            Request::Trace { trace_id } => Self::blocking(move || query::trace(&snap, &trace_id))
                .await?
                .ok_or_else(|| unknown("trace")),
            Request::Session { key } => Self::blocking(move || query::session(&snap, &key))
                .await?
                .ok_or_else(|| unknown("session")),
            Request::SpanContent {
                trace_id,
                span_id,
                query,
            } => {
                if !self.cfg.content {
                    return Err(content_disabled());
                }
                let Some(dir) = self.dirs.atof.clone() else {
                    return Err(unknown("span"));
                };
                let max_line = self.cfg.limits.max_line_bytes;
                let _slot = Arc::clone(&self.content_slots).acquire_owned().await;
                Self::blocking(move || {
                    let row = query::span_in_trace(&snap.core, &trace_id, &span_id)
                        .ok_or_else(|| unknown("span"))?;
                    content::span_content(&snap.core, &dir, row, query, max_line)
                        .map_err(|_| content_unavailable("line"))
                })
                .await?
            }
            Request::Trajectory { id, source } => {
                let dirs = self.dirs.traj();
                let max = self.cfg.limits.max_atif_bytes;
                let _slot = Arc::clone(&self.content_slots).acquire_owned().await;
                Self::blocking(move || {
                    let t = match query::find_trajectory(&snap.core, &id, source.as_deref()) {
                        Found::One(t) => t,
                        Found::None => return Err(unknown("trajectory")),
                        Found::Ambiguous => {
                            return Err(AdapterError::Invalid(
                                "this trajectory id exists in more than one source: add source=<name>".to_owned(),
                            ));
                        }
                    };
                    let dir = dirs.dir_of(&t.source).ok_or_else(|| unknown("trajectory"))?;
                    // the file went away or changed since it was indexed: 409, like content
                    let (steps, truncated) = atif::steps(&dir, t, max).map_err(|e| match e {
                        atif::ReadError::NoSuchStep => unknown("trajectory"),
                        atif::ReadError::Unavailable => content_unavailable("file"),
                    })?;
                    Ok(json!({
                        "trajectory": query::trajectory_summary(&snap.core, t),
                        "steps": steps,
                        "truncated": truncated,
                    }))
                })
                .await?
            }
            Request::StepContent {
                id,
                source,
                step_id,
                max_chars,
            } => {
                if !self.cfg.content {
                    return Err(content_disabled());
                }
                let dirs = self.dirs.traj();
                let max = self.cfg.limits.max_atif_bytes;
                let _slot = Arc::clone(&self.content_slots).acquire_owned().await;
                Self::blocking(move || {
                    let t = match query::find_trajectory(&snap.core, &id, source.as_deref()) {
                        Found::One(t) => t,
                        Found::None => return Err(unknown("trajectory")),
                        Found::Ambiguous => {
                            return Err(AdapterError::Invalid(
                                "this trajectory id exists in more than one source: add source=<name>".to_owned(),
                            ));
                        }
                    };
                    let dir = dirs.dir_of(&t.source).ok_or_else(|| unknown("trajectory"))?;
                    atif::step_content(&dir, t, max, step_id, max_chars).map_err(|e| match e {
                        atif::ReadError::NoSuchStep => unknown("step"),
                        atif::ReadError::Unavailable => content_unavailable("file"),
                    })
                })
                .await?
            }
        }
    }

    /// Whether something answers on a loopback OTLP endpoint. One short GET per endpoint per minute at most; the
    /// URL is `http://host:port/` of a loopback host and nothing else (built by `capture::endpoint_target`).
    async fn reachable(&self, http: &Http, urls: &[String]) -> IndexMap<String, bool> {
        let mut out: IndexMap<String, bool> = IndexMap::new();
        let mut ask: Vec<String> = Vec::new();
        if let Ok(mut cache) = self.probes.lock_recovering() {
            cache.retain(|_, (at, _)| at.elapsed() < PROBE_TTL);
            for u in urls {
                match cache.get(u) {
                    Some((_, ok)) => {
                        out.insert(u.clone(), *ok);
                    }
                    None => ask.push(u.clone()),
                }
            }
        }
        let answers = futures::future::join_all(ask.iter().map(|u| async move {
            match http.get(u, PROBE_TIMEOUT_S).await {
                // anything that answers counts, a protocol error after connecting included (a gRPC port)
                Ok(_) => true,
                Err(e) => matches!(e.class, "RemoteProtocolError" | "ReadError"),
            }
        }))
        .await;
        if let Ok(mut cache) = self.probes.lock_recovering() {
            for (u, ok) in ask.into_iter().zip(answers) {
                cache.insert(u.clone(), (Instant::now(), ok));
                out.insert(u, ok);
            }
        }
        out
    }

    /// `GET /capture`. `plugins_toml`: the text of the file, `None` when it cannot be read.
    pub async fn capture(
        &self,
        plugins_toml: Option<&str>,
        relay_version: Option<&str>,
        http: &Http,
    ) -> Value {
        let cfg = plugins_toml.map(capture::parse_config).unwrap_or_default();
        let observed = capture::Observed::of(&self.snapshot());
        let reachable = self.reachable(http, &capture::probe_urls(&cfg)).await;
        capture::capture(&cfg, &observed, relay_version, &reachable, now_ts())
    }
}
