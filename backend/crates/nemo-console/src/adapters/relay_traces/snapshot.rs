//! What requests read: an immutable picture of the index, and the JSON shapes of addendum 9.12 for its rows.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::atif::TrajSet;
use super::derive::{Derived, SessKey, SessionRow, Status, TraceRow, mark_fields, span_status};
use super::ids::{Id, NO_TS, OddTable, Sym, SymTable, ms_between, secs};
use super::rows::{CHUNK_SLOTS, LlmRec, MarkRec, NO_ROW, SpanRec, Store, Val, bit};
use super::scan::{FileRow, GzPoints, Limits};

/// Where a content can be read, for the content route.
#[derive(Clone, Debug)]
pub struct Source {
    pub name: String,
    pub gz: bool,
    pub head_len: u32,
    pub head_hash: u128,
    /// Seek points into the compressed file (a `.gz` source only), with the identity of the file they belong to.
    pub points: Option<Arc<GzPoints>>,
}

/// The rows and everything derived from them. Shared by every snapshot until the next change.
pub struct Core {
    pub store: Store,
    pub syms: SymTable,
    pub odd: OddTable,
    pub d: Derived,
    /// Per lineage: the file that holds it now (`None`: it is gone).
    pub sources: Vec<Option<Source>>,
    pub trajectories: Arc<TrajSet>,
}

impl Default for Core {
    fn default() -> Self {
        Core {
            store: Store::default(),
            syms: SymTable::default(),
            odd: OddTable::default(),
            d: Derived {
                first_us: NO_TS,
                last_us: NO_TS,
                ..Derived::default()
            },
            sources: Vec::new(),
            trajectories: Arc::new(TrajSet::default()),
        }
    }
}

