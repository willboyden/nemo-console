//! Runtime helpers shared by the three workstream C adapters (Data Designer, NeMo Evaluator, SkillEvaluator): the Python
//! adapters repeat the same `_run`, `_router_up`, run-directory, `run.json` and `_num` code; these are their ports,
//! plus the Python built-ins the adapters lean on (`str()` of JSON values, `format(x, ".Nf")`, `Path.resolve()`,
//! `Path.read_text()`, OSError texts).

use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use regex::Regex;
use serde_json::{Map, Value};

use crate::adapters::base::{AdapterContext, AdapterError, CommandResult, RunOpts};
use crate::api::pyjson;
use crate::domain::models::{ActionResult, Problem};
use crate::services::common::RecoverLock as _;
use crate::services::common::redact;
use crate::services::pyfmt::{JsonOpts, char_prefix, float_repr, json_dumps_with, str_repr};

use super::pyobj::py_strip;

/// The router's default base URL (Python `ctx.urls.get("litellm", "http://127.0.0.1:4000")`).
pub const DEFAULT_LITELLM: &str = "http://127.0.0.1:4000";

/// `re.fullmatch(pattern)` as a compiled anchored regex.
#[allow(clippy::expect_used)]
#[must_use]
pub fn full(pattern: &str) -> Regex {
    Regex::new(&format!(r"\A(?:{pattern})\z")).expect("static regex")
}

/// `self.ctx.urls.get("litellm", DEFAULT).rstrip("/")`.
#[must_use]
pub fn litellm(ctx: &AdapterContext) -> String {
    ctx.urls
        .get("litellm")
        .map_or(DEFAULT_LITELLM, String::as_str)
        .trim_end_matches('/')
        .to_owned()
}

/// `_run`: the runner's refusals (not allowlisted, not on PATH, spawn failures) become data, rc 127.
pub async fn run_cmd(
    ctx: &AdapterContext,
    argv: &[String],
    timeout: f64,
    max_bytes: usize,
    cwd: Option<&Path>,
) -> CommandResult {
    let mut opts = RunOpts::timeout(timeout).max_bytes(max_bytes);
    if let Some(d) = cwd {
        opts = opts.cwd(d.to_path_buf());
    }
    match ctx.runner.run(argv, opts).await {
        Ok(r) => r,
        Err(e) => {
            let text = format!("{}: {e}", e.py_class());
            CommandResult::new(127, "", char_prefix(&text, 300))
        }
    }
}

/// `_router_up`: GET <litellm>/health/liveliness (3 s); any status below 500 counts as up.
pub async fn router_up(ctx: &AdapterContext) -> (bool, String) {
    let url = format!("{}/health/liveliness", litellm(ctx));
    match ctx.http.get(&url, 3.0).await {
        Ok(resp) => (resp.status < 500, format!("HTTP {}", resp.status)),
        Err(e) => (false, e.class.to_owned()),
    }
}

/// `_lab_file(rel)`: `rel`'s file name under the layout dir `layout_key`, else `rel` under the repo root; the first
/// that exists.
#[must_use]
pub fn lab_file(ctx: &AdapterContext, layout_key: &str, rel: &str) -> Option<PathBuf> {
    let name = Path::new(rel).file_name().map(PathBuf::from);
    let base = ctx.paths.get(layout_key).zip(name).map(|(b, n)| b.join(n));
    let repo = ctx.paths.get("repo").map(|r| r.join(rel));
    [base, repo].into_iter().flatten().find(|p| p.exists())
}

/// `time.time()`.
#[must_use]
pub fn now() -> f64 {
    crate::domain::models::now_ts()
}

/// `time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{int(time.time() * 1000) % 1000:03d}"`, from ONE clock reading
/// (PORTING D-30: Python reads the clock twice, so at a second boundary its id can fall a second behind).
#[must_use]
pub fn run_stamp() -> String {
    crate::services::plumbing::run_stamp()
}

