//! The rows of the trace index and how one event changes them.
//!
//! Everything here is fixed-size, identifier-only data: symbols (interned identifier strings), numbers, flags and
//! the position of the source line. No text from a prompt, a message, a tool call or a header can be stored, because
//! no row has a field that could hold it.

use std::collections::HashMap;

use super::chunkvec::ChunkVec;
use super::event::RawEvent;
use super::ids::{Id, NO_TS, OddIds, Sym, Syms};

/// Where a line is: which content (a "lineage", see `scan.rs`), the byte offset inside it and its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    pub offset: u64,
    pub len: u32,
    pub lineage: u32,
}

impl Loc {
    pub const NONE: Loc = Loc {
        offset: 0,
        len: 0,
        lineage: u32::MAX,
    };

    #[must_use]
    pub fn is_some(self) -> bool {
        self.lineage != u32::MAX
    }
}

/// `SpanRec::bits`.
pub mod bit {
    pub const HAS_START: u16 = 1;
    pub const HAS_END: u16 = 1 << 1;
    /// The capture says the span failed (OTel status ERROR, an error type, a failed tool call).
    pub const ERROR: u16 = 1 << 2;
    /// One of its events carried a payload (`data` was not null).
    pub const PAYLOAD: u16 = 1 << 3;
    /// A tool whose name has the MCP shape `mcp__server__tool`.
    pub const MCP: u16 = 1 << 4;
    pub const IS_LLM: u16 = 1 << 5;
    pub const IS_TOOL: u16 = 1 << 6;
}

pub const NO_ROW: u32 = u32::MAX;

/// One scope (a start event, an end event, or both).
#[derive(Clone, Copy, Debug)]
pub struct SpanRec {
    pub id: Id,
    pub parent: Id,
    pub start_us: i64,
    pub end_us: i64,
    pub loc_start: Loc,
    pub loc_end: Loc,
    pub name: Sym,
    pub category: Sym,
    pub subtype: Sym,
    pub role: Sym,
    /// The identifier-shaped `attributes`, joined with commas.
    pub flags: Sym,
    pub agent_kind: Sym,
    pub relay_session: Sym,
    pub error_type: Sym,
    /// A tool scope's call id.
    pub call_id: Sym,
    pub turn_index: i32,
    /// Row in the LLM table, or [`NO_ROW`].
    pub llm: u32,
    pub http_status: u16,
    pub bits: u16,
}

impl SpanRec {
    fn blank(id: Id) -> SpanRec {
        SpanRec {
            id,
            parent: Id::NONE,
            start_us: NO_TS,
            end_us: NO_TS,
            loc_start: Loc::NONE,
            loc_end: Loc::NONE,
            name: Sym::NONE,
            category: Sym::NONE,
            subtype: Sym::NONE,
            role: Sym::NONE,
            flags: Sym::NONE,
            agent_kind: Sym::NONE,
            relay_session: Sym::NONE,
            error_type: Sym::NONE,
            call_id: Sym::NONE,
            turn_index: -1,
            llm: NO_ROW,
            http_status: 0,
            bits: 0,
        }
    }

    /// A span exists once a scope event named it. A row that only llm.chunk marks pointed at is a placeholder.
    #[must_use]
    pub fn live(&self) -> bool {
        self.bits & (bit::HAS_START | bit::HAS_END) != 0
    }

    #[must_use]
    pub fn has(&self, b: u16) -> bool {
        self.bits & b != 0
    }

    /// The earliest moment known for this span.
    #[must_use]
    pub fn first_us(&self) -> i64 {
        if self.start_us == NO_TS {
            self.end_us
        } else {
            self.start_us
        }
    }

    /// The latest moment known for this span.
    #[must_use]
    pub fn last_us(&self) -> i64 {
        if self.end_us == NO_TS {
            self.start_us
        } else {
            self.end_us
        }
    }
}

/// Offsets kept per streamed answer (the contract's "at most 64 evenly thinned offsets").
pub const CHUNK_SLOTS: usize = 64;
pub type ChunkBlock = [i32; CHUNK_SLOTS];

