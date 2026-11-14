//! The content of one span, read from its source lines when an admin asks for it.
//!
//! The index remembers where a span's start and end lines are (which content, which offset, how long). This module
//! opens that file through the usual checks, proves that the file is still the content the index read and that the
//! bytes at the offset are still that span's event, and only then builds the answer: capped, scrubbed, with a header
//! allowlist. Nothing from a request names a file; the only inputs are ids that were looked up in the index.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::io::{Seek as _, SeekFrom};
use std::path::Path;

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};

use super::event::{Parsed, parse_line};
use super::lenient::{Opt, Seq, skip_kinds};
use super::reader::{LineReader, hash128, open_in, read_at};
use super::rows::{Loc, SpanRec, bit};
use super::secrets::scrub_content;
use super::snapshot::{Core, count_json};
use super::text::{Maker, Node, Part, Scalar, Text, fit};
use crate::services::common::key_is_sensitive;
use crate::services::gzip::GzReader;

/// Text bytes an answer may hold; with the JSON around it the whole answer stays under 4 MiB.
pub const ANSWER_TEXT_BUDGET: usize = 3_400_000;
/// Messages of a request that are returned.
pub const MAX_MESSAGES: usize = 400;
const MAX_TOOLS: usize = 200;
const MAX_PARAMS: usize = 40;
const MAX_HEADERS: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct ContentQuery {
    pub max_chars: usize,
    /// The first messages instead of the last ones.
    pub head: bool,
}

/// The source line of the span can no longer be read (rotated away, rewritten, removed).
#[derive(Debug, PartialEq, Eq)]
pub struct Unavailable;

/// The request headers that may be shown, by exact name: the client's name, content type, trace context, the
/// session ids and NeMo Relay's own id headers. Nothing else, whatever its prefix (audit F5).
const HEADER_ALLOWLIST: [&str; 18] = [
    "user-agent",
    "content-type",
    "traceparent",
    "tracestate",
    "x-opencode-client",
    "x-opencode-session-id",
    "x-session-id",
    "x-claude-code-session-id",
    "x-nemo-relay-agent-kind",
    "x-nemo-relay-identity-quality",
    "x-nemo-relay-parent-scope-id",
    "x-nemo-relay-request-id",
    "x-nemo-relay-root-scope-id",
    "x-nemo-relay-session-id",
    "x-nemo-relay-source",
    "x-nemo-relay-turn-id",
    "x-nemo-relay-owner-id",
    "x-nemo-relay-subagent-id",
];

/// True for a header name on [`HEADER_ALLOWLIST`] (case-insensitive).
#[must_use]
pub fn header_allowed(name: &str) -> bool {
    HEADER_ALLOWLIST
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name))
}

/// True when the (scrubbed) value of an allowlisted header may be shown: a user agent is printable ASCII of at most
/// 256 characters; every other header is identifier-shaped (at most 200 characters of letters, digits and
/// `_ . : / @ + -`, plus `, ; =` and blanks for the trace context lists).
#[must_use]
pub fn header_value_ok(name: &str, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    if name.eq_ignore_ascii_case("user-agent") {
        return value.len() <= 256 && value.bytes().all(|b| (0x20..0x7f).contains(&b));
    }
    value.len() <= 200
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'_' | b'.' | b':' | b'/' | b'@' | b'+' | b'-' | b',' | b';' | b'=' | b' '
                )
        })
}

/// The allowlisted headers of a request, values capped.
#[derive(Default)]
struct HeaderCapture(Vec<(String, String)>);

impl<'de> Deserialize<'de> for HeaderCapture {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = HeaderCapture;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<HeaderCapture, A::Error> {
                let mut out = Vec::new();
                while let Some(k) = a.next_key::<Cow<'de, str>>()? {
                    if out.len() < MAX_HEADERS && k.len() <= 64 && header_allowed(&k) {
                        if let Scalar(Some(Value::String(v))) = a.next_value::<Scalar>()? {
                            out.push((k.to_ascii_lowercase(), v));
                        }
                    } else {
                        a.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(HeaderCapture(out))
            }
            skip_kinds!(bool, int, float, str, unit, seq);
        }
        d.deserialize_any(V)
    }
}