/// `path.mkdir(mode=0o700, parents=True, exist_ok=True)`: missing parents get the default mode, the leaf 0700.
pub fn mkdir_private(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// `_new_run_dir(name)`: `<runs>/<stamp>-<name>` created 0700.
pub fn new_run_dir(runs_dir: &Path, name: &str) -> std::io::Result<(String, PathBuf)> {
    let run_id = format!("{}-{name}", run_stamp());
    let run_dir = runs_dir.join(&run_id);
    mkdir_private(&run_dir)?;
    Ok((run_id, run_dir))
}

/// `_write_meta`: `json.dumps(meta (error keys redacted), indent=1)` to `.run.json.tmp`, chmod 0600, rename.
pub fn write_meta(
    run_dir: &Path,
    meta: &Map<String, Value>,
    redact_keys: &[&str],
) -> std::io::Result<()> {
    let mut out = meta.clone();
    for k in redact_keys {
        if let Some(Value::String(s)) = out.get(*k) {
            let r = redact(s);
            out.insert((*k).to_owned(), Value::String(r));
        }
    }
    let text = json_dumps_with(
        &Value::Object(out),
        JsonOpts {
            indent: Some(1),
            ..JsonOpts::default()
        },
    );
    // Python writes `.run.json.tmp` then renames; the core's atomic write uses a random, exclusively created 0600 temp
    // file instead, so a symlink planted at the predictable name is never followed (audit finding 9)
    crate::services::config_store::atomic_write(&run_dir.join("run.json"), text.as_bytes(), 0o600)
}

/// Why `Path.read_text()` failed: an `OSError`, or a `UnicodeDecodeError` (a `ValueError`).
#[derive(Debug)]
pub enum ReadErr {
    Os(std::io::Error),
    Decode(String),
}

impl ReadErr {
    /// The Python exception this is, as an adapter error.
    #[must_use]
    pub fn to_adapter(&self, path: &Path) -> AdapterError {
        match self {
            ReadErr::Os(e) => {
                let (class, text) = os_error(e, Some(path));
                AdapterError::failed(class, text)
            }
            ReadErr::Decode(m) => AdapterError::failed("UnicodeDecodeError", m.clone()),
        }
    }
}

/// `Path.read_text()`: UTF-8 (strict) with universal newlines (`\r\n` and `\r` become `\n`).
pub fn read_text(path: &Path) -> Result<String, ReadErr> {
    let bytes = std::fs::read(path).map_err(ReadErr::Os)?;
    decode_text(&bytes).map_err(ReadErr::Decode)
}

/// `bytes.decode("utf-8")` + universal newlines; the error is `str(UnicodeDecodeError)`.
pub fn decode_text(bytes: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(if s.contains('\r') {
            s.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            s.to_owned()
        }),
        Err(e) => Err(utf8_error_text(bytes, &e)),
    }
}

/// `str(UnicodeDecodeError)` of `bytes.decode('utf-8')`.
#[must_use]
pub fn utf8_error_text(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let start = e.valid_up_to();
    let (end, reason) = match e.error_len() {
        None => (bytes.len(), "unexpected end of data"),
        Some(n) => {
            let b0 = bytes.get(start).copied().unwrap_or(0);
            let reason = if (0x80..0xc2).contains(&b0) || b0 > 0xf4 {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            (start + n, reason)
        }
    };
    if end.saturating_sub(start) <= 1 {
        format!(
            "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            bytes.get(start).copied().unwrap_or(0)
        )
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
            end - 1
        )
    }
}

