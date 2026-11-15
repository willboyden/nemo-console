//! From rows to traces: which span belongs to which trace, per-trace and per-session summaries, and which traces go
//! when the index is over its span limit.
//!
//! A trace is a root span and its descendants; the root is a span whose parent is null or not in the index. The
//! grouping is recomputed from the rows for every published snapshot, so events may arrive in any order (an end
//! before its start, a child before its parent, a newer file before an older one).

use std::collections::HashMap;

use super::chunkvec::ChunkVec;
use super::ids::{Id, NO_TS, Sym};
use super::rows::{Ingest, Key, LlmRec, MarkRec, NO_ROW, SpanRec, Store, Val, bit};

/// Grouping of the span rows into traces.
pub struct Grouping {
    /// Per span row: the row of its root ([`NO_ROW`] for a placeholder row).
    pub root: Vec<u32>,
    pub depth: Vec<u16>,
}

/// Find every span's root. A parent chain that loops (a corrupt capture) is cut at the span that closes the loop.
pub fn group(spans: &ChunkVec<SpanRec>, by_id: &HashMap<Id, u32>) -> Grouping {
    let n = spans.len();
    let mut root = vec![NO_ROW; n];
    let mut depth = vec![0u16; n];
    // 0 = not visited, 1 = on the path being followed, 2 = done
    let mut state = vec![0u8; n];
    let parent_row = |i: usize| -> u32 {
        let Some(s) = spans.get(i) else { return NO_ROW };
        if s.parent.is_none() {
            return NO_ROW;
        }
        match by_id.get(&s.parent) {
            Some(&p) if p as usize != i && spans.get(p as usize).is_some_and(SpanRec::live) => p,
            _ => NO_ROW,
        }
    };
    let mut path: Vec<u32> = Vec::new();
    for i in 0..n {
        if state[i] != 0 || !spans.get(i).is_some_and(SpanRec::live) {
            continue;
        }
        path.clear();
        let mut cur = i;
        // the root of the chain and the depth of the node just below what was already known
        let (found_root, mut next_depth) = loop {
            if state[cur] == 2 {
                break (root[cur], u32::from(depth[cur]) + 1);
            }
            if state[cur] == 1 {
                // `cur` is its own ancestor: the last span followed becomes a root
                let Some(top) = path.pop() else {
                    break (NO_ROW, 0);
                };
                let t = top as usize;
                root[t] = top;
                depth[t] = 0;
                state[t] = 2;
                break (top, 1);
            }
            state[cur] = 1;
            let Ok(c) = u32::try_from(cur) else {
                break (NO_ROW, 0);
            };
            let p = parent_row(cur);
            if p == NO_ROW {
                root[cur] = c;
                depth[cur] = 0;
                state[cur] = 2;
                break (c, 1);
            }
            path.push(c);
            cur = p as usize;
        };
        for &node in path.iter().rev() {
            let k = node as usize;
            root[k] = found_root;
            depth[k] = u16::try_from(next_depth).unwrap_or(u16::MAX);
            state[k] = 2;
            next_depth += 1;
        }
    }
    Grouping { root, depth }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Error,
    Open,
}

impl Status {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Error => "error",
            Status::Open => "open",
        }
    }
}

/// A span's status: failed, still open (no end), or ended.
#[must_use]
pub fn span_status(s: &SpanRec) -> Status {
    if s.has(bit::ERROR) {
        Status::Error
    } else if s.has(bit::HAS_END) {
        Status::Ok
    } else {
        Status::Open
    }
}

/// What a session is named by: a client or Relay session id (a symbol), or the id of a trace that has neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessKey {
    pub sym: Sym,
    pub trace: Id,
}

/// The routing marks of a trace, summed up.
#[derive(Clone, Copy, Debug)]
pub struct RoutingRow {
    pub algorithm: Sym,
    pub route: Sym,
    pub decision: Sym,
    pub served_model: Sym,
    pub overhead_ms: f64,
    pub has_overhead: bool,
    pub errors: u32,
}

