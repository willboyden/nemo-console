//! OpenShell, wave 4 (addendum 9.16, PORTING.md D-42): the policy decisions of a sandbox's log and the persisted
//! sandbox lifecycle events. Pure functions; the adapter (`adapters/openshell.rs`) runs the CLI, holds the caches and
//! calls these on the blocking pool.
//!
//! Decisions: `openshell logs NAME` prints OCSF lines such as
//! `[1792841081.496] [sandbox] [OCSF ] [ocsf] SSH:OPEN [INFO] ALLOWED` (tests/fixtures/openshell/logs.txt). A row is
//! made only from a line whose token after the level is a verdict (`ALLOWED` / `DENIED`, also `ALLOW`, `DENY`,
//! `BLOCKED`); its `category` / `kind` come from FIXED tables (anything else is `other`), `target` is the next token
//! only when it is `host:port` or a path-free identifier (at most 120 characters), else null. No other text of the
//! line (the message, a command line, a path, a key=value list) is ever copied. At most [`DECISIONS_MAX_ROWS`] rows
//! (the newest), every line looked at only up to [`DECISION_LINE_PREFIX`] bytes.
//!
//! Events: `<state>/openshell/events.jsonl`, one [`SystemEvent`] per line, written by ONE writer (the adapter's
//! persist lock), 0600, never through a symlink or a hard link (`runviz::RunDir`); past [`EVENTS_REWRITE_AT`] lines it
//! is rewritten to the newest [`EVENTS_KEEP`]. Read back after a restart with byte and line caps; every field is
//! checked again (the file is data, not trusted) and the summary is rebuilt from the checked fields.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::adapters::runviz::RunDir;
use crate::domain::models::{SystemEvent, SystemId};
use crate::services::support::full;

// ── decisions ───────────────────────────────────────────────────────────────────────────────────

/// The most rows a decisions answer holds (the newest ones; `limit` is 1..=this).
pub const DECISIONS_MAX_ROWS: usize = 2000;
/// The default `limit`.
pub const DECISIONS_DEFAULT_LIMIT: usize = 500;
/// Log lines asked of `openshell logs -n` for the decisions.
pub const DECISIONS_LOG_LINES: usize = 2000;
/// The CLI's output cap for the decisions call (the `sandbox_logs` action's cap).
pub const DECISIONS_MAX_BYTES: usize = 512_000;
/// How long one sandbox's decisions are served from memory.
pub const DECISIONS_CACHE_S: f64 = 10.0;
/// Only this many bytes of a line are looked at (a verdict sits in the first hundred).
pub const DECISION_LINE_PREFIX: usize = 1024;
/// The longest `target` kept.
pub const TARGET_MAX_CHARS: usize = 120;

/// OCSF event class prefix -> category (fixed; anything else is `other`).
pub const CATEGORIES: [(&str, &str); 12] = [
    ("SSH", "ssh"),
    ("CONFIG", "config"),
    ("NET", "network"),
    ("NETWORK", "network"),
    ("HTTP", "http"),
    ("DNS", "dns"),
    ("PROC", "process"),
    ("PROCESS", "process"),
    ("FILE", "filesystem"),
    ("FS", "filesystem"),
    ("POLICY", "policy"),
    ("AUTH", "auth"),
];
/// OCSF activity -> kind (fixed; anything else is `other`).
pub const KINDS: [(&str, &str); 11] = [
    ("OPEN", "open"),
    ("CLOSE", "close"),
    ("CONNECT", "connect"),
    ("REQUEST", "request"),
    ("APPLYING", "applying"),
    ("BUILT", "built"),
    ("EXEC", "exec"),
    ("READ", "read"),
    ("WRITE", "write"),
    ("RESOLVE", "resolve"),
    ("LOAD", "load"),
];

/// `[ts] [source] [OCSF ] [ocsf] CLASS:ACTIVITY [LEVEL] VERDICT [TARGET]` (the rest of the line is never captured).
static OCSF_RE: LazyLock<Regex> = LazyLock::new(|| {
    full(
        r"\[([0-9]{1,12}(?:\.[0-9]{1,9})?)\]\s+\[[A-Za-z0-9_.:-]{0,32}\]\s+\[OCSF\s*\]\s+\[ocsf\]\s+([A-Z][A-Z0-9_]{0,31}):([A-Z][A-Z0-9_]{0,31})\s+\[[A-Z]{1,8}\s*\]\s+([A-Z]{1,16})(?:\s+(\S{1,256}))?(?:\s.*)?",
    )
});
/// `host:port` (a DNS name or an IPv4 address, or `[ipv6]`, and a port).
static HOST_PORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    full(r"(?:[A-Za-z0-9](?:[A-Za-z0-9.-]{0,100}[A-Za-z0-9])?|\[[0-9A-Fa-f:.]{2,45}\]):[0-9]{1,5}")
});
/// A path-free identifier.
static IDENT_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,119}"));