/// `type(e).__name__` and `str(e)` of the OSError Python raises for this errno (`[Errno N] text: 'path'`).
#[must_use]
pub fn os_error(e: &std::io::Error, path: Option<&Path>) -> (&'static str, String) {
    let Some(code) = e.raw_os_error() else {
        return ("OSError", e.to_string());
    };
    let class = match code {
        2 => "FileNotFoundError",
        1 | 13 => "PermissionError",
        17 => "FileExistsError",
        20 => "NotADirectoryError",
        21 => "IsADirectoryError",
        _ => "OSError",
    };
    let full = std::io::Error::from_raw_os_error(code).to_string();
    let text = full
        .rsplit_once(" (os error ")
        .map_or(full.as_str(), |(t, _)| t)
        .to_owned();
    match path {
        Some(p) => (
            class,
            format!("[Errno {code}] {text}: {}", str_repr(&p.to_string_lossy())),
        ),
        None => (class, format!("[Errno {code}] {text}")),
    }
}

/// An io error as the adapter error Python would raise.
#[must_use]
pub fn io_adapter_error(e: &std::io::Error, path: Option<&Path>) -> AdapterError {
    let (class, text) = os_error(e, path);
    AdapterError::failed(class, text)
}

/// Deepest JSON nesting accepted before parsing (a cheap pre-scan of tool-written files: run.json, summary.json,
/// result.json, records.jsonl lines). The core's `pyjson` itself refuses more than `pyjson::MAX_DEPTH` (512) levels
/// since A-6, so this bound is now a fast path; Python's decoder goes to about 9,998 (PORTING.md D-12).
pub const MAX_JSON_DEPTH: usize = 900;

/// True when the JSON text nests arrays/objects at most [`MAX_JSON_DEPTH`] deep (strings skipped).
#[must_use]
pub fn json_depth_ok(b: &[u8]) -> bool {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for &c in b {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return false;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

/// `json.loads(text)` (str input) as a JSON value; `None` on any ValueError.
#[must_use]
pub fn json_loads_str(text: &str) -> Option<Value> {
    if !json_depth_ok(text.as_bytes()) {
        return None;
    }
    pyjson::loads(text.as_bytes()).ok().map(|v| v.to_value())
}

/// `json.loads(bytes)`: a UTF-8 BOM is accepted for bytes input.
#[must_use]
pub fn json_loads_bytes(b: &[u8]) -> Option<Value> {
    let b = b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(b);
    if !json_depth_ok(b) {
        return None;
    }
    pyjson::loads(b).ok().map(|v| v.to_value())
}

/// `json.loads(path.read_text())` that must be a dict (`_load_meta` without the running check; `None` on OSError or
/// ValueError).
#[must_use]
pub fn load_json_dict(path: &Path, cap: Option<u64>) -> Option<Map<String, Value>> {
    if let Some(cap) = cap {
        let md = std::fs::metadata(path).ok()?;
        if md.len() > cap {
            return None;
        }
    }
    let text = read_text(path).ok()?;
    match json_loads_str(&text)? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Entries of `dir` whose name fully matches `re` and that are directories (following symlinks unless
/// `no_symlinks`), newest name first (`_run_dirs`).
#[must_use]
pub fn run_dirs(dir: &Path, re: &Regex, no_symlinks: bool) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let p = e.path();
        if re.is_match(&name) && p.is_dir() && !(no_symlinks && p.is_symlink()) {
            out.push((name, p));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, p)| p).collect()
}

/// `_prune`: every run directory past `keep` is removed, except the one a live run writes into
/// (`shutil.rmtree(d, ignore_errors=True)`, which refuses symlinks).
pub fn prune(dirs: &[PathBuf], keep: usize, live: Option<&str>) {
    for d in dirs.iter().skip(keep) {
        let name = d.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if Some(name) == live || d.is_symlink() {
            continue;
        }
        let _ = std::fs::remove_dir_all(d);
    }
}

/// `_num(v)`: an int or float (not a bool) as a float.
#[must_use]
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// `_num(v)` as a JSON float value (or null).
#[must_use]
pub fn num_value(v: Option<&Value>) -> Value {
    num(v)
        .and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// `isinstance(v, int) and not isinstance(v, bool)` for a JSON value.
#[must_use]
pub fn is_int(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Number(n)) if !n.is_f64())
}

/// `_dict(v)`.
#[must_use]
pub fn dict(v: Option<&Value>) -> Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// `_list(v)`.
#[must_use]
pub fn list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

/// `bool(v)` of a JSON value.
#[must_use]
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// `str(v)` of a JSON-loaded value (`f"{v}"`).
#[must_use]
pub fn pystr(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        other => pyrepr(other),
    }
}