#[derive(Clone, Debug)]
pub struct TraceRow {
    /// Span row of the root.
    pub root: u32,
    /// Its spans: `span_n` entries of `Derived::order` from `span_at`, earliest first.
    pub span_at: u32,
    pub span_n: u32,
    /// Its marks: `mark_n` entries of `Derived::marks` from `mark_at`, earliest first.
    pub mark_at: u32,
    pub mark_n: u32,
    /// The time the trace is ordered and windowed by: the root's start, or the earliest thing known about it.
    pub t0_us: i64,
    pub end_us: i64,
    /// The latest thing known about it.
    pub last_us: i64,
    pub status: Status,
    pub kind: Sym,
    pub client: Sym,
    pub relay_session: Sym,
    pub session: u32,
    pub key: SessKey,
    pub llm_calls: u32,
    pub llm_errors: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub subagents: u32,
    pub models: Vec<Sym>,
    pub input: i64,
    pub output: i64,
    pub cached: i64,
    pub reasoning: i64,
    pub total: i64,
    pub max_input: i64,
    pub ttfc_us: i64,
    pub routing: Option<RoutingRow>,
    pub error_type: Sym,
    pub error_phase: Sym,
    pub has_trajectory: bool,
}

#[derive(Clone, Debug)]
pub struct SessionRow {
    pub key: SessKey,
    pub client: Sym,
    pub kind: Sym,
    pub first_us: i64,
    pub last_us: i64,
    /// Its traces: `trace_n` entries of `Derived::session_traces` from `trace_at`, by start.
    pub trace_at: u32,
    pub trace_n: u32,
    pub llm_calls: u64,
    pub tool_calls: u64,
    /// Traces that failed.
    pub errors: u32,
    pub models: Vec<Sym>,
    pub input: i64,
    pub output: i64,
    pub cached: i64,
    pub max_input: i64,
}

/// Everything a snapshot derives from the rows.
#[derive(Default)]
pub struct Derived {
    /// Live span rows grouped by trace, earliest first inside a trace.
    pub order: Vec<u32>,
    /// Per span row: its trace ([`NO_ROW`] for a placeholder row).
    pub trace_of: Vec<u32>,
    pub depth: Vec<u16>,
    /// Traces by `t0_us`, oldest first.
    pub traces: Vec<TraceRow>,
    /// Mark rows grouped by trace, earliest first; marks of no known span come last.
    pub marks: Vec<u32>,
    /// How many entries at the end of `marks` belong to no trace.
    pub orphan_marks: u32,
    pub sessions: Vec<SessionRow>,
    pub session_traces: Vec<u32>,
    /// Live span rows sorted by id (the snapshot's lookup).
    pub by_id: Vec<u32>,
    pub spans: u64,
    pub open_spans: u64,
    pub first_us: i64,
    pub last_us: i64,
}

/// Labels the derivation needs as symbols; interned by the scan so they exist in the string table.
#[derive(Clone, Copy)]
pub struct Labels {
    pub unknown: Sym,
    pub fallback: Sym,
    pub passthrough: Sym,
    pub above: Sym,
    pub below: Sym,
    pub routed: Sym,
    pub requested: Sym,
    pub decision: Sym,
    pub overhead: Sym,
    pub error: Sym,
    pub llm_call: Sym,
    pub subagent: Sym,
    pub ok: Sym,
    pub routing: Sym,
}

impl Labels {
    pub fn intern(ing: &mut Ingest) -> Labels {
        let mut s = |t: &str| ing.syms.intern(Some(t));
        Labels {
            unknown: s("unknown"),
            fallback: s("fallback"),
            passthrough: s("passthrough"),
            above: s("above_threshold"),
            below: s("below_threshold"),
            routed: s("routed"),
            requested: s("switchyard.routing.requested"),
            decision: s("switchyard.routing.decision"),
            overhead: s("switchyard.routing.overhead"),
            error: s("switchyard.routing.error"),
            llm_call: s("switchyard.routing.llm_call"),
            subagent: s("subagent"),
            ok: s("ok"),
            routing: s("routing"),
        }
    }
}

/// The fields of one mark.
pub fn mark_fields<'a>(store: &'a Store, m: &MarkRec) -> impl Iterator<Item = (Key, Val)> + 'a {
    let at = m.fields as usize;
    (at..at + usize::from(m.nfields))
        .filter_map(|i| store.mark_fields.get(i).map(|f| (f.key, f.val)))
}

#[must_use]
pub fn mark_sym(store: &Store, m: &MarkRec, key: Key) -> Sym {
    mark_fields(store, m)
        .find_map(|(k, v)| match v {
            Val::Sym(s) if k == key => Some(s),
            _ => None,
        })
        .unwrap_or(Sym::NONE)
}