/// The LLM side of a span. `-1` is "unknown" in every count.
#[derive(Clone, Copy, Debug)]
pub struct LlmRec {
    pub model: Sym,
    pub served_model: Sym,
    pub provider: Sym,
    pub finish_reason: Sym,
    pub effort: Sym,
    pub correlation: Sym,
    pub client: Sym,
    pub client_session: Sym,
    /// Names of the tools the answer asked for (at most 16), joined with commas.
    pub requested_tools: Sym,
    pub input: i64,
    pub output: i64,
    pub cached: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub total: i64,
    pub ttfc_us: i64,
    pub max_tokens: i64,
    pub messages: i32,
    pub tool_defs: i32,
    pub tool_calls: i32,
    /// -1 unknown, 0 no, 1 yes.
    pub stream: i8,
    // llm.chunk marks: a count, the largest gap and a thinned sample of their times; never the marks themselves
    pub chunks: u32,
    pub chunk_block: u32,
    pub chunk_len: u8,
    pub chunk_stride: u32,
    pub chunk_base_us: i64,
    pub chunk_prev_us: i64,
    pub chunk_run_min_us: i64,
    pub chunk_bridge_us: i64,
    pub chunk_max_gap_us: i64,
    /// The run of rising chunk indices being read, and the range of the runs read before it (-1: none yet).
    pub chunk_run_first: i32,
    pub chunk_run_last: i32,
    pub chunk_done_min: i32,
    pub chunk_done_max: i32,
}

impl LlmRec {
    fn blank() -> LlmRec {
        LlmRec {
            model: Sym::NONE,
            served_model: Sym::NONE,
            provider: Sym::NONE,
            finish_reason: Sym::NONE,
            effort: Sym::NONE,
            correlation: Sym::NONE,
            client: Sym::NONE,
            client_session: Sym::NONE,
            requested_tools: Sym::NONE,
            input: -1,
            output: -1,
            cached: -1,
            cache_write: -1,
            reasoning: -1,
            total: -1,
            ttfc_us: -1,
            max_tokens: -1,
            messages: -1,
            tool_defs: -1,
            tool_calls: -1,
            stream: -1,
            chunks: 0,
            chunk_block: NO_ROW,
            chunk_len: 0,
            chunk_stride: 1,
            chunk_base_us: NO_TS,
            chunk_prev_us: NO_TS,
            chunk_run_min_us: NO_TS,
            chunk_bridge_us: NO_TS,
            chunk_max_gap_us: -1,
            chunk_run_first: -1,
            chunk_run_last: -1,
            chunk_done_min: -1,
            chunk_done_max: -1,
        }
    }

    /// The largest pause between two chunks, in microseconds (`None` with fewer than two chunks).
    #[must_use]
    pub fn max_chunk_gap_us(&self) -> Option<i64> {
        let mut gap = self.chunk_max_gap_us;
        // chunks of an older file arrive after those of a newer one: the pause between the two runs counts too
        if self.chunk_bridge_us != NO_TS && self.chunk_prev_us != NO_TS {
            gap = gap.max(self.chunk_bridge_us - self.chunk_prev_us);
        }
        (gap >= 0).then_some(gap)
    }