/// The part of `/traces/index` that is about the scan rather than the data.
#[derive(Clone, Debug)]
pub struct Info {
    pub state: &'static str,
    pub generation: u64,
    pub last_refresh_ts: Option<f64>,
    pub build_ms: Option<u64>,
    pub files: Vec<FileRow>,
    pub events: u64,
    pub chunk_marks: u64,
    pub truncated: bool,
    pub limits: Limits,
    pub content: &'static str,
    pub problems: Vec<(&'static str, String)>,
}

pub struct Snapshot {
    pub core: Arc<Core>,
    pub info: Info,
}

/// A float, or `null` (also for a value JSON cannot hold).
pub(super) fn f_json(v: Option<f64>) -> Value {
    v.and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// A count, or `null` for "unknown" (-1).
pub(super) fn count_json(v: i64) -> Value {
    if v < 0 { Value::Null } else { json!(v) }
}

pub(super) fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Milliseconds from microseconds, `null` for "unknown" (negative).
pub(super) fn ms_json(us: i64) -> Value {
    if us < 0 {
        Value::Null
    } else {
        #[allow(clippy::cast_precision_loss)]
        f_json(Some(us as f64 / 1e3))
    }
}

impl Core {
    #[must_use]
    pub fn sym(&self, s: Sym) -> Value {
        self.syms
            .get(s)
            .map_or(Value::Null, |t| Value::String(t.to_owned()))
    }

    #[must_use]
    pub fn sym_str(&self, s: Sym) -> Option<&str> {
        self.syms.get(s)
    }

    #[must_use]
    pub fn id(&self, id: Id) -> Value {
        self.odd.text(id).map_or(Value::Null, Value::String)
    }

    /// A comma-joined identifier list as an array.
    fn list(&self, s: Sym) -> Value {
        Value::Array(
            self.syms
                .get(s)
                .map(|t| {
                    t.split(',')
                        .filter(|x| !x.is_empty())
                        .map(|x| Value::String(x.to_owned()))
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    #[must_use]
    pub fn span(&self, row: u32) -> Option<&SpanRec> {
        self.store.spans.get(row as usize)
    }

    #[must_use]
    pub fn llm_of(&self, s: &SpanRec) -> Option<&LlmRec> {
        if s.llm == NO_ROW {
            None
        } else {
            self.store.llm.get(s.llm as usize)
        }
    }

    /// The row of the live span with this id.
    #[must_use]
    pub fn span_row(&self, id: Id) -> Option<u32> {
        let i = self
            .d
            .by_id
            .binary_search_by_key(&id, |&r| self.span(r).map_or(Id::NONE, |s| s.id))
            .ok()?;
        self.d.by_id.get(i).copied()
    }

    /// The trace whose root span has this id.
    #[must_use]
    pub fn trace_by_id(&self, id: Id) -> Option<(u32, &TraceRow)> {
        let row = self.span_row(id)?;
        let ti = *self.d.trace_of.get(row as usize)?;
        let t = self.d.traces.get(ti as usize)?;
        (t.root == row).then_some((ti, t))
    }

    #[must_use]
    pub fn trace_id(&self, t: &TraceRow) -> Id {
        self.span(t.root).map_or(Id::NONE, |s| s.id)
    }

    /// The text of a session key.
    #[must_use]
    pub fn key_text(&self, k: SessKey) -> Option<String> {
        if k.sym.is_some() {
            self.syms.get(k.sym).map(str::to_owned)
        } else {
            self.odd.text(k.trace)
        }
    }

    #[must_use]
    pub fn trace_summary(&self, t: &TraceRow) -> Value {
        let root = self.span(t.root);
        let routing = t.routing.map_or(Value::Null, |r| {
            json!({
                "algorithm": self.sym(r.algorithm),
                "route": self.sym(r.route),
                "decision": self.sym(r.decision),
                "served_model": self.sym(r.served_model),
                "overhead_ms": if r.has_overhead { f_json(Some(round3(r.overhead_ms))) } else { Value::Null },
                "errors": r.errors,
            })
        });
        let failed = t.status == Status::Error || t.error_type.is_some() || t.error_phase.is_some();
        let error = if failed {
            json!({"type": self.sym(t.error_type), "phase": self.sym(t.error_phase)})
        } else {
            Value::Null
        };
        json!({
            "trace_id": self.id(self.trace_id(t)),
            "root_name": root.map_or(Value::Null, |s| self.sym(s.name)),
            "kind": self.sym(t.kind),
            "client": self.sym(t.client),
            "session_key": self.key_text(t.key),
            "relay_session_id": self.sym(t.relay_session),
            "start_ts": f_json(secs(t.t0_us)),
            "end_ts": f_json(secs(t.end_us)),
            "duration_ms": f_json(ms_between(t.t0_us, t.end_us)),
            "status": t.status.as_str(),
            "spans": t.span_n,
            "llm_calls": t.llm_calls,
            "tool_calls": t.tool_calls,
            "tool_errors": t.tool_errors,
            "marks": t.mark_n,
            "subagents": t.subagents,
            "models": t.models.iter().map(|m| self.sym(*m)).collect::<Vec<_>>(),
            "input_tokens": count_json(t.input),
            "output_tokens": count_json(t.output),
            "cached_tokens": count_json(t.cached),
            "reasoning_tokens": count_json(t.reasoning),
            "total_tokens": count_json(t.total),
            "max_input_tokens": count_json(t.max_input),
            "ttfc_ms": ms_json(t.ttfc_us),
            "routing": routing,
            "error": error,
            "has_trajectory": t.has_trajectory,
        })
    }

    /// Output tokens per second of generation: output / (duration - time to first chunk), when all are known and
    /// the time is positive.
    #[must_use]
    pub fn tokens_per_s(s: &SpanRec, l: &LlmRec) -> Option<f64> {
        if l.output <= 0 || l.ttfc_us < 0 || s.start_us == NO_TS || s.end_us == NO_TS {
            return None;
        }
        let gen_us = s.end_us - s.start_us - l.ttfc_us;
        if gen_us <= 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some(round3(l.output as f64 / (gen_us as f64 / 1e6)))
    }

    /// The thinned chunk times of an LLM span, as milliseconds from the span's start (from the first chunk when the
    /// start is unknown), earliest first.
    fn chunk_offsets(&self, s: &SpanRec, l: &LlmRec) -> Vec<Value> {
        let Some(block) = self.store.chunk_blocks.get(l.chunk_block as usize) else {
            return Vec::new();
        };
        let n = usize::from(l.chunk_len).min(CHUNK_SLOTS);
        let mut at: Vec<i64> = block[..n]
            .iter()
            .map(|ms| l.chunk_base_us + i64::from(*ms) * 1000)
            .collect();
        at.sort_unstable();
        let zero = if s.start_us == NO_TS {
            at.first().copied().unwrap_or(0)
        } else {
            s.start_us
        };
        #[allow(clippy::cast_precision_loss)]
        at.iter()
            .map(|t| f_json(Some((t - zero) as f64 / 1e3)))
            .collect()
    }

    fn llm_json(&self, s: &SpanRec, l: &LlmRec) -> Value {
        let stream = match l.stream {
            0 => json!(false),
            1 => json!(true),
            // the request did not say, but chunks were seen
            _ if l.chunks > 0 => json!(true),
            _ => Value::Null,
        };
        json!({
            "model": self.sym(l.model),
            "served_model": self.sym(l.served_model),
            "provider": self.sym(l.provider),
            "api": self.sym(s.name),
            "stream": stream,
            "input_tokens": count_json(l.input),
            "output_tokens": count_json(l.output),
            "cached_tokens": count_json(l.cached),
            "cache_write_tokens": count_json(l.cache_write),
            "reasoning_tokens": count_json(l.reasoning),
            "total_tokens": count_json(l.total),
            "ttfc_ms": ms_json(l.ttfc_us),
            "tokens_per_s": f_json(Self::tokens_per_s(s, l)),
            "finish_reason": self.sym(l.finish_reason),
            "messages": count_json(i64::from(l.messages)),
            "tool_defs": count_json(i64::from(l.tool_defs)),
            "tool_calls_requested": count_json(i64::from(l.tool_calls)),
            "requested_tools": self.list(l.requested_tools),
            "max_tokens": count_json(l.max_tokens),
            "reasoning_effort": self.sym(l.effort),
            "chunks": if l.chunks > 0 { json!(l.chunks) } else { Value::Null },
            "chunk_offsets_ms": self.chunk_offsets(s, l),
            "max_chunk_gap_ms": l.max_chunk_gap_us().map_or(Value::Null, ms_json),
            "correlation": self.sym(l.correlation),
        })
    }

    /// True when the line of at least one of the span's events can still be read.
    #[must_use]
    pub fn content_available(&self, s: &SpanRec) -> bool {
        let readable = |lineage: u32| {
            self.sources
                .get(lineage as usize)
                .is_some_and(Option::is_some)
        };
        s.has(bit::PAYLOAD)
            && ((s.loc_start.is_some() && readable(s.loc_start.lineage))
                || (s.loc_end.is_some() && readable(s.loc_end.lineage)))
    }

    #[must_use]
    pub fn span_json(&self, row: u32) -> Option<Value> {
        let s = self.span(row)?;
        // a parent that is not in the index is no parent: the span is a root
        let parent = self
            .span_row(s.parent)
            .filter(|_| {
                !s.parent.is_none() && self.d.depth.get(row as usize).copied().unwrap_or(0) > 0
            })
            .map_or(Value::Null, |_| self.id(s.parent));
        let llm = self
            .llm_of(s)
            .filter(|_| s.has(bit::IS_LLM))
            .map_or(Value::Null, |l| self.llm_json(s, l));
        let tool = if s.has(bit::IS_TOOL) {
            json!({
                "name": self.sym(s.name),
                "call_id": self.sym(s.call_id),
                "type": if s.has(bit::MCP) { "mcp" } else { "function" },
                "mcp": s.has(bit::MCP),
            })
        } else {
            Value::Null
        };
        let error = if s.has(bit::ERROR) {
            json!({
                "type": self.sym(s.error_type),
                "status_code": if s.http_status == 0 { Value::Null } else { json!(s.http_status) },
            })
        } else {
            Value::Null
        };
        Some(json!({
            "id": self.id(s.id),
            "parent_id": parent,
            "name": self.sym(s.name),
            "category": self.sym(s.category),
            "subtype": self.sym(s.subtype),
            "role": self.sym(s.role),
            "flags": self.list(s.flags),
            "start_ts": f_json(secs(s.start_us)),
            "end_ts": f_json(secs(s.end_us)),
            "duration_ms": f_json(ms_between(s.start_us, s.end_us)),
            "status": span_status(s).as_str(),
            "depth": self.d.depth.get(row as usize).copied().unwrap_or(0),
            "turn_index": if s.turn_index < 0 { Value::Null } else { json!(s.turn_index) },
            "llm": llm,
            "tool": tool,
            "error": error,
            "content_available": self.content_available(s),
            "size_bytes": u64::from(s.loc_start.len) + u64::from(s.loc_end.len),
        }))
    }

    #[must_use]
    pub fn mark_json(&self, m: &MarkRec) -> Value {
        let mut data = Map::new();
        for (key, val) in mark_fields(&self.store, m) {
            // the routing evidence numbers are for the analytics only
            let Some(name) = key.name() else { continue };
            let v = match val {
                Val::Sym(s) => self.sym(s),
                Val::Num(n) => f_json(Some(n)),
                Val::Int(n) => json!(n),
                Val::Bool(b) => json!(b),
            };
            data.insert(name.to_owned(), v);
        }
        if m.nmeas > 0 {
            let at = m.meas as usize;
            let list: Vec<Value> = (at..at + usize::from(m.nmeas))
                .filter_map(|i| self.store.meas.get(i))
                .map(|x| {
                    let mut attrs = Map::new();
                    if let Some(text) = self.syms.get(x.attrs) {
                        for pair in text.split(',') {
                            if let Some((k, v)) = pair.split_once('=') {
                                attrs.insert(k.to_owned(), Value::String(v.to_owned()));
                            }
                        }
                    }
                    json!({
                        "name": self.sym(x.name),
                        "kind": self.sym(x.kind),
                        "value": f_json(Some(x.value).filter(|v| v.is_finite())),
                        "unit": self.sym(x.unit),
                        "attributes": attrs,
                    })
                })
                .collect();
            data.insert("measurements".to_owned(), Value::Array(list));
        }
        // ATOF gives marks no category of their own: say at least whether this one is a metric
        let category = match self.syms.get(m.category) {
            Some(c) => Value::String(c.to_owned()),
            None if m.nmeas > 0 => json!("metric"),
            None => json!("mark"),
        };
        json!({
            "id": self.id(m.id),
            "parent_id": self.id(m.parent),
            "ts": f_json(secs(m.ts_us)),
            "name": self.sym(m.name),
            "category": category,
            "schema": self.sym(m.schema),
            "severity": self.sym(m.severity),
            "data": data,
        })
    }

    #[must_use]
    pub fn session_summary(&self, s: &SessionRow) -> Value {
        json!({
            "session_key": self.key_text(s.key),
            "client": self.sym(s.client),
            "kind": self.sym(s.kind),
            "first_ts": f_json(secs(s.first_us)),
            "last_ts": f_json(secs(s.last_us)),
            "span_ms": f_json(ms_between(s.first_us, s.last_us)),
            "traces": s.trace_n,
            "llm_calls": s.llm_calls,
            "tool_calls": s.tool_calls,
            "errors": s.errors,
            "models": s.models.iter().map(|m| self.sym(*m)).collect::<Vec<_>>(),
            "input_tokens": count_json(s.input),
            "output_tokens": count_json(s.output),
            "cached_tokens": count_json(s.cached),
            "max_input_tokens": count_json(s.max_input),
        })
    }
}

impl Snapshot {
    /// An index with nothing in it, in the given state.
    #[must_use]
    pub fn empty(state: &'static str, limits: Limits, content: &'static str) -> Snapshot {
        Snapshot {
            core: Arc::new(Core::default()),
            info: Info {
                state,
                generation: 0,
                last_refresh_ts: None,
                build_ms: None,
                files: Vec::new(),
                events: 0,
                chunk_marks: 0,
                truncated: false,
                limits,
                content,
                problems: Vec::new(),
            },
        }
    }

    /// `IndexState`.
    #[must_use]
    pub fn index_state(&self) -> Value {
        let c = &self.core;
        let i = &self.info;
        let files: Vec<Value> = i
            .files
            .iter()
            .map(|f| {
                json!({
                    "name": f.name,
                    "bytes": f.bytes,
                    "scanned_bytes": f.scanned_bytes,
                    "compressed": f.compressed,
                    "events": f.events,
                    "skipped_lines": f.skipped_lines,
                    "state": f.state.as_str(),
                    "first_ts": f_json(secs(f.first_us)),
                    "last_ts": f_json(secs(f.last_us)),
                })
            })
            .collect();
        let marks = c.d.marks.len();
        json!({
            "state": i.state,
            "generation": i.generation,
            "last_refresh_ts": f_json(i.last_refresh_ts),
            "build_ms": i.build_ms,
            "files": files,
            "events": i.events,
            "spans": c.d.spans,
            "open_spans": c.d.open_spans,
            "marks": marks,
            "chunk_marks": i.chunk_marks,
            "traces": c.d.traces.len(),
            "sessions": c.d.sessions.len(),
            "trajectories": c.trajectories.items.len(),
            "coverage": {"from_ts": f_json(secs(c.d.first_us)), "to_ts": f_json(secs(c.d.last_us))},
            "truncated": i.truncated,
            "limits": {
                "scan_bytes": i.limits.scan_bytes,
                "max_spans": i.limits.max_spans,
                "max_line_bytes": i.limits.max_line_bytes,
            },
            "content": i.content,
            "problems": i.problems.iter().map(|(code, message)| json!({"code": code, "message": message})).collect::<Vec<_>>(),
        })
    }
}