#[must_use]
pub fn mark_num(store: &Store, m: &MarkRec, key: Key) -> Option<f64> {
    mark_fields(store, m).find_map(|(k, v)| match v {
        Val::Num(n) if k == key => Some(n),
        #[allow(clippy::cast_precision_loss)]
        Val::Int(n) if k == key => Some(n as f64),
        _ => None,
    })
}

#[must_use]
pub fn mark_bool(store: &Store, m: &MarkRec, key: Key) -> Option<bool> {
    mark_fields(store, m).find_map(|(k, v)| match v {
        Val::Bool(b) if k == key => Some(b),
        _ => None,
    })
}

/// What a routing decision mark decided, as a label. The mark's own `decision` or `tier` when the capture has one;
/// otherwise what its other fields say: a fallback, a passthrough route, or on which side of the classifier's
/// threshold the request fell.
#[must_use]
pub fn decision_label(store: &Store, m: &MarkRec, lb: &Labels) -> Sym {
    let own = mark_sym(store, m, Key::Decision);
    if own.is_some() {
        return own;
    }
    let tier = mark_sym(store, m, Key::Tier);
    if tier.is_some() {
        return tier;
    }
    if mark_bool(store, m, Key::FallbackUsed) == Some(true) {
        return lb.fallback;
    }
    if mark_sym(store, m, Key::Algorithm) == lb.passthrough {
        return lb.passthrough;
    }
    match (
        mark_num(store, m, Key::Score),
        mark_num(store, m, Key::Threshold),
    ) {
        (Some(s), Some(t)) if s >= t => lb.above,
        (Some(_), Some(_)) => lb.below,
        _ => lb.routed,
    }
}

/// A session id a route can carry: one path segment.
fn usable_key(s: &str) -> bool {
    !s.contains('/') && s != "." && s != ".."
}

fn add(sum: &mut i64, v: i64) {
    if v >= 0 {
        *sum = if *sum < 0 { v } else { sum.saturating_add(v) };
    }
}

fn push_model(models: &mut Vec<Sym>, m: Sym, cap: usize) {
    if m.is_some() && models.len() < cap && !models.contains(&m) {
        models.push(m);
    }
}

