//! ATIF trajectories: the index of the files, and the on-demand views of one file.
//!
//! The index keeps a summary per trajectory file (identifiers and numbers). The steps of a trajectory and the content
//! of a step are read from the file when asked for, with a streaming JSON reader: a file is never held in memory,
//! whatever its size.

use std::collections::HashMap;
use std::fmt;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::Deserialize;
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};

use super::ids::{NO_TS, fmt_uuid, is_ident, parse_uuid};
use super::lenient::{Count, Int, Num, Opt, Stamp, skip_kinds};
use super::reader::{hash128, open_in};
use super::snapshot::{count_json, f_json};
use super::text::{Maker, Node, Part};
use crate::adapters::relay::{MAX_SCAN_ENTRIES, ident_ok};

/// The source name of trajectories in `relay_atif_dir`.
pub const GATEWAY: &str = "gateway";
/// Trajectories kept in the index (the newest by modification time).
const MAX_TRAJECTORIES: usize = 100_000;
/// Steps a detail answer returns.
pub const MAX_STEPS: usize = 2000;
/// Span ids remembered per trajectory to link it to a trace.
const MAX_LINKS: usize = 8;

/// An identifier-shaped string, owned (trajectory files are read as a stream, so nothing can be borrowed).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Name(Option<String>);

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Name;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Name, E> {
                Ok(Name(is_ident(v).then(|| v.to_owned())))
            }
            skip_kinds!(bool, int, float, unit, seq, map);
        }
        d.deserialize_any(V)
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Agent {
    name: Name,
    version: Name,
    model_name: Name,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FinalMetrics {
    total_prompt_tokens: Int,
    total_completion_tokens: Int,
    total_cached_tokens: Int,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StepMetrics {
    prompt_tokens: Int,
    completion_tokens: Int,
    cached_tokens: Int,
    reasoning_tokens: Int,
    extra: Opt<MetricsExtra>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MetricsExtra {
    reasoning_tokens: Int,
    cached_tokens: Int,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Ancestry {
    function_id: Name,
    parent_id: Name,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Invocation {
    start_timestamp: Num,
    end_timestamp: Num,
    status: Name,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StepExtra {
    ancestry: Opt<Ancestry>,
    invocation: Opt<Invocation>,
}

/// The counted parts of a step (index time).
#[derive(Deserialize, Default)]
#[serde(default)]
struct StepLite {
    timestamp: Stamp,
    tool_calls: Count,
    metrics: Opt<StepMetrics>,
    extra: Opt<StepExtra>,
}

/// All steps folded into totals, one at a time.
#[derive(Default)]
struct StepFold {
    steps: u32,
    tool_calls: u32,
    first_us: i64,
    last_us: i64,
    prompt: i64,
    completion: i64,
    cached: i64,
    links: Vec<u128>,
}

fn add(sum: &mut i64, v: Option<i64>) {
    if let Some(v) = v.filter(|v| *v >= 0) {
        *sum = if *sum < 0 { v } else { sum.saturating_add(v) };
    }
}

impl<'de> Deserialize<'de> for StepFold {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = StepFold;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<StepFold, A::Error> {
                let mut f = StepFold {
                    first_us: NO_TS,
                    last_us: NO_TS,
                    prompt: -1,
                    completion: -1,
                    cached: -1,
                    ..StepFold::default()
                };
                while let Some(s) = a.next_element::<Opt<StepLite>>()? {
                    f.steps = f.steps.saturating_add(1);
                    let Some(s) = s.0 else { continue };
                    f.tool_calls = f.tool_calls.saturating_add(s.tool_calls.0.unwrap_or(0));
                    if let Some(ts) = s.timestamp.0 {
                        if f.first_us == NO_TS || ts < f.first_us {
                            f.first_us = ts;
                        }
                        if f.last_us == NO_TS || ts > f.last_us {
                            f.last_us = ts;
                        }
                    }
                    if let Some(m) = s.metrics.get() {
                        add(&mut f.prompt, m.prompt_tokens.0);
                        add(&mut f.completion, m.completion_tokens.0);
                        add(
                            &mut f.cached,
                            m.cached_tokens
                                .0
                                .or(m.extra.get().and_then(|e| e.cached_tokens.0)),
                        );
                    }
                    if let Some(an) = s.extra.get().and_then(|e| e.ancestry.get()) {
                        for id in [&an.function_id, &an.parent_id] {
                            if let Some(u) = id.0.as_deref().and_then(parse_uuid)
                                && f.links.len() < MAX_LINKS
                                && !f.links.contains(&u)
                            {
                                f.links.push(u);
                            }
                        }
                    }
                }
                Ok(f)
            }
            skip_kinds!(bool, int, float, str, unit, map);
        }
        d.deserialize_any(V)
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Doc {
    schema_version: Name,
    session_id: Name,
    trajectory_id: Name,
    agent: Opt<Agent>,
    steps: StepFold,
    final_metrics: Opt<FinalMetrics>,
}

/// One trajectory file, summarised.
#[derive(Clone, Debug)]
pub struct Traj {
    /// UUID value of the trajectory id.
    pub id: u128,
    pub source: String,
    pub file: String,
    pub bytes: u64,
    pub mtime_us: i64,
    pub schema_version: Option<String>,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub agent_model: Option<String>,
    pub session_id: Option<String>,
    pub steps: u32,
    pub tool_calls: u32,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub first_us: i64,
    pub last_us: i64,
    /// Span ids its steps name (`extra.ancestry`): how a trajectory finds its trace.
    pub links: Vec<u128>,
    dev: u64,
    ino: u64,
    mtime_ns: i128,
}

impl Traj {
    /// `TrajectorySummary`. `trace_id`: the indexed trace one of its steps points into.
    #[must_use]
    pub fn summary(&self, trace_id: Option<&str>) -> Value {
        #[allow(clippy::cast_precision_loss)]
        let secs = |us: i64| {
            if us == NO_TS {
                Value::Null
            } else {
                f_json(Some(us as f64 / 1e6))
            }
        };
        json!({
            "id": fmt_uuid(self.id),
            "source": self.source,
            "file": self.file,
            "bytes": self.bytes,
            "mtime": secs(self.mtime_us),
            "schema_version": self.schema_version,
            "agent": {"name": self.agent_name, "version": self.agent_version, "model": self.agent_model},
            "session_id": self.session_id,
            "steps": self.steps,
            "tool_calls": self.tool_calls,
            "prompt_tokens": count_json(self.prompt_tokens),
            "completion_tokens": count_json(self.completion_tokens),
            "cached_tokens": count_json(self.cached_tokens),
            "first_ts": secs(self.first_us),
            "last_ts": secs(self.last_us),
            "trace_id": trace_id,
        })
    }
}

/// The trajectories of one published snapshot, newest first.
#[derive(Default, Debug)]
pub struct TrajSet {
    pub items: Vec<Traj>,
    /// Files larger than `relay_trace_max_atif_bytes`.
    pub too_large: u64,
    /// Files that are not a readable trajectory (not JSON, no id).
    pub unreadable: u64,
}

/// The directories trajectories are read from.
#[derive(Clone, Debug, Default)]
pub struct TrajDirs {
    pub gateway: Option<PathBuf>,
    pub hermes: Option<PathBuf>,
}

impl TrajDirs {
    /// The directory of a source name: `gateway`, or an agent directory under the Hermes root. Checked when it is
    /// asked for (a request), not only when it was listed: `<agent>` and its `atif` must still be real directories,
    /// not symlinks, or there is no directory (the files are not read through a link that was put there since).
    #[must_use]
    pub fn dir_of(&self, source: &str) -> Option<PathBuf> {
        if source == GATEWAY {
            return self.gateway.clone();
        }
        if !agent_name_ok(source) {
            return None;
        }
        let agent = self.hermes.as_ref()?.join(source);
        let atif = agent.join("atif");
        let real_dir =
            |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
        (real_dir(&agent) && real_dir(&atif)).then_some(atif)
    }
}

/// `^[a-z][a-z0-9-]{0,40}$`, and not the name reserved for the gateway's own directory.
fn agent_name_ok(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=41).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && name != GATEWAY
}

struct Listed {
    source: String,
    dir: PathBuf,
    name: String,
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
}

fn list_json(dir: &Path, source: &str, out: &mut Vec<Listed>) {
    use std::os::unix::fs::MetadataExt;
    let Ok(it) = std::fs::read_dir(dir) else {
        return;
    };
    for (i, e) in it.enumerate() {
        if i >= MAX_SCAN_ENTRIES {
            break;
        }
        let Ok(e) = e else { continue };
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        if !name.ends_with(".json") || name.len() <= 5 {
            continue;
        }
        // not following links: a symlink is not a regular file here
        let Ok(md) = e.metadata() else { continue };
        if !md.file_type().is_file() {
            continue;
        }
        out.push(Listed {
            source: source.to_owned(),
            dir: dir.to_path_buf(),
            name,
            dev: md.dev(),
            ino: md.ino(),
            size: md.len(),
            mtime_ns: i128::from(md.mtime()) * 1_000_000_000 + i128::from(md.mtime_nsec()),
        });
    }
}

fn list_all(dirs: &TrajDirs) -> Vec<Listed> {
    let mut out = Vec::new();
    if let Some(d) = &dirs.gateway {
        list_json(d, GATEWAY, &mut out);
    }
    if let Some(root) = &dirs.hermes
        && let Ok(it) = std::fs::read_dir(root)
    {
        for (i, e) in it.enumerate() {
            if i >= 4096 {
                break;
            }
            let Ok(e) = e else { continue };
            let Ok(agent) = e.file_name().into_string() else {
                continue;
            };
            // a real directory with a plain lower-case name; `atif` under it likewise
            if !agent_name_ok(&agent) || !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let atif = root.join(&agent).join("atif");
            if std::fs::symlink_metadata(&atif).is_ok_and(|m| m.file_type().is_dir()) {
                list_json(&atif, &agent, &mut out);
            }
        }
    }
    out
}

/// A UUID inside a file name (`nemo-relay-atif-<uuid>.json`).
fn uuid_in_name(name: &str) -> Option<u128> {
    let b = name.as_bytes();
    (0..b.len().saturating_sub(35)).find_map(|i| name.get(i..i + 36).and_then(parse_uuid))
}

/// The id of a trajectory whose own id is not a UUID: `hash128("nemo-console/atif-id\0" + source + "\0" + id)` with
/// the version (8) and variant (10xx) bits of an RFC 9562 UUIDv8, so it reads and prints as any other UUID. The same
/// source and id give the same value in every build of the index.
#[must_use]
pub fn derived_id(source: &str, id: &str) -> u128 {
    let mut seed = Vec::with_capacity(22 + source.len() + id.len());
    seed.extend_from_slice(b"nemo-console/atif-id\0");
    seed.extend_from_slice(source.as_bytes());
    seed.push(0);
    seed.extend_from_slice(id.as_bytes());
    let h = hash128(&seed);
    // the 13th hex digit (bits 76..80) is the version; the top two bits of the 17th (bits 62, 63) are the variant
    let h = (h & !(0xF_u128 << 76)) | (0x8_u128 << 76);
    (h & !(0b11_u128 << 62)) | (0b10_u128 << 62)
}

fn summarise(f: &Listed, max_bytes: u64) -> Result<Traj, bool> {
    if f.size > max_bytes {
        return Err(true);
    }
    let opened = open_in(&f.dir, &f.name).map_err(|_| false)?;
    if opened.size > max_bytes {
        return Err(true);
    }
    let doc: Doc = serde_json::from_reader(BufReader::with_capacity(256 * 1024, opened.file))
        .map_err(|_| false)?;
    let id = doc
        .trajectory_id
        .0
        .as_deref()
        .and_then(parse_uuid)
        .or_else(|| doc.session_id.0.as_deref().and_then(parse_uuid))
        .or_else(|| uuid_in_name(&f.name))
        .or_else(|| {
            // Hermes names its trajectories `<word>_<hex>_<date>_<time>`: no UUID anywhere. Such an id still names
            // the file; it gets a derived UUID (stable, per source) so the routes can keep their UUID-shaped ids.
            let text = doc
                .trajectory_id
                .0
                .as_deref()
                .or(doc.session_id.0.as_deref())
                .filter(|s| ident_ok(s))?;
            Some(derived_id(&f.source, text))
        })
        .ok_or(false)?;
    let fin = doc.final_metrics.get();
    let pick = |total: Option<i64>, sum: i64| total.filter(|v| *v >= 0).unwrap_or(sum);
    let agent = doc.agent.0.unwrap_or_default();
    Ok(Traj {
        id,
        source: f.source.clone(),
        file: f.name.clone(),
        bytes: opened.size,
        mtime_us: i64::try_from(opened.mtime_ns / 1000).unwrap_or(NO_TS),
        schema_version: doc.schema_version.0,
        agent_name: agent.name.0,
        agent_version: agent.version.0,
        agent_model: agent.model_name.0,
        session_id: doc.session_id.0,
        steps: doc.steps.steps,
        tool_calls: doc.steps.tool_calls,
        prompt_tokens: pick(fin.and_then(|m| m.total_prompt_tokens.0), doc.steps.prompt),
        completion_tokens: pick(
            fin.and_then(|m| m.total_completion_tokens.0),
            doc.steps.completion,
        ),
        cached_tokens: pick(fin.and_then(|m| m.total_cached_tokens.0), doc.steps.cached),
        first_us: doc.steps.first_us,
        last_us: doc.steps.last_us,
        links: doc.steps.links,
        dev: opened.dev,
        ino: opened.ino,
        mtime_ns: opened.mtime_ns,
    })
}

/// The trajectory index between passes.
#[derive(Default)]
pub struct TrajScan {
    known: HashMap<(String, String), Traj>,
    /// Files that could not be summarised, with the identity they had (so they are not read again every pass).
    bad: HashMap<(String, String), (u64, i128, bool)>,
    pub set: Arc<TrajSet>,
}

impl TrajScan {
    /// Look at the directories again; read new and changed files. True when the published set changed.
    pub fn refresh(&mut self, dirs: &TrajDirs, max_bytes: u64, cancel: &AtomicBool) -> bool {
        let listing = list_all(dirs);
        let mut keep: HashMap<(String, String), Traj> = HashMap::new();
        let mut bad: HashMap<(String, String), (u64, i128, bool)> = HashMap::new();
        let mut todo: Vec<&Listed> = Vec::new();
        for f in &listing {
            let key = (f.source.clone(), f.name.clone());
            match self.known.get(&key) {
                Some(t)
                    if t.dev == f.dev
                        && t.ino == f.ino
                        && t.bytes == f.size
                        && t.mtime_ns == f.mtime_ns =>
                {
                    keep.insert(key, t.clone());
                }
                _ => match self.bad.get(&key) {
                    Some(&(size, mtime, large)) if size == f.size && mtime == f.mtime_ns => {
                        bad.insert(key, (size, mtime, large));
                    }
                    _ => todo.push(f),
                },
            }
        }
        let changed =
            keep.len() != self.known.len() || bad.len() != self.bad.len() || !todo.is_empty();
        // many files at first start: a few threads, each reading one file at a time through a small buffer
        let workers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(8)
            .min(todo.len().max(1));
        let next = AtomicUsize::new(0);
        let results: std::sync::Mutex<Vec<(usize, Result<Traj, bool>)>> =
            std::sync::Mutex::new(Vec::new());
        let work = || {
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(f) = todo.get(i) else { break };
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let r = summarise(f, max_bytes);
                if let Ok(mut g) = results.lock() {
                    g.push((i, r));
                }
            }
        };
        if workers <= 1 {
            work();
        } else {
            std::thread::scope(|s| {
                for _ in 0..workers {
                    s.spawn(work);
                }
            });
        }
        let done = results.into_inner().unwrap_or_default();
        if done.len() < todo.len() {
            // cancelled: keep the previous state, the next pass starts over
            return false;
        }
        for (i, r) in done {
            let Some(f) = todo.get(i) else { continue };
            let key = (f.source.clone(), f.name.clone());
            match r {
                Ok(t) => {
                    keep.insert(key, t);
                }
                Err(large) => {
                    bad.insert(key, (f.size, f.mtime_ns, large));
                }
            }
        }
        self.known = keep;
        self.bad = bad;
        if !changed {
            return false;
        }
        // one entry per (source, id): the newest file wins; newest first overall
        let mut items: Vec<Traj> = self.known.values().cloned().collect();
        items.sort_by(|a, b| {
            (b.mtime_us, &a.source, a.id, &a.file).cmp(&(a.mtime_us, &b.source, b.id, &b.file))
        });
        let mut seen: std::collections::HashSet<(String, u128)> = std::collections::HashSet::new();
        items.retain(|t| seen.insert((t.source.clone(), t.id)));
        items.truncate(MAX_TRAJECTORIES);
        self.set = Arc::new(TrajSet {
            items,
            too_large: self.bad.values().filter(|b| b.2).count() as u64,
            unreadable: self.bad.values().filter(|b| !b.2).count() as u64,
        });
        true
    }
}

// ── one file, on demand ─────────────────────────────────────────────────────────────────────────

/// Why reading a trajectory file for a request failed.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The file is gone, changed into something unreadable, or grew past the size limit.
    Unavailable,
    /// No step with that id.
    NoSuchStep,
}

/// Calls `each` with every step of the document (a bounded copy of it), stopping early when it returns false.
struct StepsSeed<'f>(&'f mut dyn FnMut(Node) -> bool);

/// The error text a visitor uses to stop reading the rest of a file.
const STOP: &str = "\u{1}stop";

impl<'de> DeserializeSeed<'de> for StepsSeed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StepsSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("anything")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        let each = self.0;
        while let Some(step) = a.next_element::<Node>()? {
            if !each(step) {
                return Err(de::Error::custom(STOP));
            }
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<(), A::Error> {
        super::lenient::drain_map(a)
    }
    fn visit_bool<E: de::Error>(self, _v: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _v: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _v: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _v: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, _v: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}

struct DocSeed<'f>(&'f mut dyn FnMut(Node) -> bool);

impl<'de> DeserializeSeed<'de> for DocSeed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for DocSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a trajectory object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        let each = self.0;
        while let Some(k) = a.next_key::<std::borrow::Cow<'de, str>>()? {
            if k == "steps" {
                a.next_value_seed(StepsSeed(&mut *each))?;
            } else {
                a.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// Walk the steps of a trajectory file. The file is opened through the same checks as every capture file and must
/// still be the one the index saw (same size limit).
fn walk_steps(
    dir: &Path,
    t: &Traj,
    max_bytes: u64,
    each: &mut dyn FnMut(Node) -> bool,
) -> Result<(), ReadError> {
    let opened = open_in(dir, &t.file).map_err(|_| ReadError::Unavailable)?;
    if opened.size > max_bytes {
        return Err(ReadError::Unavailable);
    }
    // the file the index summarised, not one written or moved there since (same device, inode, size and time)
    if (opened.dev, opened.ino, opened.size, opened.mtime_ns) != (t.dev, t.ino, t.bytes, t.mtime_ns)
    {
        return Err(ReadError::Unavailable);
    }
    let mut de =
        serde_json::Deserializer::from_reader(BufReader::with_capacity(256 * 1024, opened.file));
    match DocSeed(each).deserialize(&mut de) {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().starts_with(STOP) => Ok(()),
        Err(_) => Err(ReadError::Unavailable),
    }
}

fn node_int(n: Option<&Node>) -> Option<i64> {
    match n? {
        Node::Num(t) => t.parse::<i64>().ok().or_else(|| {
            #[allow(clippy::cast_possible_truncation)]
            t.parse::<f64>()
                .ok()
                .filter(|f| f.fract() == 0.0 && f.abs() < 9e15)
                .map(|f| f as i64)
        }),
        _ => None,
    }
}

fn node_f(n: Option<&Node>) -> Option<f64> {
    match n? {
        Node::Num(t) => t.parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

fn node_ident(n: Option<&Node>) -> Value {
    n.and_then(Node::short)
        .filter(|s| is_ident(s))
        .map_or(Value::Null, |s| Value::String(s.to_owned()))
}

fn node_ts(n: Option<&Node>) -> Value {
    #[allow(clippy::cast_precision_loss)]
    let us = match n {
        Some(Node::Str(t)) if t.chars <= 64 => {
            super::ids::parse_rfc3339_us(&t.text).and_then(super::ids::ts_in_range)
        }
        Some(Node::Num(t)) => t
            .parse::<f64>()
            .ok()
            .and_then(super::ids::number_us)
            .and_then(super::ids::ts_in_range),
        _ => None,
    };
    #[allow(clippy::cast_precision_loss)]
    us.map_or(Value::Null, |us| f_json(Some(us as f64 / 1e6)))
}

/// `TrajectoryStep`: numbers and identifiers of one step, no text.
fn step_meta(s: &Node) -> Value {
    let metrics = s.get("metrics");
    let m = |k: &str| {
        node_int(metrics.and_then(|x| x.get(k)))
            .or_else(|| node_int(metrics.and_then(|x| x.get("extra")).and_then(|e| e.get(k))))
            .filter(|v| *v >= 0)
            .map_or(Value::Null, |v| json!(v))
    };
    let extra = s.get("extra");
    let inv = extra.and_then(|e| e.get("invocation"));
    let duration = match (
        node_f(inv.and_then(|i| i.get("start_timestamp"))),
        node_f(inv.and_then(|i| i.get("end_timestamp"))),
    ) {
        (Some(a), Some(b)) if b >= a => f_json(Some(((b - a) * 1e6).round() / 1e3)),
        _ => Value::Null,
    };
    let span_id = extra
        .and_then(|e| e.get("ancestry"))
        .and_then(|a| a.get("function_id"))
        .and_then(Node::short)
        .and_then(parse_uuid)
        .map_or(Value::Null, |u| Value::String(fmt_uuid(u)));
    let calls: Vec<Value> = s
        .get("tool_calls")
        .map(Node::items)
        .unwrap_or_default()
        .iter()
        .map(|c| {
            json!({
                "id": node_ident(c.get("tool_call_id").or_else(|| c.get("id"))),
                "name": node_ident(c.get("function_name").or_else(|| c.get("name"))),
                "arguments_chars": c.get("arguments").map_or(0, Node::text_chars),
            })
        })
        .collect();
    let observations = match s.get("observation").and_then(|o| o.get("results")) {
        Some(Node::Arr { total, .. }) => u64::from(*total),
        _ => 0,
    };
    json!({
        "step_id": node_int(s.get("step_id")),
        "ts": node_ts(s.get("timestamp")),
        "source": node_ident(s.get("source")),
        "model": node_ident(s.get("model_name")),
        "message_chars": message_chars(s.get("message")),
        "reasoning_chars": s.get("reasoning_content").map_or(0, Node::text_chars),
        "tool_calls": calls,
        "observations": observations,
        "metrics": {
            "prompt_tokens": m("prompt_tokens"),
            "completion_tokens": m("completion_tokens"),
            "cached_tokens": m("cached_tokens"),
            "reasoning_tokens": m("reasoning_tokens"),
        },
        "duration_ms": duration,
        "span_id": span_id,
        "status": node_ident(inv.and_then(|i| i.get("status"))),
    })
}

/// Characters of a step's message: a string, or the text parts of a list of parts.
fn message_chars(n: Option<&Node>) -> u64 {
    match n {
        Some(Node::Str(t)) => t.chars,
        Some(Node::Arr { items, .. }) => items
            .iter()
            .map(|p| match p {
                Node::Str(t) => t.chars,
                other => other.get("text").and_then(Node::str).map_or(0, |t| t.chars),
            })
            .sum(),
        _ => 0,
    }
}

/// `{steps: TrajectoryStep[] (<= 2000), truncated}` of one trajectory file.
pub fn steps(dir: &Path, t: &Traj, max_bytes: u64) -> Result<(Vec<Value>, bool), ReadError> {
    let mut out = Vec::new();
    let mut more = false;
    walk_steps(dir, t, max_bytes, &mut |s| {
        if out.len() >= MAX_STEPS {
            more = true;
            return false;
        }
        out.push(step_meta(&s));
        true
    })?;
    Ok((out, more))
}

/// `StepContent` of one step.
pub fn step_content(
    dir: &Path,
    t: &Traj,
    max_bytes: u64,
    step_id: i64,
    max_chars: usize,
) -> Result<Value, ReadError> {
    let mut found: Option<Node> = None;
    walk_steps(dir, t, max_bytes, &mut |s| {
        if node_int(s.get("step_id")) == Some(step_id) {
            found = Some(s);
            return false;
        }
        true
    })?;
    let s = found.ok_or(ReadError::NoSuchStep)?;
    let mut mk = Maker::new(max_chars);
    let one = |mk: &mut Maker, n: Option<&Node>, kind: &'static str| -> Value {
        let mut parts: Vec<Part> = Vec::new();
        if let Some(n) = n {
            mk.content(n, kind, &mut parts);
        }
        // a step has one message: several text parts are shown as one
        match parts.len() {
            0 => Value::Null,
            1 => parts[0].json(),
            _ => {
                let texts: Vec<&str> = parts.iter().filter_map(|p| p.text.as_deref()).collect();
                let joined = Part {
                    kind,
                    text: Some(texts.join("\n")),
                    name: None,
                    call_id: None,
                    chars: parts.iter().map(|p| p.chars).sum(),
                    truncated: parts.iter().any(|p| p.truncated),
                };
                joined.json()
            }
        }
    };
    let message = one(&mut mk, s.get("message"), "text");
    let reasoning = one(&mut mk, s.get("reasoning_content"), "reasoning");
    let tool_calls: Vec<Value> = s
        .get("tool_calls")
        .map(Node::items)
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let args = match c.get("arguments") {
                Some(a) => mk.value("tool_call", a),
                None => Maker::sized("tool_call", 0),
            };
            json!({
                "id": mk.label(c.get("tool_call_id").or_else(|| c.get("id"))),
                "name": mk.label(c.get("function_name").or_else(|| c.get("name"))),
                "arguments": args.json(),
            })
        })
        .collect();
    let observations: Vec<Value> = s
        .get("observation")
        .and_then(|o| o.get("results"))
        .map(Node::items)
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let content = one(&mut mk, r.get("content"), "tool_result");
            json!({
                "call_id": mk.label(r.get("source_call_id")),
                "content": if content.is_null() { Maker::sized("tool_result", 0).json() } else { content },
            })
        })
        .collect();
    let mut v = json!({
        "step_id": step_id,
        "message": message,
        "reasoning": reasoning,
        "tool_calls": tool_calls,
        "observations": observations,
        "truncated": false,
        "redactions": 0,
    });
    let cut = super::text::fit(&mut v, super::content::ANSWER_TEXT_BUDGET);
    v["truncated"] = json!(mk.truncated || cut);
    v["redactions"] = json!(mk.redactions);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_directory_names() {
        for ok in ["hermes", "a", "agent-2", &format!("a{}", "b".repeat(40))] {
            assert!(agent_name_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "Hermes",
            "2agent",
            "a_b",
            "a.b",
            "..",
            "gateway",
            &format!("a{}", "b".repeat(41)),
        ] {
            assert!(!agent_name_ok(bad), "{bad}");
        }
        assert_eq!(
            uuid_in_name("nemo-relay-atif-01a0f1e0-04e0-7209-88c4-de6f7c994b63.json"),
            parse_uuid("01a0f1e0-04e0-7209-88c4-de6f7c994b63")
        );
        assert_eq!(uuid_in_name("notes.json"), None);
    }

    #[test]
    fn derived_ids_are_uuidv8_stable_and_per_source() {
        let a = derived_id("hermes", "chat_0a1b2c_20261110_093000");
        let text = fmt_uuid(a);
        assert_eq!(parse_uuid(&text), Some(a));
        assert_eq!(text.as_bytes()[14], b'8', "version 8: {text}");
        assert!(
            matches!(text.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "variant 10xx: {text}"
        );
        assert_eq!(a, derived_id("hermes", "chat_0a1b2c_20261110_093000"));
        assert_ne!(a, derived_id("hermes-2", "chat_0a1b2c_20261110_093000"));
        assert_ne!(a, derived_id("hermes", "chat_0a1b2c_20261110_093001"));
    }
}
