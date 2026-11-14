//! Content, read on demand and handed out carefully.
//!
//! The index never holds text. When an admin asks for the content of a span or a trajectory step, the source line (or
//! file) is parsed again with the types below. They keep at most a fixed number of characters of every string and a
//! fixed number of elements of every list, so memory is bounded by the size of the line that is being read. Before a
//! text leaves this module it goes through the adapter's `scrub()` and the credential-pair pass of `secrets`, values
//! of secret-looking keys are masked, and it is cut to the length the request asked for.

use std::borrow::Cow;
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};

use super::secrets::scrub_content;
use crate::services::common::key_is_sensitive;

/// The longest text a request may ask for (`max_chars`).
pub const MAX_CHARS: usize = 200_000;
/// Kept beyond the requested length while parsing, so that a secret which starts just before the cut is still seen
/// whole by the scrubber. The cut happens after scrubbing.
const SCRUB_MARGIN: usize = 8192;
const CAPTURE_CHARS: usize = MAX_CHARS + SCRUB_MARGIN;
/// Elements kept of one list, keys kept of one object, nesting followed.
const MAX_ITEMS: usize = 256;
const MAX_KEYS: usize = 64;
const MAX_DEPTH: usize = 24;
const MASK: &str = "[REDACTED]";

/// A string with a ceiling: the first [`CAPTURE_CHARS`] characters and the real length.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Text {
    pub text: String,
    pub chars: u64,
}

impl Text {
    fn of(v: &str) -> Text {
        let mut chars = 0u64;
        let mut end = v.len();
        for (n, (i, _)) in v.char_indices().enumerate() {
            if n == CAPTURE_CHARS {
                end = i;
            }
            chars += 1;
        }
        Text {
            text: v[..end].to_owned(),
            chars,
        }
    }
}

/// A JSON value with ceilings everywhere: what the content views are built from.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Node {
    #[default]
    Null,
    Bool(bool),
    /// A number, as written.
    Num(String),
    Str(Text),
    Arr {
        items: Vec<Node>,
        total: u32,
    },
    Obj(Vec<(String, Node)>),
    /// Something nested deeper than the reader follows.
    Deep,
}

struct NodeSeed(usize);