/// Derive traces, sessions and orderings from the rows.
#[allow(clippy::too_many_lines)]
pub fn derive(ing: &Ingest, lb: &Labels) -> Derived {
    let store = &ing.store;
    let spans = &store.spans;
    let n = spans.len();
    let g = group(spans, &ing.span_by_id);
    // rows in trace order: by root row, then by time
    let mut order: Vec<u32> = (0..n)
        .filter(|&i| g.root[i] != NO_ROW)
        .filter_map(|i| u32::try_from(i).ok())
        .collect();
    let key = |i: u32| {
        let s = spans.get(i as usize);
        (g.root[i as usize], s.map_or(NO_TS, SpanRec::first_us), i)
    };
    order.sort_unstable_by_key(|&i| key(i));
    let mut by_id = order.clone();
    by_id.sort_unstable_by_key(|&i| spans.get(i as usize).map_or(Id::NONE, |s| s.id));

    let mut traces: Vec<TraceRow> = Vec::new();
    let mut slot_of_root: HashMap<u32, u32> = HashMap::new();
    let mut d = Derived {
        first_us: NO_TS,
        last_us: NO_TS,
        ..Derived::default()
    };
    let table = ing.syms.frozen();
    let mut at = 0usize;
    while at < order.len() {
        let r = g.root[order[at] as usize];
        let mut end = at;
        while end < order.len() && g.root[order[end] as usize] == r {
            end += 1;
        }
        let Some(root) = spans.get(r as usize) else {
            at = end;
            continue;
        };
        let mut t = TraceRow {
            root: r,
            span_at: u32::try_from(at).unwrap_or(0),
            span_n: u32::try_from(end - at).unwrap_or(0),
            mark_at: 0,
            mark_n: 0,
            t0_us: root.start_us,
            end_us: root.end_us,
            last_us: NO_TS,
            status: Status::Ok,
            kind: root.agent_kind,
            client: Sym::NONE,
            relay_session: root.relay_session,
            session: NO_ROW,
            key: SessKey {
                sym: Sym::NONE,
                trace: root.id,
            },
            llm_calls: 0,
            llm_errors: 0,
            tool_calls: 0,
            tool_errors: 0,
            subagents: 0,
            models: Vec::new(),
            input: -1,
            output: -1,
            cached: -1,
            reasoning: -1,
            total: -1,
            max_input: -1,
            ttfc_us: -1,
            routing: None,
            error_type: Sym::NONE,
            error_phase: Sym::NONE,
            has_trajectory: false,
        };
        let mut first_us = NO_TS;
        let mut client_session = Sym::NONE;
        let mut any_error = false;
        for &i in &order[at..end] {
            let Some(s) = spans.get(i as usize) else {
                continue;
            };
            d.spans += 1;
            if !s.has(bit::HAS_END) {
                d.open_spans += 1;
            }
            let (a, b) = (s.first_us(), s.last_us());
            if a != NO_TS && (first_us == NO_TS || a < first_us) {
                first_us = a;
            }
            if b != NO_TS && (t.last_us == NO_TS || b > t.last_us) {
                t.last_us = b;
            }
            if s.has(bit::ERROR) {
                if !any_error {
                    t.error_type = s.error_type;
                }
                any_error = true;
            }
            if t.kind.is_none() {
                t.kind = s.agent_kind;
            }
            if t.relay_session.is_none() {
                t.relay_session = s.relay_session;
            }
            if i != r
                && (s.role == lb.subagent || (s.category == ing.known.agent && s.role.is_none()))
            {
                t.subagents += 1;
            }
            if s.has(bit::IS_TOOL) {
                t.tool_calls += 1;
                if s.has(bit::ERROR) {
                    t.tool_errors += 1;
                }
            }
            if s.has(bit::IS_LLM) {
                t.llm_calls += 1;
                if s.has(bit::ERROR) {
                    t.llm_errors += 1;
                }
                if let Some(l) = store.llm.get(s.llm as usize) {
                    push_model(&mut t.models, l.model, 8);
                    add(&mut t.input, l.input);
                    add(&mut t.output, l.output);
                    add(&mut t.cached, l.cached);
                    add(&mut t.reasoning, l.reasoning);
                    add(&mut t.total, l.total);
                    t.max_input = t.max_input.max(l.input);
                    // rows come earliest first: the first call that has a value decides
                    if t.ttfc_us < 0 {
                        t.ttfc_us = l.ttfc_us;
                    }
                    if t.client.is_none() {
                        t.client = l.client;
                    }
                    if client_session.is_none()
                        && l.client_session.is_some()
                        && table.get(l.client_session).is_some_and(usable_key)
                    {
                        client_session = l.client_session;
                    }
                }
            }
        }
        if t.t0_us == NO_TS {
            t.t0_us = first_us;
        }
        if t.last_us == NO_TS {
            t.last_us = t.t0_us;
        }
        t.status = if any_error {
            Status::Error
        } else if root.has(bit::HAS_END) {
            Status::Ok
        } else {
            Status::Open
        };
        if t.kind.is_none() {
            t.kind = lb.unknown;
        }
        t.key.sym = client_session;
        slot_of_root.insert(r, u32::try_from(traces.len()).unwrap_or(NO_ROW));
        traces.push(t);
        at = end;
    }
    // the session key falls back to Relay's own session id, then to the trace id
    for t in &mut traces {
        if t.key.sym.is_none()
            && t.relay_session.is_some()
            && table.get(t.relay_session).is_some_and(usable_key)
        {
            t.key.sym = t.relay_session;
        }
        if t.key.sym.is_some() {
            t.key.trace = Id::NONE;
        }
    }

    // marks: by trace, then time; the same mark read twice (a duplicated line) is kept once
    let trace_of_parent = |m: &MarkRec| -> u32 {
        if m.parent.is_none() {
            return NO_ROW;
        }
        ing.span_by_id
            .get(&m.parent)
            .map(|&p| g.root.get(p as usize).copied().unwrap_or(NO_ROW))
            .and_then(|root| slot_of_root.get(&root).copied())
            .unwrap_or(NO_ROW)
    };
    let mut marks: Vec<(u32, i64, Id, u32)> = store
        .marks
        .iter()
        .enumerate()
        .filter_map(|(i, m)| Some((trace_of_parent(m), m.ts_us, m.id, u32::try_from(i).ok()?)))
        .collect();
    marks.sort_unstable();
    marks.dedup_by(|a, b| a.2 == b.2 && a.0 == b.0);
    let mut k = 0usize;
    while k < marks.len() {
        let slot = marks[k].0;
        let mut end = k;
        while end < marks.len() && marks[end].0 == slot {
            end += 1;
        }
        if slot == NO_ROW {
            d.orphan_marks = u32::try_from(end - k).unwrap_or(u32::MAX);
        } else if let Some(t) = traces.get_mut(slot as usize) {
            t.mark_at = u32::try_from(k).unwrap_or(0);
            t.mark_n = u32::try_from(end - k).unwrap_or(0);
            let mut routing: Option<RoutingRow> = None;
            let new_row = || RoutingRow {
                algorithm: Sym::NONE,
                route: Sym::NONE,
                decision: Sym::NONE,
                served_model: Sym::NONE,
                overhead_ms: 0.0,
                has_overhead: false,
                errors: 0,
            };
            for &(_, ts, _, mi) in &marks[k..end] {
                if ts > t.last_us {
                    t.last_us = ts;
                }
                let Some(m) = store.marks.get(mi as usize) else {
                    continue;
                };
                if m.name == lb.requested {
                    let r = routing.get_or_insert_with(new_row);
                    let a = mark_sym(store, m, Key::Algorithm);
                    if a.is_some() {
                        r.algorithm = a;
                    }
                } else if m.name == lb.decision {
                    // the last decision of the trace describes it
                    let r = routing.get_or_insert_with(new_row);
                    let a = mark_sym(store, m, Key::Algorithm);
                    if a.is_some() {
                        r.algorithm = a;
                    }
                    r.route = mark_sym(store, m, Key::Route);
                    r.decision = decision_label(store, m, lb);
                    let served = mark_sym(store, m, Key::ServedModel);
                    r.served_model = if served.is_some() {
                        served
                    } else {
                        mark_sym(store, m, Key::SelectedModel)
                    };
                } else if m.name == lb.overhead && m.nmeas == 0 {
                    // the plain mark, not its metric twin (the same number twice)
                    if let Some(ms) = mark_num(store, m, Key::LatencyMs) {
                        let r = routing.get_or_insert_with(new_row);
                        r.overhead_ms += ms;
                        r.has_overhead = true;
                    }
                } else if m.name == lb.error {
                    let r = routing.get_or_insert_with(new_row);
                    r.errors += 1;
                    if t.error_phase.is_none() {
                        t.error_phase = mark_sym(store, m, Key::Phase);
                    }
                    if t.error_type.is_none() {
                        t.error_type = mark_sym(store, m, Key::Category);
                    }
                }
            }
            if let Some(mut r) = routing {
                if r.route.is_none() {
                    // with Switchyard routing, the model a client asks for is the route id
                    r.route = t.models.first().copied().unwrap_or(Sym::NONE);
                }
                t.routing = Some(r);
            }
        }
        k = end;
    }
    d.marks = marks.iter().map(|m| m.3).collect();

    // traces by time; `trace_of` and the mark ranges follow the new order
    let mut perm: Vec<u32> = (0..traces.len())
        .filter_map(|i| u32::try_from(i).ok())
        .collect();
    perm.sort_by_key(|&i| {
        let t = &traces[i as usize];
        (
            t.t0_us,
            spans.get(t.root as usize).map_or(Id::NONE, |s| s.id),
        )
    });
    let mut sorted: Vec<TraceRow> = Vec::with_capacity(traces.len());
    let mut trace_of = vec![NO_ROW; n];
    for &old in &perm {
        let t = traces[old as usize].clone();
        let new_ix = u32::try_from(sorted.len()).unwrap_or(NO_ROW);
        for &i in &order[t.span_at as usize..(t.span_at + t.span_n) as usize] {
            trace_of[i as usize] = new_ix;
        }
        if t.t0_us != NO_TS && (d.first_us == NO_TS || t.t0_us < d.first_us) {
            d.first_us = t.t0_us;
        }
        if t.last_us != NO_TS && (d.last_us == NO_TS || t.last_us > d.last_us) {
            d.last_us = t.last_us;
        }
        sorted.push(t);
    }
    let mut traces = sorted;

    // sessions
    let mut slot: HashMap<SessKey, u32> = HashMap::new();
    let mut sessions: Vec<SessionRow> = Vec::new();
    let mut members: Vec<Vec<u32>> = Vec::new();
    for (ti, t) in traces.iter_mut().enumerate() {
        let si = *slot.entry(t.key).or_insert_with(|| {
            sessions.push(SessionRow {
                key: t.key,
                client: Sym::NONE,
                kind: Sym::NONE,
                first_us: NO_TS,
                last_us: NO_TS,
                trace_at: 0,
                trace_n: 0,
                llm_calls: 0,
                tool_calls: 0,
                errors: 0,
                models: Vec::new(),
                input: -1,
                output: -1,
                cached: -1,
                max_input: -1,
            });
            members.push(Vec::new());
            u32::try_from(sessions.len() - 1).unwrap_or(NO_ROW)
        });
        t.session = si;
        let (Some(s), Some(mem)) = (sessions.get_mut(si as usize), members.get_mut(si as usize))
        else {
            continue;
        };
        mem.push(u32::try_from(ti).unwrap_or(NO_ROW));
        if s.client.is_none() {
            s.client = t.client;
        }
        if s.kind.is_none() || s.kind == lb.unknown {
            s.kind = t.kind;
        }
        if t.t0_us != NO_TS && (s.first_us == NO_TS || t.t0_us < s.first_us) {
            s.first_us = t.t0_us;
        }
        if t.last_us != NO_TS && (s.last_us == NO_TS || t.last_us > s.last_us) {
            s.last_us = t.last_us;
        }
        s.llm_calls += u64::from(t.llm_calls);
        s.tool_calls += u64::from(t.tool_calls);
        if t.status == Status::Error {
            s.errors += 1;
        }
        for &m in &t.models {
            push_model(&mut s.models, m, 16);
        }
        add(&mut s.input, t.input);
        add(&mut s.output, t.output);
        add(&mut s.cached, t.cached);
        s.max_input = s.max_input.max(t.max_input);
    }
    for (s, mem) in sessions.iter_mut().zip(&members) {
        s.trace_at = u32::try_from(d.session_traces.len()).unwrap_or(0);
        s.trace_n = u32::try_from(mem.len()).unwrap_or(0);
        d.session_traces.extend_from_slice(mem);
    }
    // sessions by last activity, oldest first; the traces point at the new places
    let mut sperm: Vec<u32> = (0..sessions.len())
        .filter_map(|i| u32::try_from(i).ok())
        .collect();
    sperm.sort_by_key(|&i| {
        (
            sessions[i as usize].last_us,
            sessions[i as usize].first_us,
            i,
        )
    });
    let mut new_place = vec![NO_ROW; sessions.len()];
    for (new_ix, &old) in sperm.iter().enumerate() {
        new_place[old as usize] = u32::try_from(new_ix).unwrap_or(NO_ROW);
    }
    for t in &mut traces {
        if let Some(&p) = new_place.get(t.session as usize) {
            t.session = p;
        }
    }
    d.sessions = sperm
        .iter()
        .map(|&i| sessions[i as usize].clone())
        .collect();
    d.order = order;
    d.trace_of = trace_of;
    d.depth = g.depth;
    d.traces = traces;
    d.by_id = by_id;
    d
}