fn lookup(table: &[(&str, &'static str)], key: &str) -> &'static str {
    table
        .iter()
        .find(|(k, _)| *k == key)
        .map_or("other", |(_, v)| *v)
}

fn verdict(token: &str) -> Option<&'static str> {
    match token {
        "ALLOWED" | "ALLOW" => Some("allowed"),
        "DENIED" | "DENY" | "BLOCKED" => Some("denied"),
        _ => None,
    }
}

/// The decision of one log line (`None`: not an OCSF decision line).
#[must_use]
pub fn decision(line: &str) -> Option<Value> {
    let mut end = line.len().min(DECISION_LINE_PREFIX);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let head = line[..end].trim_end_matches(['\r', '\n']);
    let c = OCSF_RE.captures(head)?;
    let ts: f64 = c.get(1)?.as_str().parse().ok()?;
    if !ts.is_finite() {
        return None;
    }
    let verdict = verdict(c.get(4)?.as_str())?;
    let target = c.get(5).map(|m| m.as_str()).filter(|t| {
        t.len() <= TARGET_MAX_CHARS && (HOST_PORT_RE.is_match(t) || IDENT_RE.is_match(t))
    });
    Some(json!({
        "ts": ts,
        "category": lookup(&CATEGORIES, c.get(2)?.as_str()),
        "verdict": verdict,
        "kind": lookup(&KINDS, c.get(3)?.as_str()),
        "target": target,
    }))
}

/// The decisions of a log text, oldest first: at most [`DECISIONS_MAX_ROWS`] (the newest), and whether more were
/// found than kept.
#[must_use]
pub fn decisions(text: &str) -> (Vec<Value>, bool) {
    let mut rows: VecDeque<Value> = VecDeque::new();
    let mut dropped = false;
    for line in text.split('\n') {
        if let Some(row) = decision(line) {
            if rows.len() == DECISIONS_MAX_ROWS {
                rows.pop_front();
                dropped = true;
            }
            rows.push_back(row);
        }
    }
    (rows.into(), dropped)
}

// ── persisted sandbox events ────────────────────────────────────────────────────────────────────

/// The file under `<state>/openshell/`.
pub const EVENTS_FILE: &str = "events.jsonl";
/// Lines kept after a rewrite.
pub const EVENTS_KEEP: usize = 5000;
/// A rewrite happens when the file grows past this many lines.
pub const EVENTS_REWRITE_AT: usize = 6000;
/// The most bytes read back (the tail of the file). A line we write is well under 1 KiB.
pub const EVENTS_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// A longer line is skipped.
pub const EVENT_LINE_MAX: usize = 4096;
/// The four lifecycle actions the adapter emits.
pub const ACTIONS: [&str; 4] = ["present", "created", "phase", "deleted"];

static NAME_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}"));
static SANDBOX_ID_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9._:-]{0,64}"));
static PHASE_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9 _.-]{0,24}"));

/// The event's checked fields: (name, id, phase, action, previous_phase).
type Fields<'a> = (&'a str, &'a str, &'a str, &'a str, Option<&'a str>);

fn fields(data: &Map<String, Value>) -> Option<Fields<'_>> {
    let name = data.get("name")?.as_str().filter(|s| NAME_RE.is_match(s))?;
    let id = data
        .get("id")?
        .as_str()
        .filter(|s| SANDBOX_ID_RE.is_match(s))?;
    let phase = data
        .get("phase")?
        .as_str()
        .filter(|s| PHASE_RE.is_match(s))?;
    let action = data
        .get("action")?
        .as_str()
        .filter(|s| ACTIONS.contains(s))?;
    let previous = match data.get("previous_phase") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if PHASE_RE.is_match(s) => Some(s.as_str()),
        Some(_) => return None,
    };
    Some((name, id, phase, action, previous))
}

/// The summary the adapter writes for these fields.
#[must_use]
pub fn summary(name: &str, phase: &str, action: &str, previous: Option<&str>) -> String {
    match action {
        "phase" => format!("sandbox {name}: {} -> {phase}", previous.unwrap_or("")),
        "deleted" => format!("sandbox {name} deleted (was {phase})"),
        _ => format!("sandbox {name} {action} ({phase})"),
    }
}

/// The checked event rebuilt from its fields (`None`: a field fails its check, a non-finite time).
#[must_use]
pub fn checked_event(ts: f64, kind: &str, data: &Map<String, Value>) -> Option<SystemEvent> {
    if !ts.is_finite() || kind != super::openshell::EVENT_KIND {
        return None;
    }
    let (name, id, phase, action, previous) = fields(data)?;
    Some(SystemEvent {
        ts,
        system: SystemId::Openshell,
        kind: kind.to_owned(),
        summary: crate::services::pystr::head(
            &crate::services::common::redact(&summary(name, phase, action, previous)),
            300,
        )
        .to_owned(),
        data: match json!({"name": name, "id": id, "phase": phase, "action": action, "previous_phase": previous})
        {
            Value::Object(m) => m,
            _ => Map::new(),
        },
    })
}