/// `repr(v)` of a JSON-loaded value.
#[must_use]
pub fn pyrepr(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_owned(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.to_owned(),
        Some(Value::Number(n)) => {
            if n.is_f64() {
                float_repr(n.as_f64().unwrap_or(0.0))
            } else {
                n.to_string()
            }
        }
        Some(Value::String(s)) => str_repr(s),
        Some(Value::Array(a)) => {
            let inner: Vec<String> = a.iter().map(|x| pyrepr(Some(x))).collect();
            format!("[{}]", inner.join(", "))
        }
        Some(Value::Object(m)) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, x)| format!("{}: {}", str_repr(k), pyrepr(Some(x))))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// `v == s` for a JSON value and a str.
#[must_use]
pub fn is_str(v: Option<&Value>, s: &str) -> bool {
    matches!(v, Some(Value::String(x)) if x == s)
}

/// `format(x, f"{'+' if signed else ''}.{prec}f")` for a Python float.
#[must_use]
pub fn fixed(x: f64, prec: usize, signed: bool) -> String {
    if x.is_nan() {
        return if signed { "+nan" } else { "nan" }.to_owned();
    }
    if x.is_infinite() {
        return match (x > 0.0, signed) {
            (true, true) => "+inf",
            (true, false) => "inf",
            _ => "-inf",
        }
        .to_owned();
    }
    if signed {
        format!("{x:+.prec$}")
    } else {
        format!("{x:.prec$}")
    }
}

/// `format(v, ".{prec}f")` of a JSON-loaded value, with Python's errors for the types that refuse it.
pub fn fixed_of(v: Option<&Value>, prec: usize) -> Result<String, AdapterError> {
    match v {
        Some(Value::Number(n)) => Ok(fixed(n.as_f64().unwrap_or(0.0), prec, false)),
        Some(Value::Bool(b)) => Ok(fixed(if *b { 1.0 } else { 0.0 }, prec, false)),
        Some(Value::String(_)) => Err(AdapterError::failed(
            "ValueError",
            "Unknown format code 'f' for object of type 'str'",
        )),
        None | Some(Value::Null) => Err(AdapterError::failed(
            "TypeError",
            "unsupported format string passed to NoneType.__format__",
        )),
        Some(Value::Array(_)) => Err(AdapterError::failed(
            "TypeError",
            "unsupported format string passed to list.__format__",
        )),
        Some(Value::Object(_)) => Err(AdapterError::failed(
            "TypeError",
            "unsupported format string passed to dict.__format__",
        )),
    }
}

/// Python type name of a JSON-loaded value.
#[must_use]
pub fn type_name(v: Option<&Value>) -> &'static str {
    match v {
        None | Some(Value::Null) => "NoneType",
        Some(Value::Bool(_)) => "bool",
        Some(Value::Number(n)) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Some(Value::String(_)) => "str",
        Some(Value::Array(_)) => "list",
        Some(Value::Object(_)) => "dict",
    }
}

/// `s.strip()[-n:]` (Python: last `n` characters of the stripped text).
#[must_use]
pub fn strip_tail(s: &str, n: usize) -> String {
    let t = py_strip(s);
    let len = t.chars().count();
    t.chars().skip(len.saturating_sub(n)).collect()
}