/// Keep at most `target` marks: the oldest go first, marks that belong to no indexed span before those that do (a
/// capture can hold any number of marks; the spans' own limit does not bound them). Returns how many were dropped.
pub fn evict_marks(ing: &mut Ingest, target: usize) -> usize {
    let n = ing.store.marks.len();
    if n <= target {
        return 0;
    }
    let mut order: Vec<(bool, i64, usize)> = ing
        .store
        .marks
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let parented = !m.parent.is_none() && ing.span_by_id.contains_key(&m.parent);
            (parented, m.ts_us, i)
        })
        .collect();
    order.sort_unstable();
    let drop = n - target;
    let mut gone = vec![false; n];
    for &(_, _, i) in order.iter().take(drop) {
        if let Some(g) = gone.get_mut(i) {
            *g = true;
        }
    }
    let old_marks = std::mem::take(&mut ing.store.marks);
    let old_fields = std::mem::take(&mut ing.store.mark_fields);
    let old_meas = std::mem::take(&mut ing.store.meas);
    for (i, m) in old_marks.iter().enumerate() {
        if gone.get(i).copied().unwrap_or(true) {
            continue;
        }
        let mut m = *m;
        let (fa, ma) = (m.fields as usize, m.meas as usize);
        m.fields = u32::try_from(ing.store.mark_fields.len()).unwrap_or(NO_ROW);
        for k in fa..fa + usize::from(m.nfields) {
            if let Some(f) = old_fields.get(k) {
                ing.store.mark_fields.push(*f);
            }
        }
        m.meas = u32::try_from(ing.store.meas.len()).unwrap_or(NO_ROW);
        for k in ma..ma + usize::from(m.nmeas) {
            if let Some(x) = old_meas.get(k) {
                ing.store.meas.push(*x);
            }
        }
        ing.store.marks.push(m);
    }
    drop
}

