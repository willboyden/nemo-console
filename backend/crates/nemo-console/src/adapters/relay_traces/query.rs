//! The list and detail answers over a snapshot: traces, sessions, trajectories.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::derive::{SessionRow, Status, TraceRow};
use super::ids::{Id, NO_TS, OddTable, Sym, fmt_uuid, parse_uuid};
use super::rows::{NO_ROW, bit};
use super::snapshot::{Core, Snapshot, count_json, f_json, ms_json};

/// The time window of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    H1,
    H6,
    H24,
    D7,
    D30,
    All,
}

pub const WINDOWS: [(&str, Window); 6] = [
    ("1h", Window::H1),
    ("6h", Window::H6),
    ("24h", Window::H24),
    ("7d", Window::D7),
    ("30d", Window::D30),
    ("all", Window::All),
];

impl Window {
    fn seconds(self) -> Option<i64> {
        Some(match self {
            Window::H1 => 3600,
            Window::H6 => 6 * 3600,
            Window::H24 => 24 * 3600,
            Window::D7 => 7 * 86_400,
            Window::D30 => 30 * 86_400,
            Window::All => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Range {
    pub window: Window,
    /// The window ends now instead of at the newest indexed event.
    pub anchor_now: bool,
}

/// A resolved window in epoch microseconds.
#[derive(Clone, Copy, Debug)]
pub struct Span64 {
    pub from: i64,
    pub to: i64,
}

impl Span64 {
    #[must_use]
    pub fn json(self) -> Value {
        #[allow(clippy::cast_precision_loss)]
        let f = |us: i64| f_json(Some(us as f64 / 1e6));
        json!({"from": f(self.from), "to": f(self.to)})
    }
}

impl Core {
    /// Where a window lies. `latest` ends at the newest indexed event, so an idle lab still shows its last activity;
    /// with nothing indexed it ends now.
    #[must_use]
    pub fn resolve(&self, r: Range, now_us: i64) -> Span64 {
        let to = if r.anchor_now || self.d.last_us == NO_TS {
            now_us
        } else {
            self.d.last_us
        };
        let from = match r.window.seconds() {
            Some(s) => to - s * 1_000_000,
            None if self.d.first_us == NO_TS => to,
            None => self.d.first_us.min(to),
        };
        Span64 { from, to }
    }

    /// Traces that overlap the window: started before its end, last heard of after its start.
    pub fn traces_in(&self, w: Span64) -> impl Iterator<Item = (u32, &TraceRow)> {
        let hi = self.d.traces.partition_point(|t| t.t0_us <= w.to);
        self.d.traces[..hi]
            .iter()
            .enumerate()
            .filter(move |(_, t)| t.last_us >= w.from)
            .filter_map(|(i, t)| Some((u32::try_from(i).ok()?, t)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceSort {
    Start,
    Duration,
    Tokens,
    LlmCalls,
}

#[derive(Clone, Debug)]
pub struct TraceQuery {
    pub range: Range,
    pub client: Option<String>,
    pub model: Option<String>,
    pub kind: Option<String>,
    pub status: Option<Status>,
    pub session: Option<String>,
    /// Lower-case needle.
    pub q: Option<String>,
    pub min_ms: i64,
    pub sort: TraceSort,
    pub desc: bool,
    pub limit: usize,
    pub offset: usize,
}

/// `[{value, count}]`: most frequent first, then by value (a null value after the named ones), at most 50 entries.
fn facet(core: &Core, counts: HashMap<Sym, u64>) -> Value {
    let mut rows: Vec<(u64, Option<&str>)> = counts
        .into_iter()
        .map(|(s, n)| (n, core.sym_str(s)))
        .collect();
    rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.is_none().cmp(&b.1.is_none()))
            .then(a.1.cmp(&b.1))
    });
    rows.truncate(50);
    Value::Array(
        rows.into_iter()
            .map(|(n, v)| json!({"value": v, "count": n}))
            .collect(),
    )
}

/// Nulls sort last whatever the direction.
fn by_key<T, K: Ord>(items: &mut [T], desc: bool, key: impl Fn(&T) -> Option<K>) {
    items.sort_by(|a, b| match (key(a), key(b)) {
        (Some(x), Some(y)) => {
            if desc {
                y.cmp(&x)
            } else {
                x.cmp(&y)
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    hay.len() >= needle_lower.len() && hay.to_ascii_lowercase().contains(needle_lower)
}

/// `GET /traces`.
#[must_use]
pub fn traces(snap: &Snapshot, q: &TraceQuery, now_us: i64) -> Value {
    let core = &*snap.core;
    let w = core.resolve(q.range, now_us);
    let in_window: Vec<(u32, &TraceRow)> = core.traces_in(w).collect();
    // facets describe the window, not the filtered list: they are what the filters are chosen from
    let (mut f_client, mut f_model, mut f_kind) = (HashMap::new(), HashMap::new(), HashMap::new());
    let mut f_status = [0u64; 3];
    for (_, t) in &in_window {
        // a trace without a known client counts under the null value
        *f_client.entry(t.client).or_insert(0u64) += 1;
        *f_kind.entry(t.kind).or_insert(0u64) += 1;
        for m in &t.models {
            *f_model.entry(*m).or_insert(0u64) += 1;
        }
        f_status[t.status as usize] += 1;
    }
    let want =
        |name: &Option<String>, s: Sym| name.as_deref().is_none_or(|n| core.sym_str(s) == Some(n));
    let needle_is_hex =
        q.q.as_deref()
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
    let mut rows: Vec<(u32, &TraceRow)> = in_window
        .into_iter()
        .filter(|(_, t)| {
            want(&q.client, t.client)
                && want(&q.kind, t.kind)
                && q.status.is_none_or(|s| s == t.status)
                && q.model
                    .as_deref()
                    .is_none_or(|m| t.models.iter().any(|s| core.sym_str(*s) == Some(m)))
                && q.session
                    .as_deref()
                    .is_none_or(|k| core.key_text(t.key).as_deref() == Some(k))
                && (q.min_ms <= 0
                    || (t.end_us != NO_TS
                        && t.t0_us != NO_TS
                        && (t.end_us - t.t0_us) >= q.min_ms * 1000))
                && q.q.as_deref().is_none_or(|n| {
                    core.span(t.root)
                        .and_then(|s| core.sym_str(s.name))
                        .is_some_and(|x| contains_ci(x, n))
                        || t.models
                            .iter()
                            .any(|s| core.sym_str(*s).is_some_and(|x| contains_ci(x, n)))
                        || core.key_text(t.key).is_some_and(|x| contains_ci(&x, n))
                        || (needle_is_hex
                            && core
                                .odd
                                .text(core.trace_id(t))
                                .is_some_and(|x| contains_ci(&x, n)))
                })
        })
        .collect();
    match q.sort {
        // the list is already in start order
        TraceSort::Start => {
            if q.desc {
                rows.reverse();
            }
        }
        TraceSort::Duration => by_key(&mut rows, q.desc, |(_, t)| {
            (t.end_us != NO_TS && t.t0_us != NO_TS).then(|| t.end_us - t.t0_us)
        }),
        TraceSort::Tokens => by_key(&mut rows, q.desc, |(_, t)| {
            (t.total >= 0).then_some(t.total)
        }),
        TraceSort::LlmCalls => by_key(&mut rows, q.desc, |(_, t)| Some(t.llm_calls)),
    }
    let total = rows.len();
    let items: Vec<Value> = rows
        .iter()
        .skip(q.offset)
        .take(q.limit)
        .map(|(_, t)| core.trace_summary(t))
        .collect();
    let status_facet: Vec<Value> = {
        let mut rows: Vec<(u64, &str)> = [Status::Ok, Status::Error, Status::Open]
            .into_iter()
            .map(|s| (f_status[s as usize], s.as_str()))
            .filter(|(n, _)| *n > 0)
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        rows.into_iter()
            .map(|(n, v)| json!({"value": v, "count": n}))
            .collect()
    };
    json!({
        "range": w.json(),
        "total": total,
        "offset": q.offset,
        "limit": q.limit,
        "items": items,
        "facets": {
            "client": facet(core, f_client),
            "model": facet(core, f_model),
            "kind": facet(core, f_kind),
            "status": status_facet,
        },
    })
}

/// Spans and marks a detail answer returns.
pub const MAX_DETAIL: usize = 5000;

/// `GET /traces/{trace_id}`; `None`: no such trace.
#[must_use]
pub fn trace(snap: &Snapshot, trace_id: &str) -> Option<Value> {
    let core = &*snap.core;
    let id = Id::from_u128(parse_uuid(trace_id)?);
    let (_, t) = core.trace_by_id(id)?;
    let rows = core
        .d
        .order
        .get(t.span_at as usize..(t.span_at + t.span_n) as usize)?;
    let spans: Vec<Value> = rows
        .iter()
        .take(MAX_DETAIL)
        .filter_map(|&r| core.span_json(r))
        .collect();
    let mark_rows = core
        .d
        .marks
        .get(t.mark_at as usize..(t.mark_at + t.mark_n) as usize)
        .unwrap_or_default();
    let marks: Vec<Value> = mark_rows
        .iter()
        .take(MAX_DETAIL)
        .filter_map(|&m| core.store.marks.get(m as usize))
        .map(|m| core.mark_json(m))
        .collect();
    Some(json!({
        "trace": core.trace_summary(t),
        "spans": spans,
        "marks": marks,
        "truncated": rows.len() > MAX_DETAIL || mark_rows.len() > MAX_DETAIL,
    }))
}

/// The span row for a content request: the trace must exist and the span must belong to it.
#[must_use]
pub fn span_in_trace(core: &Core, trace_id: &str, span_id: &str) -> Option<u32> {
    let (ti, _) = core.trace_by_id(Id::from_u128(parse_uuid(trace_id)?))?;
    let row = core.span_row(Id::from_u128(parse_uuid(span_id)?))?;
    (core.d.trace_of.get(row as usize).copied() == Some(ti)).then_some(row)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSort {
    Last,
    Traces,
    Tokens,
}

#[derive(Clone, Debug)]
pub struct SessionQuery {
    pub range: Range,
    pub client: Option<String>,
    pub kind: Option<String>,
    pub q: Option<String>,
    pub sort: SessionSort,
    pub desc: bool,
    pub limit: usize,
    pub offset: usize,
}

/// `GET /sessions`.
#[must_use]
pub fn sessions(snap: &Snapshot, q: &SessionQuery, now_us: i64) -> Value {
    let core = &*snap.core;
    let w = core.resolve(q.range, now_us);
    let want =
        |name: &Option<String>, s: Sym| name.as_deref().is_none_or(|n| core.sym_str(s) == Some(n));
    let mut rows: Vec<&SessionRow> = core
        .d
        .sessions
        .iter()
        .filter(|s| s.first_us <= w.to && s.last_us >= w.from)
        .filter(|s| want(&q.client, s.client) && want(&q.kind, s.kind))
        .filter(|s| {
            q.q.as_deref()
                .is_none_or(|n| core.key_text(s.key).is_some_and(|k| contains_ci(&k, n)))
        })
        .collect();
    match q.sort {
        // sessions are kept in order of their last activity
        SessionSort::Last => {
            if q.desc {
                rows.reverse();
            }
        }
        SessionSort::Traces => by_key(&mut rows, q.desc, |s| Some(s.trace_n)),
        SessionSort::Tokens => by_key(&mut rows, q.desc, |s| {
            (s.input >= 0 || s.output >= 0).then(|| s.input.max(0).saturating_add(s.output.max(0)))
        }),
    }
    let total = rows.len();
    let items: Vec<Value> = rows
        .iter()
        .skip(q.offset)
        .take(q.limit)
        .map(|s| core.session_summary(s))
        .collect();
    json!({"range": w.json(), "total": total, "offset": q.offset, "limit": q.limit, "items": items})
}

const MAX_SESSION_TRACES: usize = 500;
const MAX_SESSION_CALLS: usize = 2000;

/// `GET /sessions/{session_key}`; `None`: no such session.
#[must_use]
pub fn session(snap: &Snapshot, key: &str) -> Option<Value> {
    let core = &*snap.core;
    let s = core
        .d
        .sessions
        .iter()
        .find(|s| core.key_text(s.key).as_deref() == Some(key))?;
    let members = core
        .d
        .session_traces
        .get(s.trace_at as usize..(s.trace_at + s.trace_n) as usize)?;
    // the newest of a long session, in time order
    let shown = &members[members.len().saturating_sub(MAX_SESSION_TRACES)..];
    let traces: Vec<Value> = shown
        .iter()
        .filter_map(|&ti| core.d.traces.get(ti as usize))
        .map(|t| core.trace_summary(t))
        .collect();
    let mut calls: Vec<(i64, Value)> = Vec::new();
    for &ti in members {
        let Some(t) = core.d.traces.get(ti as usize) else {
            continue;
        };
        let trace_id = core.id(core.trace_id(t));
        let rows = core
            .d
            .order
            .get(t.span_at as usize..(t.span_at + t.span_n) as usize)
            .unwrap_or_default();
        for &r in rows {
            let Some(sp) = core.span(r).filter(|sp| sp.has(bit::IS_LLM)) else {
                continue;
            };
            let Some(l) = core.llm_of(sp) else { continue };
            let ts = sp.first_us();
            #[allow(clippy::cast_precision_loss)]
            calls.push((
                ts,
                json!({
                    "ts": f_json(super::ids::secs(ts)),
                    "trace_id": trace_id,
                    "span_id": core.id(sp.id),
                    "model": core.sym(l.model),
                    "input_tokens": count_json(l.input),
                    "cached_tokens": count_json(l.cached),
                    "output_tokens": count_json(l.output),
                    "reasoning_tokens": count_json(l.reasoning),
                    "duration_ms": f_json(super::ids::ms_between(sp.start_us, sp.end_us)),
                    "ttfc_ms": ms_json(l.ttfc_us),
                    "status": super::derive::span_status(sp).as_str(),
                }),
            ));
        }
    }
    calls.sort_by_key(|c| c.0);
    let skip = calls.len().saturating_sub(MAX_SESSION_CALLS);
    let calls: Vec<Value> = calls.into_iter().skip(skip).map(|c| c.1).collect();
    Some(json!({"session": core.session_summary(s), "traces": traces, "calls": calls}))
}

#[derive(Clone, Debug)]
pub struct TrajQuery {
    pub source: Option<String>,
    pub q: Option<String>,
    /// Only the trajectories linked to this trace (a UUID).
    pub trace: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// The trace a trajectory belongs to: the trace of the first span one of its steps names.
#[must_use]
pub fn trajectory_trace(core: &Core, links: &[u128]) -> Option<Id> {
    links.iter().find_map(|&u| {
        let row = core.span_row(Id::from_u128(u))?;
        let ti = *core.d.trace_of.get(row as usize)?;
        if ti == NO_ROW {
            return None;
        }
        core.d.traces.get(ti as usize).map(|t| core.trace_id(t))
    })
}

fn trace_id_text(core: &Core, links: &[u128]) -> Option<String> {
    trajectory_trace(core, links)
        .filter(|id| OddTable::is_uuid(*id))
        .and_then(|id| core.odd.text(id))
}

/// `GET /trajectories`: newest first.
#[must_use]
pub fn trajectories(snap: &Snapshot, q: &TrajQuery) -> Value {
    let core = &*snap.core;
    let of_trace = q.trace.as_deref().map(|t| parse_uuid(t).map(Id::from_u128));
    let rows: Vec<&super::atif::Traj> = core
        .trajectories
        .items
        .iter()
        .filter(|t| q.source.as_deref().is_none_or(|s| t.source == s))
        .filter(|t| match of_trace {
            None => true,
            Some(want) => want.is_some() && trajectory_trace(core, &t.links) == want,
        })
        .filter(|t| {
            q.q.as_deref().is_none_or(|n| {
                contains_ci(&fmt_uuid(t.id), n)
                    || [&t.session_id, &t.agent_name, &t.agent_model]
                        .iter()
                        .any(|x| x.as_deref().is_some_and(|x| contains_ci(x, n)))
            })
        })
        .collect();
    let total = rows.len();
    let items: Vec<Value> = rows
        .iter()
        .skip(q.offset)
        .take(q.limit)
        .map(|t| t.summary(trace_id_text(core, &t.links).as_deref()))
        .collect();
    json!({"total": total, "offset": q.offset, "limit": q.limit, "items": items})
}

/// What a trajectory id (and an optional source) names.
pub enum Found<'a> {
    One(&'a super::atif::Traj),
    None,
    /// The same id exists in several sources: the request has to say which.
    Ambiguous,
}

#[must_use]
pub fn find_trajectory<'a>(core: &'a Core, id: &str, source: Option<&str>) -> Found<'a> {
    let Some(u) = parse_uuid(id) else {
        return Found::None;
    };
    let mut hits = core
        .trajectories
        .items
        .iter()
        .filter(|t| t.id == u && source.is_none_or(|s| t.source == s));
    match (hits.next(), hits.next()) {
        (Some(t), None) => Found::One(t),
        (Some(_), Some(_)) => Found::Ambiguous,
        _ => Found::None,
    }
}

/// `TrajectorySummary` of one trajectory, with its trace link.
#[must_use]
pub fn trajectory_summary(core: &Core, t: &super::atif::Traj) -> Value {
    t.summary(trace_id_text(core, &t.links).as_deref())
}