impl<'de> de::DeserializeSeed<'de> for NodeSeed {
    type Value = Node;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        if self.0 >= MAX_DEPTH {
            // skipped without recursion, however deep it goes
            IgnoredAny::deserialize(d)?;
            return Ok(Node::Deep);
        }
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for NodeSeed {
    type Value = Node;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("anything")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Node, E> {
        Ok(Node::Bool(v))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Node, E> {
        Ok(Node::Num(v.to_string()))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Node, E> {
        Ok(Node::Num(v.to_string()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Node, E> {
        Ok(serde_json::Number::from_f64(v).map_or(Node::Null, |n| Node::Num(n.to_string())))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Node, E> {
        Ok(Node::Str(Text::of(v)))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }
    fn visit_none<E: de::Error>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
        let mut items = Vec::new();
        let mut total: u32 = 0;
        loop {
            if items.len() < MAX_ITEMS {
                match a.next_element_seed(NodeSeed(self.0 + 1))? {
                    Some(n) => items.push(n),
                    None => break,
                }
            } else if a.next_element::<IgnoredAny>()?.is_none() {
                break;
            }
            total = total.saturating_add(1);
        }
        Ok(Node::Arr { items, total })
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
        let mut out = Vec::new();
        while let Some(k) = a.next_key::<Cow<'de, str>>()? {
            if out.len() < MAX_KEYS && k.len() <= 80 {
                let v = a.next_value_seed(NodeSeed(self.0 + 1))?;
                out.push((k.into_owned(), v));
            } else {
                a.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Node::Obj(out))
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        de::DeserializeSeed::deserialize(NodeSeed(0), d)
    }
}

impl Node {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    #[must_use]
    pub fn str(&self) -> Option<&Text> {
        match self {
            Node::Str(t) => Some(t),
            _ => None,
        }
    }

    /// A short string (a role, a type, a name), `None` when it is anything else or long.
    #[must_use]
    pub fn short(&self) -> Option<&str> {
        match self {
            Node::Str(t) if t.chars <= 200 => Some(&t.text),
            _ => None,
        }
    }

    #[must_use]
    pub fn items(&self) -> &[Node] {
        match self {
            Node::Arr { items, .. } => items,
            _ => &[],
        }
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Node::Null)
    }

    /// Characters this value would take as text: a string's own length, anything else as compact JSON.
    #[must_use]
    pub fn text_chars(&self) -> u64 {
        match self {
            Node::Null => 0,
            Node::Str(t) => t.chars,
            other => {
                let mut w = JsonOut::counting();
                w.node(other);
                w.chars
            }
        }
    }
}

/// Compact JSON of a [`Node`] with a ceiling, masking the value of every key that looks like a credential
/// (`api_key`, `password`, `token`, ...).
struct JsonOut {
    out: String,
    left: usize,
    chars: u64,
    masked: u64,
}

impl JsonOut {
    fn new() -> JsonOut {
        JsonOut {
            out: String::new(),
            left: CAPTURE_CHARS,
            chars: 0,
            masked: 0,
        }
    }

    fn counting() -> JsonOut {
        JsonOut {
            out: String::new(),
            left: 0,
            chars: 0,
            masked: 0,
        }
    }

    fn raw(&mut self, s: &str) {
        for c in s.chars() {
            self.chars += 1;
            if self.left > 0 {
                self.out.push(c);
                self.left -= 1;
            }
        }
    }

    fn quoted(&mut self, s: &str, cut: bool) {
        self.raw("\"");
        for c in s.chars() {
            match c {
                '"' => self.raw("\\\""),
                '\\' => self.raw("\\\\"),
                '\n' => self.raw("\\n"),
                '\r' => self.raw("\\r"),
                '\t' => self.raw("\\t"),
                c if (c as u32) < 0x20 => self.raw(&format!("\\u{:04x}", c as u32)),
                c => {
                    self.chars += 1;
                    if self.left > 0 {
                        self.out.push(c);
                        self.left -= 1;
                    }
                }
            }
        }
        if cut {
            self.raw("…");
        }
        self.raw("\"");
    }

    fn node(&mut self, n: &Node) {
        match n {
            Node::Null => self.raw("null"),
            Node::Bool(b) => self.raw(if *b { "true" } else { "false" }),
            Node::Num(t) => self.raw(t),
            Node::Str(t) => {
                let cut = t.chars > t.text.chars().count() as u64;
                self.quoted(&t.text, cut);
                // the part of a long string that was not kept still counts towards the size
                self.chars += t.chars.saturating_sub(t.text.chars().count() as u64);
            }
            Node::Deep => self.raw("\"…\""),
            Node::Arr { items, total } => {
                self.raw("[");
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        self.raw(",");
                    }
                    self.node(x);
                }
                if *total as usize > items.len() {
                    self.raw(",\"…\"");
                }
                self.raw("]");
            }
            Node::Obj(kv) => {
                self.raw("{");
                for (i, (k, v)) in kv.iter().enumerate() {
                    if i > 0 {
                        self.raw(",");
                    }
                    self.quoted(k, false);
                    self.raw(":");
                    if key_is_sensitive(k) && !v.is_null() {
                        self.masked += 1;
                        self.quoted(MASK, false);
                    } else {
                        self.node(v);
                    }
                }
                self.raw("}");
            }
        }
    }
}

/// One piece of content as the API returns it (`Part` of addendum 9.12).
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub kind: &'static str,
    pub text: Option<String>,
    pub name: Option<String>,
    pub call_id: Option<String>,
    pub chars: u64,
    pub truncated: bool,
}

impl Part {
    #[must_use]
    pub fn json(&self) -> Value {
        json!({
            "type": self.kind,
            "text": self.text,
            "name": self.name,
            "call_id": self.call_id,
            "chars": self.chars,
            "truncated": self.truncated,
        })
    }
}

fn cut_chars(s: &mut String, max: usize) -> bool {
    match s.char_indices().nth(max) {
        Some((i, _)) => {
            s.truncate(i);
            true
        }
        None => false,
    }
}

/// Builds parts: scrubs, cuts, counts what was replaced.
pub struct Maker {
    pub max_chars: usize,
    pub redactions: u64,
    pub truncated: bool,
}

impl Maker {
    #[must_use]
    pub fn new(max_chars: usize) -> Maker {
        Maker {
            max_chars: max_chars.clamp(1, MAX_CHARS),
            redactions: 0,
            truncated: false,
        }
    }

    /// A short label from a capture (a role, a tool name, a call id): scrubbed and capped.
    pub fn label(&mut self, n: Option<&Node>) -> Option<String> {
        let t = n?.str()?;
        let (mut s, hits) = scrub_content(&t.text);
        self.redactions += hits as u64;
        cut_chars(&mut s, 200);
        Some(s)
    }

    fn finish(&mut self, kind: &'static str, raw: &str, chars: u64, was_cut: bool) -> Part {
        let (mut text, hits) = scrub_content(raw);
        self.redactions += hits as u64;
        let cut = cut_chars(&mut text, self.max_chars) || was_cut;
        self.truncated |= cut;
        Part {
            kind,
            text: Some(text),
            name: None,
            call_id: None,
            chars,
            truncated: cut,
        }
    }

    /// A string as a part.
    pub fn text(&mut self, kind: &'static str, t: &Text) -> Part {
        let was_cut = t.chars > t.text.chars().count() as u64;
        self.finish(kind, &t.text, t.chars, was_cut)
    }

    /// Any value as a part: a string as it is, anything else as compact JSON.
    pub fn value(&mut self, kind: &'static str, n: &Node) -> Part {
        match n {
            Node::Str(t) => self.text(kind, t),
            other => {
                let mut w = JsonOut::new();
                w.node(other);
                self.redactions += w.masked;
                let was_cut = w.chars > w.out.chars().count() as u64;
                self.finish(kind, &w.out, w.chars, was_cut)
            }
        }
    }

    /// Something that is reported by its size only (an image, audio).
    #[must_use]
    pub fn sized(kind: &'static str, chars: u64) -> Part {
        Part {
            kind,
            text: None,
            name: None,
            call_id: None,
            chars,
            truncated: false,
        }
    }

    /// The content of a message or a tool result: a string, or a list of blocks, as parts.
    pub fn content(&mut self, n: &Node, default_kind: &'static str, out: &mut Vec<Part>) {
        match n {
            Node::Null => {}
            Node::Str(t) => out.push(self.text(default_kind, t)),
            Node::Arr { items, total } => {
                for b in items {
                    if out.len() >= MAX_PARTS {
                        self.truncated = true;
                        break;
                    }
                    self.block(b, default_kind, out);
                }
                if *total as usize > items.len() {
                    self.truncated = true;
                }
            }
            // one block on its own (an object), or a bare number or flag
            other @ Node::Obj(_) => self.block(other, default_kind, out),
            other => out.push(Maker::sized("other", other.text_chars())),
        }
    }

    /// One content block (Anthropic and OpenAI shapes).
    fn block(&mut self, b: &Node, default_kind: &'static str, out: &mut Vec<Part>) {
        if let Node::Str(t) = b {
            out.push(self.text(default_kind, t));
            return;
        }
        let kind = b.get("type").and_then(Node::short).unwrap_or("");
        match kind {
            "text" | "input_text" | "output_text" => {
                if let Some(t) = b.get("text").and_then(Node::str) {
                    out.push(self.text(default_kind, t));
                }
            }
            "thinking" | "reasoning" | "reasoning_text" => {
                if let Some(t) = b
                    .get("thinking")
                    .or_else(|| b.get("text"))
                    .and_then(Node::str)
                {
                    out.push(self.text("reasoning", t));
                }
            }
            "tool_use" | "function_call" => {
                let args = b.get("input").or_else(|| b.get("arguments"));
                let mut p = match args {
                    Some(a) => self.value("tool_call", a),
                    None => Maker::sized("tool_call", 0),
                };
                p.name = self.label(b.get("name"));
                p.call_id = self.label(b.get("id").or_else(|| b.get("call_id")));
                out.push(p);
            }
            "tool_result" | "function_call_output" => {
                let mut inner = Vec::new();
                if let Some(c) = b.get("content").or_else(|| b.get("output")) {
                    self.content(c, "tool_result", &mut inner);
                }
                let call_id = self.label(b.get("tool_use_id").or_else(|| b.get("call_id")));
                if inner.is_empty() {
                    inner.push(Maker::sized("tool_result", 0));
                }
                for mut p in inner {
                    if p.kind == "text" {
                        p.kind = "tool_result";
                    }
                    p.call_id.clone_from(&call_id);
                    out.push(p);
                }
            }
            "image" | "image_url" | "input_image" | "input_audio" | "audio" | "document"
            | "file" => {
                // never the data: only how large it is
                out.push(Maker::sized("image", b.text_chars()));
            }
            // a block the console does not know: its kind and size only, never its data (audit F7)
            _ => out.push(Maker::sized("other", b.text_chars())),
        }
    }

    /// One chat message (OpenAI or Anthropic shape, or Relay's annotated copy) as `{role, parts}`.
    pub fn message(&mut self, m: &Node) -> Value {
        // Relay wraps a provider-specific message: {role: "provider_native", kind, value: {the message}}
        let (m, outer_role) = match m.get("value") {
            Some(v @ Node::Obj(_)) => (v, m.get("kind").or_else(|| m.get("role"))),
            _ => (m, None),
        };
        let role = self.label(m.get("role").or(outer_role));
        let is_tool = role.as_deref() == Some("tool");
        let mut parts = Vec::new();
        if let Some(r) = m.get("reasoning_content").and_then(Node::str)
            && r.chars > 0
        {
            parts.push(self.text("reasoning", r));
        }
        if let Some(c) = m.get("content") {
            self.content(c, if is_tool { "tool_result" } else { "text" }, &mut parts);
        }
        if is_tool {
            let call_id = self.label(m.get("tool_call_id"));
            for p in &mut parts {
                if p.call_id.is_none() {
                    p.call_id.clone_from(&call_id);
                }
            }
        }
        for tc in m.get("tool_calls").map(Node::items).unwrap_or_default() {
            if parts.len() >= MAX_PARTS {
                self.truncated = true;
                break;
            }
            let f = tc.get("function").unwrap_or(tc);
            let mut p = match f.get("arguments") {
                Some(a) => self.value("tool_call", a),
                None => Maker::sized("tool_call", 0),
            };
            p.name = self.label(f.get("name"));
            p.call_id = self.label(tc.get("id"));
            parts.push(p);
        }
        json!({"role": role, "parts": parts.iter().map(Part::json).collect::<Vec<_>>()})
    }
}

/// Parts kept of one message.
const MAX_PARTS: usize = 64;

/// The most bytes a content answer may have as JSON (addendum 9.12: "the whole answer <= 4 MiB"), less a little
/// room for the two fields the caller sets after [`fit`].
pub const ANSWER_MAX_BYTES: usize = 4 * 1024 * 1024 - 4096;

/// Cut the texts of a finished answer further until it fits `budget` bytes of text, then, if the JSON is still over
/// [`ANSWER_MAX_BYTES`] (many parts with long names or ids), shorten every string and drop the second half of the
/// largest list until it fits. True when anything was cut.
pub fn fit(v: &mut Value, budget: usize) -> bool {
    let mut any = fit_texts(v, budget);
    let size = |v: &Value| serde_json::to_vec(v).map_or(usize::MAX, |b| b.len());
    let mut shortened = false;
    while size(v) > ANSWER_MAX_BYTES {
        any = true;
        if !shortened {
            shorten_strings(v, 64);
            shortened = true;
            continue;
        }
        if !halve_largest_list(v) {
            break;
        }
    }
    any
}

fn shorten_strings(v: &mut Value, cap: usize) {
    match v {
        Value::String(s) => {
            cut_chars(s, cap);
        }
        Value::Array(a) => a.iter_mut().for_each(|x| shorten_strings(x, cap)),
        Value::Object(o) => {
            let shortened = match o.get_mut("text") {
                Some(Value::String(s)) => s.chars().count() > cap,
                _ => false,
            };
            o.values_mut().for_each(|x| shorten_strings(x, cap));
            if shortened && o.contains_key("truncated") {
                o.insert("truncated".to_owned(), Value::Bool(true));
            }
        }
        _ => {}
    }
}

/// Drop the second half of the largest list with two or more elements. False when there is none.
fn halve_largest_list(v: &mut Value) -> bool {
    fn weight(v: &Value) -> usize {
        match v {
            Value::String(s) => s.len() + 2,
            Value::Array(a) => a.iter().map(weight).sum::<usize>() + 2,
            Value::Object(o) => {
                o.iter()
                    .map(|(k, x)| k.len() + 3 + weight(x))
                    .sum::<usize>()
                    + 2
            }
            _ => 8,
        }
    }
    // the path to the heaviest list: walk down, remembering the best list seen
    fn best(v: &Value, path: &mut Vec<PathStep>, out: &mut Option<(usize, Vec<PathStep>)>) {
        match v {
            Value::Array(a) => {
                if a.len() >= 2 {
                    let w = weight(v);
                    if out.as_ref().is_none_or(|(bw, _)| w > *bw) {
                        *out = Some((w, path.clone()));
                    }
                }
                for (i, x) in a.iter().enumerate() {
                    path.push(PathStep::Index(i));
                    best(x, path, out);
                    path.pop();
                }
            }
            Value::Object(o) => {
                for (k, x) in o {
                    path.push(PathStep::Key(k.clone()));
                    best(x, path, out);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut found = None;
    best(v, &mut Vec::new(), &mut found);
    let Some((_, path)) = found else {
        return false;
    };
    let mut at = v;
    for step in &path {
        let next = match (step, at) {
            (PathStep::Index(i), Value::Array(a)) => a.get_mut(*i),
            (PathStep::Key(k), Value::Object(o)) => o.get_mut(k),
            _ => None,
        };
        let Some(n) = next else {
            return false;
        };
        at = n;
    }
    match at {
        Value::Array(a) if a.len() >= 2 => {
            let keep = a.len() / 2;
            a.truncate(keep);
            true
        }
        _ => false,
    }
}

#[derive(Clone)]
enum PathStep {
    Index(usize),
    Key(String),
}

/// The text cuts of [`fit`]: shorten texts and descriptions until the answer holds about `budget` bytes of text.
fn fit_texts(v: &mut Value, budget: usize) -> bool {
    fn total(v: &Value) -> usize {
        match v {
            Value::String(s) => s.len(),
            Value::Array(a) => a.iter().map(total).sum(),
            Value::Object(o) => o.iter().map(|(k, x)| k.len() + total(x)).sum(),
            _ => 8,
        }
    }
    fn cut(v: &mut Value, cap: usize) {
        match v {
            Value::Array(a) => a.iter_mut().for_each(|x| cut(x, cap)),
            Value::Object(o) => {
                // a part is an object with "text" and "truncated": shorten its text and say so
                let shortened = match o.get_mut("text") {
                    Some(Value::String(s)) => cut_chars(s, cap),
                    _ => false,
                };
                if shortened && o.contains_key("truncated") {
                    o.insert("truncated".to_owned(), Value::Bool(true));
                }
                for (k, x) in o.iter_mut() {
                    if k == "description" {
                        if let Value::String(s) = x {
                            cut_chars(s, cap);
                        }
                    } else if k != "text" {
                        cut(x, cap);
                    }
                }
            }
            _ => {}
        }
    }
    let mut cap = MAX_CHARS;
    let mut any = false;
    while total(v) > budget && cap > 16 {
        cap = cap * 3 / 4;
        cut(v, cap);
        any = true;
    }
    any
}

/// A scalar request parameter (`temperature`, `max_tokens`, `stream`, a short string), or nothing.
#[derive(Default)]
pub struct Scalar(pub Option<Value>);

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Scalar;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Scalar, E> {
                Ok(Scalar(Some(json!(v))))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Scalar, E> {
                Ok(Scalar(Some(json!(v))))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Scalar, E> {
                Ok(Scalar(Some(json!(v))))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Scalar, E> {
                Ok(Scalar(serde_json::Number::from_f64(v).map(Value::Number)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Scalar, E> {
                Ok(Scalar(
                    (v.chars().count() <= 200).then(|| Value::String(v.to_owned())),
                ))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Scalar, E> {
                Ok(Scalar(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Scalar, A::Error> {
                super::lenient::drain_seq(a)?;
                Ok(Scalar(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Scalar, A::Error> {
                super::lenient::drain_map(a)?;
                Ok(Scalar(None))
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(text: &str) -> Node {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn nodes_have_ceilings() {
        let long = "x".repeat(CAPTURE_CHARS + 500);
        let many: Vec<String> = (0..MAX_ITEMS + 10).map(|i| i.to_string()).collect();
        let deep = format!("{}1{}", "[".repeat(100), "]".repeat(100));
        let n = node(&format!(
            r#"{{"s":"{long}","a":[{}],"d":{deep},"n":1.5,"b":true,"z":null}}"#,
            many.join(",")
        ));
        let s = n.get("s").unwrap().str().unwrap();
        assert_eq!(s.chars, (CAPTURE_CHARS + 500) as u64);
        assert_eq!(s.text.len(), CAPTURE_CHARS);
        match n.get("a").unwrap() {
            Node::Arr { items, total } => {
                assert_eq!((items.len(), *total as usize), (MAX_ITEMS, MAX_ITEMS + 10));
            }
            _ => panic!(),
        }
        // the deep value is followed to the limit, the rest is a marker
        let mut d = n.get("d").unwrap();
        let mut levels = 0;
        while let Node::Arr { items, .. } = d {
            d = &items[0];
            levels += 1;
        }
        assert_eq!(*d, Node::Deep);
        assert!(levels < MAX_DEPTH + 1);
        assert_eq!(n.get("n"), Some(&Node::Num("1.5".to_owned())));
        // multi-byte characters are counted as characters and never split
        let t = Text::of(&"é".repeat(CAPTURE_CHARS + 3));
        assert_eq!(t.chars, (CAPTURE_CHARS + 3) as u64);
        assert_eq!(t.text.chars().count(), CAPTURE_CHARS);
    }

    #[test]
    fn parts_are_scrubbed_cut_and_counted() {
        let mut mk = Maker::new(1000);
        let key = format!("sk-{}", "a1".repeat(12));
        let n = node(&format!(
            r#"{{"role":"user","content":"my key is {key} and Authorization: Bearer abcdefgh12345678 end {}"}}"#,
            "y".repeat(3000)
        ));
        let m = mk.message(&n);
        let text = m["parts"][0]["text"].as_str().unwrap();
        assert!(!text.contains(&key) && !text.contains("abcdefgh12345678"));
        assert!(text.contains("[REDACTED]") && text.contains("Bearer [REDACTED]"));
        assert_eq!(text.chars().count(), 1000);
        assert_eq!(m["parts"][0]["truncated"], true);
        assert!(m["parts"][0]["chars"].as_u64().unwrap() > 3000);
        assert_eq!(mk.redactions, 2);
        assert!(mk.truncated);
        assert_eq!(m["role"], "user");
    }

    #[test]
    fn a_secret_that_straddles_the_cut_does_not_leak_its_first_half() {
        let mut mk = Maker::new(1000);
        let key = format!("sk-{}", "b2".repeat(20));
        let body = format!("{} {key} tail", "z".repeat(989));
        let p = mk.text("text", &Text::of(&body));
        let t = p.text.unwrap();
        assert_eq!(t.chars().count(), 1000);
        assert!(!t.contains("sk-b2"), "{}", &t[980..]);
        assert_eq!(mk.redactions, 1);
    }

    #[test]
    fn message_shapes() {
        let mut mk = Maker::new(5000);
        // OpenAI assistant message with reasoning and a tool call, then the tool's answer
        let m = mk.message(&node(
            r#"{"role":"assistant","content":null,"reasoning_content":"think","tool_calls":[{"id":"call_1","type":"function","function":{"name":"grep","arguments":"{\"pattern\":\"x\"}"}}]}"#,
        ));
        assert_eq!(m["parts"][0]["type"], "reasoning");
        assert_eq!(m["parts"][1]["type"], "tool_call");
        assert_eq!(m["parts"][1]["name"], "grep");
        assert_eq!(m["parts"][1]["call_id"], "call_1");
        assert_eq!(m["parts"][1]["text"], "{\"pattern\":\"x\"}");
        let m = mk.message(&node(
            r#"{"role":"tool","tool_call_id":"call_1","content":"3 matches"}"#,
        ));
        assert_eq!(m["parts"][0]["type"], "tool_result");
        assert_eq!(m["parts"][0]["call_id"], "call_1");
        // Anthropic blocks: text, thinking, tool_use with an object, tool_result with nested blocks, an image
        let m = mk.message(&node(
            r#"{"role":"user","content":[{"type":"text","text":"hello"},{"type":"thinking","thinking":"hm"},
               {"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"ls","api_key":"abcdef123456"}},
               {"type":"tool_result","tool_use_id":"tu_1","content":[{"type":"text","text":"a.txt"},{"type":"image","source":{"type":"base64","data":"AAAA"}}]},
               {"type":"image","source":{"type":"base64","media_type":"image/png","data":"QUJDREVGR0g="}},
               {"type":"mystery","x":1}, "bare string"]}"#,
        ));
        let kinds: Vec<&str> = m["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "text",
                "reasoning",
                "tool_call",
                "tool_result",
                "image",
                "image",
                "other",
                "text"
            ]
        );
        let call = &m["parts"][2];
        assert_eq!(call["name"], "Bash");
        assert_eq!(call["text"], r#"{"command":"ls","api_key":"[REDACTED]"}"#);
        assert_eq!(m["parts"][3]["call_id"], "tu_1");
        assert_eq!(m["parts"][5]["text"], Value::Null);
        assert!(m["parts"][5]["chars"].as_u64().unwrap() > 12);
        assert!(mk.redactions >= 1);
        // Relay's wrapper around a provider message
        let m = mk.message(&node(
            r#"{"role":"provider_native","kind":"assistant","provider":"openai","value":{"role":"assistant","content":"done"}}"#,
        ));
        assert_eq!(m["role"], "assistant");
        assert_eq!(m["parts"][0]["text"], "done");
        // not a message at all
        let m = mk.message(&node("7"));
        assert_eq!(m["role"], Value::Null);
        assert_eq!(m["parts"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn an_answer_is_cut_to_its_budget() {
        let mut mk = Maker::new(MAX_CHARS);
        let parts: Vec<Value> = (0..40)
            .map(|_| mk.text("text", &Text::of(&"w".repeat(150_000))).json())
            .collect();
        let mut v = json!({"messages": [{"role": "user", "parts": parts}], "tools": [{"name": "t", "description": "d".repeat(50_000)}]});
        assert!(fit(&mut v, 1_000_000));
        assert!(serde_json::to_vec(&v).unwrap().len() < 1_100_000);
        assert_eq!(v["messages"][0]["parts"][0]["truncated"], true);
        let mut small = json!({"parts": [{"text": "short", "truncated": false}]});
        assert!(!fit(&mut small, 1000));
        assert_eq!(small["parts"][0]["truncated"], false);
    }

    /// Audit F7: many parts with long names and call ids (which the text cuts do not shorten) still fit 4 MiB.
    #[test]
    fn an_answer_never_exceeds_four_mib() {
        let long = "n".repeat(200);
        let part = json!({"type": "tool_call", "text": "x", "name": long, "call_id": long, "chars": 1, "truncated": false});
        let messages: Vec<Value> = (0..400)
            .map(|_| json!({"role": "assistant", "parts": vec![part.clone(); 64]}))
            .collect();
        let mut v = json!({"request": {"messages": messages}, "truncated": false, "redactions": 0});
        assert!(serde_json::to_vec(&v).unwrap().len() > 8 * 1024 * 1024);
        assert!(fit(&mut v, 3_400_000));
        assert!(
            serde_json::to_vec(&v).unwrap().len() <= ANSWER_MAX_BYTES,
            "over 4 MiB"
        );
        assert!(!v["request"]["messages"].as_array().unwrap().is_empty());
    }

    /// Audit F7: a block of a type the console does not know is reported by its size only, never its data.
    #[test]
    fn an_unknown_block_carries_only_its_type_and_size() {
        let blob = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo".repeat(100);
        let mut mk = Maker::new(MAX_CHARS);
        let mut parts = Vec::new();
        mk.content(
            &node(&format!(
                r#"[{{"type":"weird_blob","data":"{blob}"}},{{"type":"text","text":"ok"}}]"#
            )),
            "text",
            &mut parts,
        );
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].kind, "other");
        assert_eq!(parts[0].text, None);
        assert!(parts[0].chars > 3000);
        assert!(!parts[0].json().to_string().contains("QUJD"));
        // content that is a bare object or number: the same
        let mut more = Vec::new();
        mk.content(&node(&format!(r#"{{"blob":"{blob}"}}"#)), "text", &mut more);
        mk.content(&node("12345"), "text", &mut more);
        for p in &more {
            assert_eq!(p.kind, "other");
            assert_eq!(p.text, None);
        }
    }
}