/// `os.path.realpath(path)` of Python 3.13 (`Path.resolve()`, non-strict), ported from its iterative algorithm:
/// symlinks resolved as far as the path exists, `..` removes the last resolved component, a missing tail, an
/// unreadable link or a symlink loop is kept literally.
#[must_use]
pub fn realpath(path: &Path) -> PathBuf {
    let filename = path.to_string_lossy().into_owned();
    let mut rest: Vec<Option<String>> = filename
        .split('/')
        .rev()
        .map(|s| Some(s.to_owned()))
        .collect();
    let mut part_count = rest.len();
    let mut cur = if filename.starts_with('/') {
        "/".to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .to_string_lossy()
            .into_owned()
    };
    let mut seen: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    while part_count > 0 {
        let Some(item) = rest.pop() else { break };
        let Some(name) = item else {
            if let Some(Some(link)) = rest.pop() {
                seen.insert(link, Some(cur.clone()));
            }
            continue;
        };
        part_count -= 1;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            cur = match cur.rfind('/') {
                Some(0) | None => "/".to_owned(),
                Some(i) => cur[..i].to_owned(),
            };
            continue;
        }
        let newpath = if cur == "/" {
            format!("/{name}")
        } else {
            format!("{cur}/{name}")
        };
        let is_link = match std::fs::symlink_metadata(&newpath) {
            Ok(md) => md.file_type().is_symlink(),
            Err(_) => false,
        };
        if !is_link {
            cur = newpath;
            continue;
        }
        if let Some(v) = seen.get(&newpath) {
            cur = v.clone().unwrap_or(newpath);
            continue;
        }
        let Ok(target) = std::fs::read_link(&newpath) else {
            cur = newpath;
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        if target.starts_with('/') {
            cur = "/".to_owned();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(newpath));
        rest.push(None);
        let parts: Vec<Option<String>> = target
            .split('/')
            .rev()
            .map(|s| Some(s.to_owned()))
            .collect();
        part_count += parts.len();
        rest.extend(parts);
    }
    PathBuf::from(cur)
}

/// `PurePosixPath` string normalisation of a relative path (`a//b/./c` -> `a/b/c`; `..` kept).
#[must_use]
pub fn pure_join(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::ParentDir => out.push(".."),
            Component::RootDir => out = PathBuf::from("/"),
            Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// `PurePosixPath(rel).parent` as a relative string (normalised).
#[must_use]
pub fn pure_parent(rel: &str) -> String {
    let parts: Vec<String> = Path::new(rel)
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_owned()),
            _ => None,
        })
        .collect();
    if parts.len() <= 1 {
        ".".to_owned()
    } else {
        parts[..parts.len() - 1].join("/")
    }
}

/// `run_action`'s `done(ok, out, problems)`.
#[derive(Clone, Copy)]
pub struct Done {
    t0: Instant,
}

impl Done {
    #[must_use]
    pub fn start() -> Self {
        Done { t0: Instant::now() }
    }

    #[must_use]
    pub fn call(&self, ok: bool, out: &str, problems: Vec<Problem>) -> ActionResult {
        ActionResult {
            ok,
            output: char_prefix(&redact(out), 20_000).to_owned(),
            duration_ms: ms_since(self.t0),
            problems,
        }
    }
}