    /// Count one llm.chunk mark. False when it was seen before (the same content read twice).
    fn add_chunk(
        &mut self,
        ts_us: i64,
        index: Option<i64>,
        blocks: &mut ChunkVec<ChunkBlock>,
    ) -> bool {
        if let Some(ix) = index {
            let ix = i32::try_from(ix.clamp(0, i64::from(i32::MAX))).unwrap_or(i32::MAX);
            // Within one read the indices of a stream rise. A lower index starts another read: an older file (read
            // after the newer one, so its chunks lie below everything seen), or the same content again. What falls
            // inside a range that was already read is a line seen before.
            if self.chunk_run_last >= 0 && ix <= self.chunk_run_last {
                self.chunk_done_min = if self.chunk_done_min < 0 {
                    self.chunk_run_first
                } else {
                    self.chunk_done_min.min(self.chunk_run_first)
                };
                self.chunk_done_max = self.chunk_done_max.max(self.chunk_run_last);
                self.chunk_run_first = -1;
                self.chunk_run_last = -1;
            }
            if self.chunk_done_min >= 0 && ix >= self.chunk_done_min && ix <= self.chunk_done_max {
                return false;
            }
            if self.chunk_run_last < 0 {
                self.chunk_run_first = ix;
            }
            self.chunk_run_last = ix;
        }
        if self.chunk_prev_us == NO_TS {
            self.chunk_base_us = ts_us;
            self.chunk_run_min_us = ts_us;
        } else if ts_us >= self.chunk_prev_us {
            self.chunk_max_gap_us = self
                .chunk_max_gap_us
                .max(ts_us.saturating_sub(self.chunk_prev_us));
        } else {
            // an older run begins: settle the bridge of the run before, then remember where this newer run began
            if self.chunk_bridge_us != NO_TS {
                self.chunk_max_gap_us = self
                    .chunk_max_gap_us
                    .max(self.chunk_bridge_us.saturating_sub(self.chunk_prev_us));
            }
            self.chunk_bridge_us = self.chunk_run_min_us;
            self.chunk_run_min_us = ts_us;
        }
        self.chunk_prev_us = ts_us;
        // a thinned sample: every `stride`-th chunk; when the block is full keep every other one and double the stride
        if self.chunks.is_multiple_of(self.chunk_stride) {
            if self.chunk_block == NO_ROW {
                self.chunk_block = u32::try_from(blocks.len()).unwrap_or(NO_ROW);
                if self.chunk_block != NO_ROW {
                    blocks.push([0; CHUNK_SLOTS]);
                }
            }
            if let Some(block) = blocks.get_mut(self.chunk_block as usize) {
                if usize::from(self.chunk_len) == CHUNK_SLOTS {
                    for i in 0..CHUNK_SLOTS / 2 {
                        block[i] = block[i * 2];
                    }
                    self.chunk_len = u8::try_from(CHUNK_SLOTS / 2).unwrap_or(u8::MAX);
                    self.chunk_stride = self.chunk_stride.saturating_mul(2);
                }
                if self.chunks.is_multiple_of(self.chunk_stride) {
                    let ms = ts_us.saturating_sub(self.chunk_base_us) / 1000;
                    block[usize::from(self.chunk_len)] =
                        i32::try_from(ms.clamp(i64::from(i32::MIN), i64::from(i32::MAX)))
                            .unwrap_or(0);
                    self.chunk_len += 1;
                }
            }
        }
        self.chunks = self.chunks.saturating_add(1);
        true
    }
}

/// The scalar fields a mark may carry (the adapter's event allowlist plus the routing and hook names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Key {
    Model,
    SelectedModel,
    ServedModel,
    Target,
    TargetModel,
    Algorithm,
    Outcome,
    CallRole,
    Category,
    FailureKind,
    Phase,
    StopReason,
    EventType,
    Provider,
    Decision,
    Route,
    Tier,
    HookEventName,
    ToolName,
    LatencyMs,
    CallIndex,
    UpstreamStatus,
    ChunkIndex,
    FallbackUsed,
    /// Routing evidence (classifier score and its threshold): used for analytics, never returned in `Mark.data`.
    Score,
    Threshold,
}