/// What was read back: the checked events (oldest first) and the number of lines the file holds now.
#[derive(Debug, Default)]
pub struct Loaded {
    pub events: Vec<SystemEvent>,
    pub lines: usize,
}

/// The newest complete lines of the tail of the file (at most `keep`, each at most [`EVENT_LINE_MAX`] bytes; a cut
/// tail loses its first, partial line).
fn tail_lines(bytes: &[u8], cut: bool) -> Vec<&[u8]> {
    let mut parts: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if cut && !parts.is_empty() {
        parts.remove(0);
    }
    parts.retain(|l| !l.is_empty());
    parts
}

/// Read `<dir>/events.jsonl` back (blocking). An absent file, a symlinked directory or file, a hard link: nothing.
#[must_use]
pub fn load(dir: &Path) -> Loaded {
    let Some(d) = RunDir::open(dir) else {
        return Loaded::default();
    };
    let Some((bytes, cut)) = d.read_tail(EVENTS_FILE, EVENTS_MAX_BYTES) else {
        return Loaded::default();
    };
    let lines = tail_lines(&bytes, cut);
    let mut events = Vec::new();
    for l in &lines {
        if l.len() > EVENT_LINE_MAX {
            continue;
        }
        let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(l) else {
            continue;
        };
        let (Some(ts), Some(kind), Some(Value::Object(data))) = (
            m.get("ts").and_then(Value::as_f64),
            m.get("kind").and_then(Value::as_str),
            m.get("data"),
        ) else {
            continue;
        };
        if m.get("system").and_then(Value::as_str) != Some("openshell") {
            continue;
        }
        if let Some(ev) = checked_event(ts, kind, data) {
            events.push(ev);
        }
    }
    Loaded {
        events,
        lines: lines.len(),
    }
}

/// Append `events` (checked again; others are not written) to `<dir>/events.jsonl`, creating `dir` (0700) and the
/// file (0600). `lines` = the lines the file holds before; returns the lines it holds after, rewriting it to the
/// newest [`EVENTS_KEEP`] lines when it grew past [`EVENTS_REWRITE_AT`]. Blocking; ONE writer at a time (the
/// adapter's persist lock).
pub fn append(dir: &Path, events: &[SystemEvent], lines: usize) -> std::io::Result<usize> {
    use std::io::Write as _;
    let mut text = Vec::new();
    let mut n = 0usize;
    for ev in events {
        let Some(ev) = checked_event(ev.ts, &ev.kind, &ev.data) else {
            continue;
        };
        let Ok(line) = serde_json::to_vec(&ev) else {
            continue;
        };
        text.extend_from_slice(&line);
        text.push(b'\n');
        n += 1;
    }
    if n == 0 {
        return Ok(lines);
    }
    crate::services::plumbing::mkdir_leaf(dir, 0o700)?;
    let d = RunDir::open(dir).ok_or_else(|| std::io::Error::other("not a directory"))?;
    let mut f = d.append_private(EVENTS_FILE)?;
    f.write_all(&text)?;
    drop(f);
    let total = lines + n;
    if total <= EVENTS_REWRITE_AT {
        return Ok(total);
    }
    let Some((bytes, cut)) = d.read_tail(EVENTS_FILE, EVENTS_MAX_BYTES) else {
        return Ok(total);
    };
    let all = tail_lines(&bytes, cut);
    let keep = &all[all.len().saturating_sub(EVENTS_KEEP)..];
    let mut out = Vec::with_capacity(keep.iter().map(|l| l.len() + 1).sum());
    for l in keep {
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    d.replace_private(EVENTS_FILE, &out)?;
    Ok(keep.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_lines() {
        let text = "[1792841081.496] [sandbox] [OCSF ] [ocsf] SSH:OPEN [INFO] ALLOWED\n\
            [1792841081.587] [sandbox] [OCSF ] [ocsf] CONFIG:APPLYING [INFO] Applying Landlock filesystem sandbox [abi:V2 compat:BestEffort ro:8 rw:2]\n\
            [1792841081.993] [sandbox] [INFO ] [openshell_supervisor_network::run] Container filesystem accessible pid=87\n";
        let (rows, more) = decisions(text);
        assert!(!more);
        assert_eq!(
            rows,
            [
                json!({"ts": 1_792_841_081.496, "category": "ssh", "verdict": "allowed", "kind": "open", "target": null})
            ]
        );
    }

    #[test]
    fn targets_are_checked() {
        let l = |t: &str| {
            decision(&format!(
                "[1.5] [sandbox] [OCSF ] [ocsf] NET:CONNECT [WARN] DENIED {t} rest of the message"
            ))
            .unwrap()["target"]
                .clone()
        };
        assert_eq!(l("api.example.com:443"), "api.example.com:443");
        assert_eq!(l("[::1]:8080"), "[::1]:8080");
        assert_eq!(l("curl"), "curl");
        assert_eq!(l("/usr/bin/curl"), Value::Null);
        assert_eq!(l("cmd=/bin/sh"), Value::Null);
        assert_eq!(l(&"a".repeat(121)), Value::Null);
    }
}