/// Drop the oldest traces until at most `target` live spans are left. Returns how many spans went.
pub fn evict(ing: &mut Ingest, target: usize) -> usize {
    let g = group(&ing.store.spans, &ing.span_by_id);
    let n = ing.store.spans.len();
    // per root: number of spans and the latest moment known
    let mut per_root: HashMap<u32, (usize, i64)> = HashMap::new();
    let mut live = 0usize;
    for i in 0..n {
        let r = g.root[i];
        if r == NO_ROW {
            continue;
        }
        live += 1;
        let last = ing.store.spans.get(i).map_or(NO_TS, SpanRec::last_us);
        let e = per_root.entry(r).or_insert((0, NO_TS));
        e.0 += 1;
        e.1 = e.1.max(last);
    }
    if live <= target {
        return 0;
    }
    let mut roots: Vec<(i64, u32, usize)> = per_root
        .iter()
        .map(|(&r, &(c, last))| (last, r, c))
        .collect();
    roots.sort_unstable();
    let mut drop_root: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut left = live;
    let mut cut_us = NO_TS;
    for &(last, r, c) in &roots {
        if left <= target {
            break;
        }
        drop_root.insert(r);
        left -= c;
        cut_us = cut_us.max(last);
    }
    let dropped = live - left;

    // rebuild the tables with what stays
    let old = std::mem::take(&mut ing.store);
    let mut store = Store::default();
    let mut by_id: HashMap<Id, u32> = HashMap::new();
    let copy_llm = |l: &LlmRec, store: &mut Store| -> LlmRec {
        let mut l = *l;
        if l.chunk_block != NO_ROW {
            match old.chunk_blocks.get(l.chunk_block as usize) {
                Some(b) => {
                    l.chunk_block = u32::try_from(store.chunk_blocks.len()).unwrap_or(NO_ROW);
                    store.chunk_blocks.push(*b);
                }
                None => l.chunk_block = NO_ROW,
            }
        }
        l
    };
    for (i, s) in old.spans.iter().enumerate() {
        let keep = match g.root[i] {
            // a placeholder (only chunk marks point at it) stays while its chunks are newer than what was cut
            NO_ROW => old
                .llm
                .get(s.llm as usize)
                .is_some_and(|l| l.chunk_prev_us > cut_us),
            r => !drop_root.contains(&r),
        };
        if !keep {
            continue;
        }
        let mut s = *s;
        if s.llm != NO_ROW {
            match old.llm.get(s.llm as usize) {
                Some(l) => {
                    let l = copy_llm(l, &mut store);
                    s.llm = u32::try_from(store.llm.len()).unwrap_or(NO_ROW);
                    store.llm.push(l);
                }
                None => s.llm = NO_ROW,
            }
        }
        let Ok(ix) = u32::try_from(store.spans.len()) else {
            break;
        };
        by_id.insert(s.id, ix);
        store.spans.push(s);
    }
    for m in old.marks.iter() {
        let keep = match ing.span_by_id.get(&m.parent) {
            Some(&p) if g.root.get(p as usize).is_some_and(|r| *r != NO_ROW) => {
                !drop_root.contains(&g.root[p as usize])
            }
            _ => m.ts_us > cut_us,
        };
        if !keep {
            continue;
        }
        let mut m = *m;
        let (fa, ma) = (m.fields as usize, m.meas as usize);
        m.fields = u32::try_from(store.mark_fields.len()).unwrap_or(NO_ROW);
        for i in fa..fa + usize::from(m.nfields) {
            if let Some(f) = old.mark_fields.get(i) {
                store.mark_fields.push(*f);
            }
        }
        m.meas = u32::try_from(store.meas.len()).unwrap_or(NO_ROW);
        for i in ma..ma + usize::from(m.nmeas) {
            if let Some(x) = old.meas.get(i) {
                store.meas.push(*x);
            }
        }
        store.marks.push(m);
    }
    ing.store = store;
    ing.span_by_id = by_id;
    dropped
}