impl Key {
    /// The key in `Mark.data`; `None` for the two internal ones.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Key::Model => "model",
            Key::SelectedModel => "selected_model",
            Key::ServedModel => "served_model",
            Key::Target => "target",
            Key::TargetModel => "target_model",
            Key::Algorithm => "algorithm",
            Key::Outcome => "outcome",
            Key::CallRole => "call_role",
            Key::Category => "category",
            Key::FailureKind => "failure_kind",
            Key::Phase => "phase",
            Key::StopReason => "stop_reason",
            Key::EventType => "event_type",
            Key::Provider => "provider",
            Key::Decision => "decision",
            Key::Route => "route",
            Key::Tier => "tier",
            Key::HookEventName => "hook_event_name",
            Key::ToolName => "tool_name",
            Key::LatencyMs => "latency_ms",
            Key::CallIndex => "call_index",
            Key::UpstreamStatus => "upstream_status",
            Key::ChunkIndex => "chunk_index",
            Key::FallbackUsed => "fallback_used",
            Key::Score | Key::Threshold => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Val {
    Sym(Sym),
    Num(f64),
    Int(i64),
    Bool(bool),
}

#[derive(Clone, Copy, Debug)]
pub struct MarkField {
    pub key: Key,
    pub val: Val,
}

/// The name of Relay's resource sample marks (addendum 9.17).
pub const RESOURCE_MARK: &str = "nemo.relay.resource_metrics";

/// One measurement of a metric mark.
#[derive(Clone, Copy, Debug)]
pub struct MeasRec {
    pub name: Sym,
    pub kind: Sym,
    pub unit: Sym,
    /// `key=value` pairs joined with commas (identifier text only).
    pub attrs: Sym,
    pub value: f64,
}

/// One mark that is not an llm.chunk.
#[derive(Clone, Copy, Debug)]
pub struct MarkRec {
    pub id: Id,
    pub parent: Id,
    pub ts_us: i64,
    pub name: Sym,
    pub schema: Sym,
    pub severity: Sym,
    pub category: Sym,
    pub fields: u32,
    pub meas: u32,
    pub nfields: u8,
    pub nmeas: u8,
}

/// The row storage a snapshot shares with the scan (see `chunkvec`).
#[derive(Clone, Default)]
pub struct Store {
    pub spans: ChunkVec<SpanRec>,
    pub llm: ChunkVec<LlmRec>,
    pub chunk_blocks: ChunkVec<ChunkBlock>,
    pub marks: ChunkVec<MarkRec>,
    pub mark_fields: ChunkVec<MarkField>,
    pub meas: ChunkVec<MeasRec>,
}

/// Running totals of what the scan saw.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    /// Lines that were events.
    pub events: u64,
    pub chunk_marks: u64,
    /// A scope start or end that the index already had, or a chunk it had counted.
    pub duplicates: u64,
    pub no_timestamp: u64,
    pub bad_id: u64,
    pub oversize_lines: u64,
    pub malformed_lines: u64,
    /// `nemo.relay.resource_metrics` marks (addendum 9.17): counted, the newest time kept, nothing else stored.
    pub resource_marks: u64,
    pub resource_last_us: Option<i64>,
}

/// Symbols the ingest compares against.
#[derive(Clone, Copy)]
pub struct Known {
    pub llm: Sym,
    pub tool: Sym,
    pub agent: Sym,
    pub unknown: Sym,
    pub subagent: Sym,
    pub chunk: Sym,
    pub gateway: Sym,
}

pub struct Ingest {
    pub store: Store,
    pub syms: Syms,
    pub ids: OddIds,
    pub span_by_id: HashMap<Id, u32>,
    pub counters: Counters,
    pub known: Known,
}

fn opt_i32(v: Option<u32>) -> i32 {
    v.map_or(-1, |n| i32::try_from(n).unwrap_or(i32::MAX))
}

fn set_sym(slot: &mut Sym, v: Sym) {
    if slot.is_none() && v.is_some() {
        *slot = v;
    }
}

fn set_i64(slot: &mut i64, v: Option<i64>) {
    if let Some(v) = v {
        *slot = v;
    }
}

/// `mcp__server__tool`.
#[must_use]
pub fn is_mcp_name(name: &str) -> bool {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .is_some_and(|(server, tool)| !server.is_empty() && !tool.is_empty())
}

impl Ingest {
    #[must_use]
    pub fn new(sym_cap: usize) -> Ingest {
        Ingest::with_caps(sym_cap, usize::MAX, OddIds::default())
    }

    /// An ingest whose label table holds at most `sym_cap` strings and `sym_bytes` bytes, with the given id table.
    #[must_use]
    pub fn with_caps(sym_cap: usize, sym_bytes: usize, ids: OddIds) -> Ingest {
        let mut syms = Syms::with_bytes(sym_cap.max(64), sym_bytes.max(4096));
        let known = Known {
            llm: syms.intern(Some("llm")),
            tool: syms.intern(Some("tool")),
            agent: syms.intern(Some("agent")),
            unknown: syms.intern(Some("unknown")),
            subagent: syms.intern(Some("subagent")),
            chunk: syms.intern(Some("llm.chunk")),
            gateway: syms.intern(Some("gateway")),
        };
        Ingest {
            store: Store::default(),
            syms,
            ids,
            span_by_id: HashMap::new(),
            counters: Counters::default(),
            known,
        }
    }