/// The messages of a request: the first [`MAX_MESSAGES`] and, when there are more, the last ones too.
#[derive(Default)]
struct Messages {
    head: Vec<Node>,
    tail: VecDeque<Node>,
    total: u32,
}

impl<'de> Deserialize<'de> for Messages {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Messages;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Messages, A::Error> {
                let mut m = Messages::default();
                while let Some(n) = a.next_element::<Node>()? {
                    m.total = m.total.saturating_add(1);
                    if m.head.len() < MAX_MESSAGES {
                        m.head.push(n);
                    } else {
                        if m.tail.len() == MAX_MESSAGES {
                            m.tail.pop_front();
                        }
                        m.tail.push_back(n);
                    }
                }
                Ok(m)
            }
            skip_kinds!(bool, int, float, str, unit, map);
        }
        d.deserialize_any(V)
    }
}

impl Messages {
    /// The messages to return, oldest first.
    fn pick(&self, head: bool) -> Vec<&Node> {
        if head || self.tail.is_empty() {
            return self.head.iter().collect();
        }
        // the last MAX_MESSAGES: the end of `head` (when the tail is short) followed by the tail
        let from_head = MAX_MESSAGES - self.tail.len();
        self.head[self.head.len() - from_head..]
            .iter()
            .chain(self.tail.iter())
            .collect()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ToolFn {
    name: Node,
    description: Node,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ToolDef {
    name: Node,
    description: Node,
    function: Opt<ToolFn>,
}

/// A request body: what the content view shows of it.
#[derive(Default)]
struct ReqCapture {
    present: bool,
    model: Node,
    params: Vec<(String, Value)>,
    system: Node,
    messages: Messages,
    tools: Seq<ToolDef, MAX_TOOLS>,
}

impl<'de> Deserialize<'de> for ReqCapture {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ReqCapture;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<ReqCapture, A::Error> {
                let mut r = ReqCapture {
                    present: true,
                    ..ReqCapture::default()
                };
                while let Some(k) = a.next_key::<Cow<'de, str>>()? {
                    match &*k {
                        "model" => r.model = a.next_value()?,
                        "messages" => r.messages = a.next_value()?,
                        // `system` is the Anthropic field, `instructions` Relay's annotated name for it
                        "system" | "instructions" => r.system = a.next_value()?,
                        "tools" => r.tools = a.next_value()?,
                        // Relay's annotated copy keeps the sampling parameters in an object of their own
                        "params" => {
                            if let Node::Obj(kv) = a.next_value::<Node>()? {
                                for (pk, pv) in kv {
                                    let v = match pv {
                                        Node::Bool(b) => Some(json!(b)),
                                        Node::Num(t) => serde_json::from_str::<Value>(&t).ok(),
                                        Node::Str(t) if t.chars <= 200 => {
                                            Some(Value::String(t.text))
                                        }
                                        _ => None,
                                    };
                                    if let Some(v) = v
                                        && r.params.len() < MAX_PARAMS
                                        && !key_is_sensitive(&pk)
                                    {
                                        r.params.push((pk, v));
                                    }
                                }
                            }
                        }
                        other => {
                            // every other scalar of the body is a parameter (temperature, max_tokens, stream, ...)
                            let keep = r.params.len() < MAX_PARAMS
                                && other.len() <= 64
                                && !key_is_sensitive(other);
                            match a.next_value::<Scalar>()? {
                                Scalar(Some(v)) if keep => r.params.push((other.to_owned(), v)),
                                _ => {}
                            }
                        }
                    }
                }
                Ok(r)
            }
            skip_kinds!(bool, int, float, str, unit, seq);
        }
        d.deserialize_any(V)
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StartData {
    content: ReqCapture,
    headers: HeaderCapture,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StartProfile {
    annotated_request: ReqCapture,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LlmStart {
    data: Opt<StartData>,
    category_profile: Opt<StartProfile>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EndProfile {
    annotated_response: Node,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LlmEnd {
    data: Node,
    category_profile: Opt<EndProfile>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AnyLine {
    data: Node,
}

/// The bytes of one source line, after proving that the file is the content the index read.
fn read_line(
    core: &Core,
    dir: &Path,
    loc: Loc,
    max_line: usize,
) -> Result<(Vec<u8>, String, bool), Unavailable> {
    if !loc.is_some() || loc.len as usize > max_line {
        return Err(Unavailable);
    }
    let src = core
        .sources
        .get(loc.lineage as usize)
        .and_then(Option::as_ref)
        .ok_or(Unavailable)?;
    let opened = open_in(dir, &src.name).map_err(|_| Unavailable)?;
    let len = loc.len as usize;
    let bytes = if src.gz {
        let file = &opened.file;
        let mut lr = LineReader::new(GzReader::new(file), 0, max_line);
        let head = lr
            .peek_head(src.head_len as usize)
            .map_err(|_| Unavailable)?;
        if head.len() != src.head_len as usize || hash128(head) != src.head_hash {
            return Err(Unavailable);
        }
        // the nearest seek point at or before the line, taken from THIS file (same device, inode, size and time)
        let key = (opened.dev, opened.ino, opened.size, opened.mtime_ns);
        let point = src
            .points
            .as_deref()
            .filter(|p| p.key == key)
            .and_then(|p| {
                let i = p.points.partition_point(|c| c.out <= loc.offset);
                i.checked_sub(1).and_then(|i| p.points.get(i))
            });
        let mut buf = Vec::with_capacity(len);
        if let Some(cp) = point {
            // resume the decoder there and read only what lies between the point and the line
            drop(lr);
            let mut f = file;
            f.seek(SeekFrom::Start(cp.bit_pos / 8))
                .map_err(|_| Unavailable)?;
            let gz = GzReader::resume(f, cp).map_err(|_| Unavailable)?;
            let mut lr = LineReader::new(gz, cp.out, max_line);
            let gap = loc.offset - cp.out;
            if lr.skip_bytes(gap, |_, _| {}).map_err(|_| Unavailable)? != gap {
                return Err(Unavailable);
            }
            lr.skip_bytes(len as u64, |_, b| buf.extend_from_slice(b))
                .map_err(|_| Unavailable)?;
        } else {
            // no seek point before the line: read past everything before it
            if lr
                .skip_bytes(loc.offset, |_, _| {})
                .map_err(|_| Unavailable)?
                != loc.offset
            {
                return Err(Unavailable);
            }
            lr.skip_bytes(len as u64, |_, b| buf.extend_from_slice(b))
                .map_err(|_| Unavailable)?;
        }
        buf
    } else {
        let head = read_at(&opened.file, 0, src.head_len as usize).map_err(|_| Unavailable)?;
        if head.len() != src.head_len as usize || hash128(&head) != src.head_hash {
            return Err(Unavailable);
        }
        read_at(&opened.file, loc.offset, len).map_err(|_| Unavailable)?
    };
    if bytes.len() != len {
        return Err(Unavailable);
    }
    Ok((bytes, src.name.clone(), src.gz))
}

/// True when the line is the start (or end) event of the span with this id.
fn is_event_of(line: &[u8], id_text: &str, end: bool) -> bool {
    match parse_line(line) {
        Parsed::Event(ev) => {
            ev.uuid
                .as_str()
                .is_some_and(|u| u.eq_ignore_ascii_case(id_text))
                && ev.scope_category.as_str() == Some(if end { "end" } else { "start" })
        }
        _ => false,
    }
}

fn parts_json(parts: &[Part]) -> Vec<Value> {
    parts.iter().map(Part::json).collect()
}

fn request_json(mk: &mut Maker, req: &ReqCapture, q: ContentQuery) -> Value {
    let chosen = req.messages.pick(q.head);
    let messages: Vec<Value> = chosen.iter().map(|m| mk.message(m)).collect();
    if (req.messages.total as usize) > messages.len() {
        mk.truncated = true;
    }
    let mut system = Vec::new();
    mk.content(&req.system, "text", &mut system);
    let tools: Vec<Value> = req
        .tools
        .items
        .iter()
        .map(|t| {
            let f = t.function.get();
            let name = mk.label(Some(f.map_or(&t.name, |f| &f.name)));
            let description = match f.map_or(&t.description, |f| &f.description) {
                Node::Str(text) => {
                    // a description is a hint, not the payload: 1000 characters of it
                    let short = Text {
                        text: text.text.chars().take(1000 + 256).collect(),
                        chars: text.chars,
                    };
                    let (mut s, hits) = scrub_content(&short.text);
                    mk.redactions += hits as u64;
                    if let Some((i, _)) = s.char_indices().nth(1000) {
                        s.truncate(i);
                        mk.truncated = true;
                    }
                    Value::String(s)
                }
                _ => Value::Null,
            };
            json!({"name": name, "description": description})
        })
        .collect();
    if req.tools.total as usize > req.tools.items.len() {
        mk.truncated = true;
    }
    let mut params = Map::new();
    for (k, v) in &req.params {
        let v = match v {
            Value::String(s) => {
                let (s, hits) = scrub_content(s);
                mk.redactions += hits as u64;
                Value::String(s)
            }
            other => other.clone(),
        };
        params.insert(k.clone(), v);
    }
    json!({
        "model": mk.label(Some(&req.model)),
        "params": params,
        "system": parts_json(&system),
        "messages": messages,
        "tools": tools,
        "messages_total": req.messages.total,
        "messages_returned": messages.len(),
    })
}

fn response_json(mk: &mut Maker, core: &Core, s: &SpanRec, end: &LlmEnd) -> Value {
    let d = &end.data;
    let ann = end
        .category_profile
        .get()
        .map(|p| &p.annotated_response)
        .filter(|n| !n.is_null());
    let mut parts: Vec<Part> = Vec::new();
    let finish: Option<String>;
    let mut model = mk.label(d.get("model"));
    if let Some(choice) = d.get("choices").map(Node::items).and_then(<[Node]>::first) {
        // OpenAI: the answer is the first choice's message
        if let Some(m) = choice.get("message") {
            if let Some(r) = m.get("reasoning_content").and_then(Node::str)
                && r.chars > 0
            {
                parts.push(mk.text("reasoning", r));
            }
            if let Some(c) = m.get("content") {
                mk.content(c, "text", &mut parts);
            }
            for tc in m.get("tool_calls").map(Node::items).unwrap_or_default() {
                let f = tc.get("function").unwrap_or(tc);
                let mut p = match f.get("arguments") {
                    Some(a) => mk.value("tool_call", a),
                    None => Maker::sized("tool_call", 0),
                };
                p.name = mk.label(f.get("name"));
                p.call_id = mk.label(tc.get("id"));
                parts.push(p);
            }
        }
        finish = mk.label(choice.get("finish_reason"));
    } else if let Some(c) = d.get("content") {
        // Anthropic: content blocks
        mk.content(c, "text", &mut parts);
        finish = mk.label(d.get("stop_reason"));
    } else if let Some(a) = ann {
        // only Relay's annotated copy is there
        if let Some(t) = a.get("message").and_then(Node::str)
            && t.chars > 0
        {
            parts.push(mk.text("text", t));
        }
        for tc in a.get("tool_calls").map(Node::items).unwrap_or_default() {
            let mut p = match tc.get("arguments") {
                Some(x) => mk.value("tool_call", x),
                None => Maker::sized("tool_call", 0),
            };
            p.name = mk.label(tc.get("name"));
            p.call_id = mk.label(tc.get("id"));
            parts.push(p);
        }
        finish = mk.label(a.get("finish_reason"));
        model = model.or_else(|| mk.label(a.get("model")));
    } else {
        return Value::Null;
    }
    let usage = core.llm_of(s).map_or(Value::Null, |l| {
        json!({
            "input_tokens": count_json(l.input),
            "output_tokens": count_json(l.output),
            "cached_tokens": count_json(l.cached),
            "cache_write_tokens": count_json(l.cache_write),
            "reasoning_tokens": count_json(l.reasoning),
            "total_tokens": count_json(l.total),
        })
    });
    json!({
        "model": model,
        "finish_reason": finish,
        "parts": parts_json(&parts),
        "usage": usage,
    })
}

/// `SpanContent` of the span in row `row`.
pub fn span_content(
    core: &Core,
    dir: &Path,
    row: u32,
    q: ContentQuery,
    max_line: usize,
) -> Result<Value, Unavailable> {
    let s = core.span(row).ok_or(Unavailable)?;
    let id_text = core.odd.text(s.id).ok_or(Unavailable)?;
    let read = |loc: Loc, end: bool| -> Option<(Vec<u8>, String, bool)> {
        let (bytes, name, gz) = read_line(core, dir, loc, max_line).ok()?;
        is_event_of(&bytes, &id_text, end).then_some((bytes, name, gz))
    };
    let start = read(s.loc_start, false);
    let end = read(s.loc_end, true);
    let (file, compressed) = match (&start, &end) {
        (Some((_, n, gz)), _) | (None, Some((_, n, gz))) => (n.clone(), *gz),
        (None, None) => return Err(Unavailable),
    };
    let mut mk = Maker::new(q.max_chars);
    let mut request = Value::Null;
    let mut response = Value::Null;
    let mut tool = Value::Null;
    let mut headers = Map::new();
    if s.has(bit::IS_LLM) {
        if let Some((bytes, ..)) = &start
            && let Ok(line) = serde_json::from_slice::<LlmStart>(bytes)
        {
            let data = line.data.0.unwrap_or_default();
            let annotated = line.category_profile.0.map(|p| p.annotated_request);
            // what the client sent comes first; Relay's annotated copy stands in when the raw body is missing
            let req = if data.content.present {
                Some(&data.content)
            } else {
                annotated.as_ref().filter(|a| a.present)
            };
            if let Some(req) = req {
                request = request_json(&mut mk, req, q);
            }
            for (k, v) in data.headers.0 {
                let (v, hits) = scrub_content(&v);
                mk.redactions += hits as u64;
                // a value that is not plain (or that the scrubber changed) is not shown at all
                if hits == 0 && header_value_ok(&k, &v) {
                    headers.insert(k, Value::String(v));
                }
            }
        }
        if let Some((bytes, ..)) = &end
            && let Ok(line) = serde_json::from_slice::<LlmEnd>(bytes)
        {
            response = response_json(&mut mk, core, s, &line);
        }
    } else if s.has(bit::IS_TOOL) {
        let part = |mk: &mut Maker, bytes: Option<&Vec<u8>>, kind: &'static str| -> Value {
            bytes
                .and_then(|b| serde_json::from_slice::<AnyLine>(b).ok())
                .filter(|l| !l.data.is_null())
                .map_or(Value::Null, |l| mk.value(kind, &l.data).json())
        };
        tool = json!({
            "arguments": part(&mut mk, start.as_ref().map(|x| &x.0), "tool_call"),
            "result": part(&mut mk, end.as_ref().map(|x| &x.0), "tool_result"),
        });
    }
    let mut v = json!({
        "span_id": id_text,
        "category": core.sym(s.category),
        "source": {"file": file, "compressed": compressed},
        "request": request,
        "response": response,
        "tool": tool,
        "headers": headers,
        "truncated": false,
        "redactions": 0,
    });
    let cut = fit(&mut v, ANSWER_TEXT_BUDGET);
    v["truncated"] = json!(mk.truncated || cut);
    v["redactions"] = json!(mk.redactions);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_harmless_headers_are_shown() {
        for ok in [
            "user-agent",
            "Content-Type",
            "traceparent",
            "x-opencode-client",
            "x-session-id",
            "x-opencode-session-id",
            "x-claude-code-session-id",
            "x-nemo-relay-session-id",
            "x-nemo-relay-turn-id",
        ] {
            assert!(header_allowed(ok), "{ok}");
        }
        for no in [
            "authorization",
            "Authorization",
            "proxy-authorization",
            "cookie",
            "set-cookie",
            "x-api-key",
            "x-nemo-relay-api-key",
            "x-nemo-relay-token",
            "x-nemo-relay-auth",
            "x-nemo-relay-upstream-secret",
            "x-goog-api-key",
            "anthropic-version",
            "accept",
            "x-stainless-os",
            "b3",
            "x-opencode-project",
            "x-nemo-relay-apikey",
            "x-nemo-relay-api_key",
            "x-nemo-relay-bearer",
            "x-nemo-relay-passwd",
            "x-nemo-relay-pwd",
            "x-nemo-relay-pat",
            "x-nemo-relay-jwt",
            "x-nemo-relay-anything",
        ] {
            assert!(!header_allowed(no), "{no}");
        }
        assert!(header_value_ok(
            "user-agent",
            "claude-cli/1.0 (external, cli)"
        ));
        assert!(!header_value_ok("user-agent", "bad\u{7}bell"));
        assert!(header_value_ok("tracestate", "a=1,b=2"));
        assert!(!header_value_ok("x-session-id", "<script>"));
        assert!(!header_value_ok("x-session-id", &"a".repeat(201)));
    }

    #[test]
    fn the_last_messages_of_a_long_request() {
        let body = format!(
            "[{}]",
            (0..1000)
                .map(|i| format!(r#"{{"role":"user","content":"m{i}"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let m: Messages = serde_json::from_str(&body).unwrap();
        assert_eq!(m.total, 1000);
        let text = |n: &Node| n.get("content").unwrap().str().unwrap().text.clone();
        let tail = m.pick(false);
        assert_eq!(tail.len(), MAX_MESSAGES);
        assert_eq!(
            (text(tail[0]), text(tail[399])),
            ("m600".to_owned(), "m999".to_owned())
        );
        let head = m.pick(true);
        assert_eq!(
            (text(head[0]), text(head[399])),
            ("m0".to_owned(), "m399".to_owned())
        );
        // between 400 and 800: the tail is short and the end of the head completes it
        let body = format!(
            "[{}]",
            (0..450)
                .map(|i| format!(r#"{{"role":"user","content":"m{i}"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let m: Messages = serde_json::from_str(&body).unwrap();
        let tail = m.pick(false);
        assert_eq!(tail.len(), MAX_MESSAGES);
        assert_eq!(
            (text(tail[0]), text(tail[399])),
            ("m50".to_owned(), "m449".to_owned())
        );
        let m: Messages = serde_json::from_str(r#"[{"role":"user","content":"only"}]"#).unwrap();
        assert_eq!(m.pick(false).len(), 1);
        let m: Messages = serde_json::from_str("\"not a list\"").unwrap();
        assert_eq!(m.total, 0);
    }

    #[test]
    fn request_parameters_are_scalars_and_never_credentials() {
        let r: ReqCapture = serde_json::from_str(
            r#"{"model":"local-small","temperature":0.2,"max_tokens":64,"stream":true,"stop":["a"],"metadata":{"user_id":"x"},
                "api_key":"abcdef0123456789","tool_choice":"auto","messages":[],"tools":[{"type":"function","function":{"name":"grep","description":"find","parameters":{"type":"object"}}},{"name":"Bash","description":"run","input_schema":{}}]}"#,
        )
        .unwrap();
        let keys: Vec<&str> = r.params.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["temperature", "max_tokens", "stream", "tool_choice"]);
        assert_eq!(r.tools.items.len(), 2);
        let mut mk = Maker::new(1000);
        let v = request_json(
            &mut mk,
            &r,
            ContentQuery {
                max_chars: 1000,
                head: false,
            },
        );
        assert_eq!(v["model"], "local-small");
        assert_eq!(v["tools"][0]["name"], "grep");
        assert_eq!(v["tools"][1]["description"], "run");
        assert_eq!(v["params"]["temperature"], 0.2);
        assert_eq!(v["messages_total"], 0);
        assert!(!v.to_string().contains("abcdef0123456789"));
    }
}