/// `int((time.monotonic() - t0) * 1000)`.
#[must_use]
pub fn ms_since(t0: Instant) -> i64 {
    i64::try_from(t0.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// The ONE background run an adapter owns (Python `_LiveRun` + its task).
pub struct LiveRun {
    pub run_id: String,
    /// skill / script / suite name
    pub name: String,
    pub started: f64,
    pub handle: tokio::task::JoinHandle<()>,
}

/// `self._live` with `_live_run_id()`.
#[derive(Default)]
pub struct LiveSlot(std::sync::Mutex<Option<LiveRun>>);

impl LiveSlot {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<LiveRun>> {
        match self.0.lock_recovering() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// The run id of the live run this process is still running.
    #[must_use]
    pub fn run_id(&self) -> Option<String> {
        self.lock()
            .as_ref()
            .filter(|l| !l.handle.is_finished())
            .map(|l| l.run_id.clone())
    }

    /// (run_id, name, started) of the live run, when it still runs.
    #[must_use]
    pub fn info(&self) -> Option<(String, String, f64)> {
        self.lock()
            .as_ref()
            .filter(|l| !l.handle.is_finished())
            .map(|l| (l.run_id.clone(), l.name.clone(), l.started))
    }

    /// Start `fut` as the live run unless one is still running (checked and set under one lock, like Python's "no
    /// await between the check and create_task"). Returns false when busy.
    pub fn start(
        &self,
        run_id: &str,
        name: &str,
        started: f64,
        fut: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> bool {
        let mut g = self.lock();
        if g.as_ref().is_some_and(|l| !l.handle.is_finished()) {
            return false;
        }
        let handle = tokio::spawn(async move {
            crate::services::common::contained("background run", fut).await;
        });
        *g = Some(LiveRun {
            run_id: run_id.to_owned(),
            name: name.to_owned(),
            started,
            handle,
        });
        true
    }

    /// Wait for the live task to end (tests; Python `await asyncio.wait_for(a._live.task, t)`).
    pub async fn wait(&self, timeout_s: f64) -> bool {
        let deadline = Instant::now() + std::time::Duration::from_secs_f64(timeout_s.max(0.0));
        loop {
            if self.lock().as_ref().is_none_or(|l| l.handle.is_finished()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// `task.cancel()` and wait until the task is gone (tests; Python cancels on console stop).
    pub async fn cancel(&self) {
        let h = self.lock().as_ref().map(|l| l.handle.abort_handle());
        if let Some(h) = h {
            h.abort();
            while !h.is_finished() {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        }
    }

    /// `self._live = None` (tests).
    pub fn clear(&self) {
        *self.lock() = None;
    }
}

/// Writes the "interrupted by console stop" outcome when the live task is dropped before it finished (Python's
/// `except asyncio.CancelledError` branch: the runtime drops the task on shutdown, a test aborts it).
pub struct InterruptGuard {
    pub run_dir: PathBuf,
    pub meta: Arc<std::sync::Mutex<Map<String, Value>>>,
    pub t0: Instant,
    pub armed: bool,
    pub redact_keys: &'static [&'static str],
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut meta = match self.meta.lock_recovering() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        meta.insert("verdict".to_owned(), Value::from("interrupted"));
        meta.insert("interrupted_by".to_owned(), Value::from("console stop"));
        meta.insert("duration_ms".to_owned(), Value::from(ms_since(self.t0)));
        let _ = write_meta(&self.run_dir, &meta, self.redact_keys);
    }
}

/// The background task's `except Exception as e` branch: `f"{type(e).__name__}: {e}"[:300]`.
#[must_use]
pub fn exc_text(class: &str, msg: &str) -> String {
    char_prefix(&format!("{class}: {msg}"), 300).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realpath_like_python() {
        let tmp = tempfile::tempdir().unwrap();
        let root = realpath(tmp.path());
        std::fs::create_dir(root.join("a")).unwrap();
        std::os::unix::fs::symlink(root.join("a"), root.join("l")).unwrap();
        assert_eq!(realpath(&root.join("l/x/../y")), root.join("a/y"));
        assert_eq!(realpath(&root.join("missing/../b")), root.join("b"));
        std::os::unix::fs::symlink(root.join("loop"), root.join("loop")).unwrap();
        assert_eq!(realpath(&root.join("loop/z")), root.join("loop/z"));
    }

    #[test]
    fn os_error_texts() {
        let e = std::io::Error::from_raw_os_error(13);
        assert_eq!(
            os_error(&e, Some(Path::new("/x"))),
            (
                "PermissionError",
                "[Errno 13] Permission denied: '/x'".to_owned()
            )
        );
    }

    #[test]
    fn pystr_of_json() {
        let v = serde_json::json!({"a": [1, 2.0, null, true, "x"]});
        assert_eq!(pystr(Some(&v)), "{'a': [1, 2.0, None, True, 'x']}");
        assert_eq!(fixed(f64::NAN, 3, true), "+nan");
        assert_eq!(fixed(0.1597, 3, true), "+0.160");
    }
}