    fn span_slot(&mut self, id: Id) -> Option<u32> {
        if let Some(&ix) = self.span_by_id.get(&id) {
            return Some(ix);
        }
        let ix = u32::try_from(self.store.spans.len())
            .ok()
            .filter(|i| *i != NO_ROW)?;
        self.store.spans.push(SpanRec::blank(id));
        self.span_by_id.insert(id, ix);
        Some(ix)
    }

    fn llm_slot(&mut self, span: u32) -> Option<u32> {
        let row = self.store.spans.get(span as usize)?;
        if row.llm != NO_ROW {
            return Some(row.llm);
        }
        let ix = u32::try_from(self.store.llm.len())
            .ok()
            .filter(|i| *i != NO_ROW)?;
        self.store.llm.push(LlmRec::blank());
        self.store.spans.get_mut(span as usize)?.llm = ix;
        Some(ix)
    }

    /// Identifier lists (flags, tool names) are stored as one symbol: the names joined with commas, which the
    /// identifier alphabet does not contain.
    fn intern_list<'s>(&mut self, items: impl Iterator<Item = &'s str>) -> Sym {
        let joined: Vec<&str> = items.collect();
        if joined.is_empty() {
            return Sym::NONE;
        }
        self.syms.intern_joined(&joined.join(","))
    }

    /// Apply one event. `loc` is where its line is; nothing of the line itself is kept.
    pub fn event(&mut self, ev: &RawEvent<'_>, loc: Loc) {
        self.counters.events += 1;
        let Some(ts) = ev.timestamp.0 else {
            self.counters.no_timestamp += 1;
            return;
        };
        if ev.is_mark() {
            self.mark(ev, ts);
        } else {
            match ev.scope_category.as_str() {
                Some("start") => self.scope(ev, ts, loc, false),
                Some("end") => self.scope(ev, ts, loc, true),
                // a scope event that is neither: nothing to pair it with
                _ => {}
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn scope(&mut self, ev: &RawEvent<'_>, ts: i64, loc: Loc, end: bool) {
        let Some(id) = ev.uuid.as_str().and_then(|s| self.ids.parse(s)) else {
            self.counters.bad_id += 1;
            return;
        };
        let Some(ix) = self.span_slot(id) else { return };
        let flag = if end { bit::HAS_END } else { bit::HAS_START };
        if self
            .store
            .spans
            .get(ix as usize)
            .is_some_and(|r| r.has(flag))
        {
            self.counters.duplicates += 1;
            return;
        }
        let md = ev.metadata.get();
        let profile = ev.category_profile.get();
        let data = ev.data();
        let name = self.syms.intern(ev.name.as_str());
        let category = self.syms.intern(ev.category.as_str());
        let parent = ev
            .parent_uuid
            .as_str()
            .and_then(|s| self.ids.parse(s))
            .filter(|p| *p != id)
            .unwrap_or(Id::NONE);
        let subtype = self.syms.intern(profile.and_then(|p| p.subtype.as_str()));
        let role = self
            .syms
            .intern(md.and_then(|m| m.nemo_relay_scope_role.as_str()));
        let agent_kind = self.syms.intern(md.and_then(|m| m.agent_kind.as_str()));
        let flags = self.intern_list(ev.attributes.items.iter().filter_map(|t| t.as_str()));
        let error_type = self.syms.intern(md.and_then(|m| m.error_type.as_str()));
        let call_id = self
            .syms
            .intern(profile.and_then(|p| p.tool_call_id.as_str()));
        let is_llm = ev.category.as_str() == Some("llm");
        let is_tool = ev.category.as_str() == Some("tool");
        let req = ev.llm_request();
        let relay_session =
            self.syms
                .intern(md.and_then(|m| m.session_id.as_str()).or(if is_llm {
                    req.relay_session
                } else {
                    None
                }));
        let failed =
            md.is_some_and(|m| {
                m.otel_status.as_str() == Some("ERROR")
                    || m.error_type.is_some()
                    || (is_tool
                        && (m.status.as_str() == Some("error")
                            || m.hook_event_name.as_str() == Some("PostToolUseFailure")))
            }) || (is_tool && end && data.is_some_and(|d| d.error.0 || d.is_error.0 == Some(true)));
        let http = md
            .and_then(|m| m.http_status.0.or(m.http_status_legacy.0))
            .and_then(|s| u16::try_from(s).ok())
            .filter(|s| (100..=599).contains(s))
            .unwrap_or(0);
        let turn = md
            .and_then(|m| m.turn_index.0)
            .and_then(|t| i32::try_from(t).ok())
            .filter(|t| *t >= 0);
        let mcp = is_tool && ev.name.as_str().is_some_and(is_mcp_name);
        if let Some(row) = self.store.spans.get_mut(ix as usize) {
            row.bits |= flag;
            if end {
                row.end_us = ts;
                row.loc_end = loc;
            } else {
                row.start_us = ts;
                row.loc_start = loc;
            }
            // what a span is: the first event that says so wins (its start and its end normally agree)
            set_sym(&mut row.name, name);
            set_sym(&mut row.category, category);
            set_sym(&mut row.subtype, subtype);
            set_sym(&mut row.role, role);
            set_sym(&mut row.flags, flags);
            set_sym(&mut row.agent_kind, agent_kind);
            set_sym(&mut row.relay_session, relay_session);
            set_sym(&mut row.error_type, error_type);
            set_sym(&mut row.call_id, call_id);
            if row.parent.is_none() {
                row.parent = parent;
            }
            if let Some(t) = turn
                && row.turn_index < 0
            {
                row.turn_index = t;
            }
            if http != 0 {
                row.http_status = http;
            }
            if failed {
                row.bits |= bit::ERROR;
            }
            if ev.data.present {
                row.bits |= bit::PAYLOAD;
            }
            if mcp {
                row.bits |= bit::MCP;
            }
            if is_llm {
                row.bits |= bit::IS_LLM;
            }
            if is_tool {
                row.bits |= bit::IS_TOOL;
            }
        }
        if !is_llm {
            return;
        }
        let Some(lx) = self.llm_slot(ix) else { return };
        let model = self.syms.intern(req.model);
        let effort = self.syms.intern(req.effort);
        let client = self.syms.intern(req.client);
        let client_session = self.syms.intern(req.client_session);
        let correlation = self
            .syms
            .intern(md.and_then(|m| m.llm_correlation_status.as_str()));
        let anthropic = ev.name.as_str().is_some_and(|n| n.starts_with("anthropic"));
        let resp = if end {
            Some(ev.llm_response(anthropic))
        } else {
            None
        };
        let served = self.syms.intern(resp.as_ref().and_then(|r| r.served_model));
        let finish = self
            .syms
            .intern(resp.as_ref().and_then(|r| r.finish_reason));
        let tools = match &resp {
            Some(r) => self.intern_list(r.tool_names.iter().copied()),
            None => Sym::NONE,
        };
        let Some(l) = self.store.llm.get_mut(lx as usize) else {
            return;
        };
        set_sym(&mut l.model, model);
        set_sym(&mut l.effort, effort);
        set_sym(&mut l.client, client);
        set_sym(&mut l.client_session, client_session);
        set_sym(&mut l.correlation, correlation);
        if let Some(s) = req.stream {
            l.stream = i8::from(s);
        }
        if req.messages.is_some() {
            l.messages = opt_i32(req.messages);
        }
        if req.tool_defs.is_some() {
            l.tool_defs = opt_i32(req.tool_defs);
        }
        set_i64(&mut l.max_tokens, req.max_tokens.filter(|m| *m >= 0));
        if let Some(r) = resp {
            set_sym(&mut l.served_model, served);
            set_sym(&mut l.finish_reason, finish);
            set_sym(&mut l.requested_tools, tools);
            set_i64(&mut l.input, r.tokens.input);
            set_i64(&mut l.output, r.tokens.output);
            set_i64(&mut l.cached, r.tokens.cached);
            set_i64(&mut l.cache_write, r.tokens.cache_write);
            set_i64(&mut l.reasoning, r.tokens.reasoning);
            set_i64(&mut l.total, r.tokens.total);
            set_i64(&mut l.ttfc_us, r.ttfc_us);
            if r.tool_calls.is_some() {
                l.tool_calls = opt_i32(r.tool_calls);
            }
        }
    }

    fn mark(&mut self, ev: &RawEvent<'_>, ts: i64) {
        let parent = ev
            .parent_uuid
            .as_str()
            .and_then(|s| self.ids.parse(s))
            .unwrap_or(Id::NONE);
        let d = ev.data();
        if ev.name.as_str() == Some("llm.chunk") {
            // never stored one by one: the parent LLM span keeps a count, the largest gap and a thinned sample
            self.counters.chunk_marks += 1;
            if parent.is_none() {
                return;
            }
            let Some(ix) = self.span_slot(parent) else {
                return;
            };
            let Some(lx) = self.llm_slot(ix) else { return };
            let provider = self.syms.intern(d.and_then(|d| d.provider.as_str()));
            let index = d.and_then(|d| d.chunk_index.0);
            let Store {
                llm, chunk_blocks, ..
            } = &mut self.store;
            if let Some(l) = llm.get_mut(lx as usize) {
                set_sym(&mut l.provider, provider);
                if !l.add_chunk(ts, index, chunk_blocks) {
                    self.counters.duplicates += 1;
                    self.counters.chunk_marks -= 1;
                }
            }
            return;
        }
        if ev.name.as_str() == Some(RESOURCE_MARK) {
            // a resource sample (about 17 KB of measurements): only its time is needed (the capture view's
            // freshness); it is no part of a trace, a session, the analytics or the orphan counts
            self.counters.resource_marks += 1;
            self.counters.resource_last_us =
                Some(self.counters.resource_last_us.map_or(ts, |t| t.max(ts)));
            return;
        }
        let Some(id) = ev.uuid.as_str().and_then(|s| self.ids.parse(s)) else {
            self.counters.bad_id += 1;
            return;
        };
        let md = ev.metadata.get();
        let schema = ev.data_schema.get();
        let schema_sym = match schema.and_then(|s| s.name.as_str()) {
            Some(n) => {
                let full = match schema.and_then(|s| s.version.0.as_deref()) {
                    Some(v) => format!("{n}/{v}"),
                    None => n.to_owned(),
                };
                self.syms.intern(Some(&full))
            }
            None => Sym::NONE,
        };
        let fields_at = self.store.mark_fields.len();
        let mut n: u8 = 0;
        if let Some(d) = d {
            let mut push_sym = |me: &mut Ingest, key: Key, v: Option<&str>| {
                let s = me.syms.intern(v);
                if s.is_some() {
                    me.store.mark_fields.push(MarkField {
                        key,
                        val: Val::Sym(s),
                    });
                    n += 1;
                }
            };
            push_sym(self, Key::Model, d.model.as_str());
            push_sym(self, Key::SelectedModel, d.selected_model.as_str());
            push_sym(self, Key::ServedModel, d.served_model.as_str());
            push_sym(self, Key::Target, d.target.as_str());
            push_sym(self, Key::TargetModel, d.target_model.as_str());
            push_sym(self, Key::Algorithm, d.algorithm.as_str());
            push_sym(self, Key::Outcome, d.outcome.as_str());
            push_sym(self, Key::CallRole, d.call_role.as_str());
            push_sym(self, Key::Category, d.category.as_str());
            push_sym(self, Key::FailureKind, d.failure_kind.as_str());
            push_sym(self, Key::Phase, d.phase.as_str());
            push_sym(self, Key::StopReason, d.stop_reason.as_str());
            push_sym(self, Key::EventType, d.event_type.as_str());
            push_sym(self, Key::Provider, d.provider.as_str());
            push_sym(self, Key::Decision, d.decision.as_str());
            push_sym(self, Key::Route, d.route.as_str());
            push_sym(self, Key::Tier, d.tier.as_str());
            push_sym(self, Key::HookEventName, d.hook_event_name.as_str());
            push_sym(self, Key::ToolName, d.tool_name.as_str());
            let mut push = |me: &mut Ingest, key: Key, val: Option<Val>| {
                if let Some(val) = val {
                    me.store.mark_fields.push(MarkField { key, val });
                    n += 1;
                }
            };
            push(self, Key::LatencyMs, d.latency_ms.0.map(Val::Num));
            push(self, Key::CallIndex, d.call_index.0.map(Val::Int));
            push(self, Key::UpstreamStatus, d.upstream_status.0.map(Val::Int));
            push(self, Key::ChunkIndex, d.chunk_index.0.map(Val::Int));
            push(self, Key::FallbackUsed, d.fallback_used.0.map(Val::Bool));
            let ev_ = d.evidence.get();
            push(self, Key::Score, ev_.and_then(|e| e.score.0).map(Val::Num));
            push(
                self,
                Key::Threshold,
                ev_.and_then(|e| e.threshold.0).map(Val::Num),
            );
        }
        let meas_at = self.store.meas.len();
        let mut nmeas: u8 = 0;
        if let Some(d) = d {
            for m in &d.measurements.items {
                let attrs: Vec<String> = m
                    .attributes
                    .0
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                let attrs = if attrs.is_empty() {
                    Sym::NONE
                } else {
                    self.syms.intern_joined(&attrs.join(","))
                };
                let rec = MeasRec {
                    name: self.syms.intern(m.name.as_str()),
                    kind: self.syms.intern(m.kind.as_str()),
                    unit: self.syms.intern(m.unit.as_str()),
                    attrs,
                    value: m.value.0.unwrap_or(f64::NAN),
                };
                self.store.meas.push(rec);
                nmeas += 1;
            }
        }
        let rec = MarkRec {
            id,
            parent,
            ts_us: ts,
            name: self.syms.intern(ev.name.as_str()),
            schema: schema_sym,
            severity: self.syms.intern(md.and_then(|m| m.severity.as_str())),
            category: self.syms.intern(ev.category.as_str()),
            fields: u32::try_from(fields_at).unwrap_or(NO_ROW),
            meas: u32::try_from(meas_at).unwrap_or(NO_ROW),
            nfields: n,
            nmeas,
        };
        self.store.marks.push(rec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_names() {
        assert!(is_mcp_name("mcp__docs__search"));
        assert!(is_mcp_name("mcp__a__b__c"));
        assert!(!is_mcp_name("mcp__docs"));
        assert!(!is_mcp_name("mcp____x"));
        assert!(!is_mcp_name("Bash"));
    }

    #[test]
    fn chunk_marks_become_a_count_a_gap_and_a_thinned_sample() {
        let mut blocks = ChunkVec::default();
        let mut l = LlmRec::blank();
        // 1000 chunks, 10 ms apart, with one 700 ms pause before chunk 400
        let ts = |i: i64| 1_000_000 + i * 10_000 + if i >= 400 { 690_000 } else { 0 };
        for i in 0..1000 {
            assert!(l.add_chunk(ts(i), Some(i), &mut blocks));
        }
        assert_eq!(l.chunks, 1000);
        assert_eq!(l.max_chunk_gap_us(), Some(700_000));
        assert!((32..=64).contains(&l.chunk_len));
        assert_eq!(l.chunk_stride, 16);
        let block = blocks.get(l.chunk_block as usize).unwrap();
        let sample = &block[..usize::from(l.chunk_len)];
        assert_eq!(sample[0], 0);
        assert!(sample.windows(2).all(|w| w[0] < w[1]));
        // the same lines again are not counted twice
        for i in 0..1000 {
            assert!(!l.add_chunk(ts(i), Some(i), &mut blocks));
        }
        assert_eq!(l.chunks, 1000);
    }

    #[test]
    fn chunks_of_an_older_file_arrive_later_and_still_give_the_real_gap() {
        let mut blocks = ChunkVec::default();
        let mut l = LlmRec::blank();
        // the newer file holds chunks 5..10 (read first), the older file chunks 0..5; 2 s lie between the files
        for i in 5..10 {
            assert!(l.add_chunk(10_000_000 + i * 100_000, Some(i), &mut blocks));
        }
        for i in 0..5 {
            assert!(l.add_chunk(8_000_000 + i * 100_000, Some(i), &mut blocks));
        }
        assert_eq!(l.chunks, 10);
        // last chunk of the older run at 8.4 s, first of the newer at 10.5 s
        assert_eq!(l.max_chunk_gap_us(), Some(2_100_000));
        let one = {
            let mut l = LlmRec::blank();
            l.add_chunk(5, None, &mut blocks);
            l
        };
        assert_eq!(one.max_chunk_gap_us(), None);
    }
}
