//! `yaml.safe_load(text)` of PyYAML 6.0.3 (its pure-Python `SafeLoader`: reader, scanner, parser, composer,
//! resolver, `SafeConstructor`), ported function by function so the workstream C adapters see the same objects AND the same
//! errors as the Python backend: the evaluator shows `YAML error: <problem>` with `problem_mark.line + 1`, and both
//! adapters treat "not a yaml.YAMLError" (ValueError, KeyError, ... raised by constructors) differently from YAML
//! errors. Pinned by goldens generated with PyYAML itself (tests/fixtures/evaluator/yaml_golden.json,
//! parity/gen/gen_yaml_golden.py).
//!
//! Only `str` input is supported (the adapters read files as text, like `Path.read_text()`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::services::pyfmt::str_repr;
use crate::services::pyobj::{
    BigUint, Date, DateTime, Doc, MAX_DEPTH, Obj, ObjId, Py, PyExc, PyInt, canonical_nan, py_float,
    py_int, timedelta_repr,
};

// ── errors ────────────────────────────────────────────────────────────────────────────────────────

/// `yaml.error.Mark` of a `str` stream (`name` is always `<unicode string>`; pointer == index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub index: usize,
    pub line: usize,
    pub column: usize,
}

/// A `MarkedYAMLError` (ScannerError, ParserError, ComposerError, ConstructorError).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marked {
    pub class: &'static str,
    pub context: Option<String>,
    pub context_mark: Option<Mark>,
    pub problem: Option<String>,
    pub problem_mark: Option<Mark>,
    /// `str(e)`.
    pub text: String,
}

/// What `yaml.safe_load` raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlError {
    Marked(Box<Marked>),
    /// `ReaderError` (a `YAMLError` without `problem` / `problem_mark`).
    Reader(String),
    /// Not a `yaml.YAMLError`: an exception from a constructor (ValueError, KeyError, ...).
    Py(PyExc),
}

impl YamlError {
    /// `isinstance(e, yaml.YAMLError)`.
    #[must_use]
    pub fn is_yaml(&self) -> bool {
        !matches!(self, YamlError::Py(_))
    }

    /// `type(e).__name__`.
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            YamlError::Marked(m) => m.class,
            YamlError::Reader(_) => "ReaderError",
            YamlError::Py(p) => p.class,
        }
    }

    /// `str(e)`.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            YamlError::Marked(m) => m.text.clone(),
            YamlError::Reader(t) => t.clone(),
            YamlError::Py(p) => p.message.clone(),
        }
    }

    /// `getattr(e, "problem", None)`.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        match self {
            YamlError::Marked(m) => m.problem.as_deref(),
            _ => None,
        }
    }

    /// `getattr(e, "context", None)`.
    #[must_use]
    pub fn context(&self) -> Option<&str> {
        match self {
            YamlError::Marked(m) => m.context.as_deref(),
            _ => None,
        }
    }

    /// `str(getattr(e, "problem", e))`: the problem when the error has the attribute (a marked error), else `str(e)`.
    #[must_use]
    pub fn problem_or_str(&self) -> String {
        match self {
            YamlError::Marked(m) => m.problem.clone().unwrap_or_else(|| "None".to_owned()),
            other => other.text(),
        }
    }

    /// `str(e)` as the hub's `full` text: the message, prefixed with the class for an exception that is not a
    /// `yaml.YAMLError` (how such an exception prints in a traceback's last line).
    #[must_use]
    pub fn full(&self) -> String {
        match self {
            YamlError::Py(p) => format!("{}: {}", p.class, p.message),
            other => other.text(),
        }
    }

    /// `getattr(e, "problem_mark", None)`.
    #[must_use]
    pub fn problem_mark(&self) -> Option<Mark> {
        match self {
            YamlError::Marked(m) => m.problem_mark,
            _ => None,
        }
    }
}

/// `str(e)`.
impl std::fmt::Display for YamlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())
    }
}

impl std::error::Error for YamlError {}

impl From<PyExc> for YamlError {
    fn from(e: PyExc) -> Self {
        YamlError::Py(e)
    }
}

type R<T> = Result<T, YamlError>;

const BREAKS: &str = "\r\n\u{85}\u{2028}\u{2029}";
const BREAKZ: &str = "\0\r\n\u{85}\u{2028}\u{2029}";
const BLANKZ_T: &str = "\0 \t\r\n\u{85}\u{2028}\u{2029}";
const BLANKZ: &str = "\0 \r\n\u{85}\u{2028}\u{2029}";

fn is_in(c: char, set: &str) -> bool {
    set.contains(c)
}

fn alnum_dash(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

// ── tokens / events / nodes ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tk {
    Directive,
    DocumentStart,
    DocumentEnd,
    StreamStart,
    StreamEnd,
    BlockSequenceStart,
    BlockMappingStart,
    BlockEnd,
    FlowSequenceStart,
    FlowMappingStart,
    FlowSequenceEnd,
    FlowMappingEnd,
    Key,
    Value,
    BlockEntry,
    FlowEntry,
    Alias,
    Anchor,
    Tag,
    Scalar,
}

impl Tk {
    fn id(self) -> &'static str {
        match self {
            Tk::Directive => "<directive>",
            Tk::DocumentStart => "<document start>",
            Tk::DocumentEnd => "<document end>",
            Tk::StreamStart => "<stream start>",
            Tk::StreamEnd => "<stream end>",
            Tk::BlockSequenceStart => "<block sequence start>",
            Tk::BlockMappingStart => "<block mapping start>",
            Tk::BlockEnd => "<block end>",
            Tk::FlowSequenceStart => "[",
            Tk::FlowMappingStart => "{",
            Tk::FlowSequenceEnd => "]",
            Tk::FlowMappingEnd => "}",
            Tk::Key => "?",
            Tk::Value => ":",
            Tk::BlockEntry => "-",
            Tk::FlowEntry => ",",
            Tk::Alias => "<alias>",
            Tk::Anchor => "<anchor>",
            Tk::Tag => "<tag>",
            Tk::Scalar => "<scalar>",
        }
    }
}

#[derive(Debug, Clone)]
enum DirVal {
    None,
    Yaml(u128, u128),
    Tag(String, String),
}

#[derive(Debug, Clone)]
struct Token {
    kind: Tk,
    start: Mark,
    end: Mark,
    /// alias/anchor name, scalar value, directive name
    value: String,
    plain: bool,
    style: Option<char>,
    tag: Option<(Option<String>, String)>,
    dir: DirVal,
}

impl Token {
    fn new(kind: Tk, start: Mark, end: Mark) -> Self {
        Token {
            kind,
            start,
            end,
            value: String::new(),
            plain: false,
            style: None,
            tag: None,
            dir: DirVal::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ek {
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    Alias,
    Scalar,
    SequenceStart,
    SequenceEnd,
    MappingStart,
    MappingEnd,
}

#[derive(Debug, Clone)]
struct Event {
    kind: Ek,
    start: Mark,
    end: Mark,
    anchor: Option<String>,
    tag: Option<String>,
    implicit: (bool, bool),
    value: String,
}

impl Event {
    fn new(kind: Ek, start: Mark, end: Mark) -> Self {
        Event {
            kind,
            start,
            end,
            anchor: None,
            tag: None,
            implicit: (false, false),
            value: String::new(),
        }
    }
}

/// Index of a node in [`Composed`] (and in the loader's node arena).
pub type NodeId = usize;

#[derive(Debug, Clone)]
enum NodeVal {
    Scalar(String),
    Seq(Vec<NodeId>),
    Map(Vec<(NodeId, NodeId)>),
}

#[derive(Debug, Clone)]
struct Node {
    tag: String,
    val: NodeVal,
    start: Mark,
}

impl Node {
    fn id(&self) -> &'static str {
        match self.val {
            NodeVal::Scalar(_) => "scalar",
            NodeVal::Seq(_) => "sequence",
            NodeVal::Map(_) => "mapping",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PState {
    ImplicitDocumentStart,
    DocumentStart,
    DocumentEnd,
    DocumentContent,
    BlockNode,
    BlockSequenceFirstEntry,
    BlockSequenceEntry,
    IndentlessSequenceEntry,
    BlockMappingFirstKey,
    BlockMappingKey,
    BlockMappingValue,
    FlowSequenceFirstEntry,
    FlowSequenceEntry,
    FlowSequenceEntryMappingKey,
    FlowSequenceEntryMappingValue,
    FlowSequenceEntryMappingEnd,
    FlowMappingFirstKey,
    FlowMappingKey,
    FlowMappingValue,
    FlowMappingEmptyValue,
    StreamStart,
}

#[derive(Debug, Clone)]
struct SimpleKey {
    token_number: usize,
    required: bool,
    index: usize,
    line: usize,
    mark: Mark,
}

#[derive(Debug, Clone, Copy)]
enum Gen {
    Seq(NodeId, ObjId),
    Map(NodeId, ObjId),
    Set(NodeId, ObjId),
    Omap(NodeId, ObjId),
    Pairs(NodeId, ObjId),
}

const T_NULL: &str = "tag:yaml.org,2002:null";
const T_BOOL: &str = "tag:yaml.org,2002:bool";
const T_INT: &str = "tag:yaml.org,2002:int";
const T_FLOAT: &str = "tag:yaml.org,2002:float";
const T_BINARY: &str = "tag:yaml.org,2002:binary";
const T_TIMESTAMP: &str = "tag:yaml.org,2002:timestamp";
const T_OMAP: &str = "tag:yaml.org,2002:omap";
const T_PAIRS: &str = "tag:yaml.org,2002:pairs";
const T_SET: &str = "tag:yaml.org,2002:set";
const T_STR: &str = "tag:yaml.org,2002:str";
const T_SEQ: &str = "tag:yaml.org,2002:seq";
const T_MAP: &str = "tag:yaml.org,2002:map";
const T_MERGE: &str = "tag:yaml.org,2002:merge";
const T_VALUE: &str = "tag:yaml.org,2002:value";

/// `yaml.safe_load(text)`.
pub fn safe_load(text: &str) -> Result<Doc, YamlError> {
    check_printable(text)?;
    let mut l = Loader::new(text);
    l.get_single_data()
}

/// Lead (A-7): what `yaml.compose(text)` returns: the node graph of the single document, before construction.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// `ScalarNode.value` (the scalar's text).
    Scalar(String),
    /// `SequenceNode.value`.
    Seq(Vec<NodeId>),
    /// `MappingNode.value`: (key node, value node) pairs in document order.
    Map(Vec<(NodeId, NodeId)>),
}

/// A composed node: its resolved tag, kind and `start_mark`.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposedNode {
    pub tag: String,
    pub kind: NodeKind,
    pub start: Mark,
}

/// The result of [`compose`]: `root` is `None` for an empty stream. Aliases point at the anchored node (shared ids).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Composed {
    pub root: Option<NodeId>,
    nodes: Vec<ComposedNode>,
}

impl Composed {
    /// The node with this id.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&ComposedNode> {
        self.nodes.get(id)
    }
}

/// `yaml.compose(text)` (the default `yaml.Loader` composes like `SafeLoader`: same reader, scanner, parser, composer
/// and resolver), errors included.
pub fn compose(text: &str) -> Result<Composed, YamlError> {
    check_printable(text)?;
    let mut l = Loader::new(text);
    let root = l.get_single_node()?;
    let nodes = std::mem::take(&mut l.nodes)
        .into_iter()
        .map(|n| ComposedNode {
            tag: n.tag,
            kind: match n.val {
                NodeVal::Scalar(v) => NodeKind::Scalar(v),
                NodeVal::Seq(v) => NodeKind::Seq(v),
                NodeVal::Map(v) => NodeKind::Map(v),
            },
            start: n.start,
        })
        .collect();
    Ok(Composed { root, nodes })
}

/// Reader.check_printable for a `str` stream.
fn check_printable(text: &str) -> R<()> {
    for (pos, c) in text.chars().enumerate() {
        let ok = matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{7e}' | '\u{85}' | '\u{a0}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}');
        if !ok {
            return Err(YamlError::Reader(format!(
                "unacceptable character #x{:04x}: special characters are not allowed\n  in \"<unicode string>\", position {pos}",
                u32::from(c)
            )));
        }
    }
    Ok(())
}

struct Loader {
    buf: Vec<char>,
    ptr: usize,
    line: usize,
    column: usize,
    // scanner
    done: bool,
    flow_level: usize,
    tokens: VecDeque<Token>,
    tokens_taken: usize,
    indent: isize,
    indents: Vec<isize>,
    allow_simple_key: bool,
    possible_simple_keys: IndexMap<usize, SimpleKey>,
    // parser
    current_event: Option<Event>,
    yaml_version: Option<(u128, u128)>,
    tag_handles: IndexMap<String, String>,
    states: Vec<PState>,
    marks: Vec<Mark>,
    state: Option<PState>,
    // composer
    anchors: HashMap<String, NodeId>,
    nodes: Vec<Node>,
    // constructor
    objs: Vec<Obj>,
    constructed: HashMap<NodeId, Py>,
    recursive: HashSet<NodeId>,
    gens: Vec<Gen>,
}

impl Loader {
    fn new(text: &str) -> Self {
        let mut buf: Vec<char> = text.chars().collect();
        buf.push('\0');
        let mut l = Loader {
            buf,
            ptr: 0,
            line: 0,
            column: 0,
            done: false,
            flow_level: 0,
            tokens: VecDeque::new(),
            tokens_taken: 0,
            indent: -1,
            indents: Vec::new(),
            allow_simple_key: true,
            possible_simple_keys: IndexMap::new(),
            current_event: None,
            yaml_version: None,
            tag_handles: IndexMap::new(),
            states: Vec::new(),
            marks: Vec::new(),
            state: Some(PState::StreamStart),
            anchors: HashMap::new(),
            nodes: Vec::new(),
            objs: Vec::new(),
            constructed: HashMap::new(),
            recursive: HashSet::new(),
            gens: Vec::new(),
        };
        // fetch_stream_start
        let m = l.mark();
        l.tokens.push_back(Token::new(Tk::StreamStart, m, m));
        l
    }

    // ── reader ──────────────────────────────────────────────────────────────────────────────

    fn peek(&self, i: usize) -> char {
        self.buf.get(self.ptr + i).copied().unwrap_or('\0')
    }

    fn ch(&self) -> char {
        self.peek(0)
    }

    fn prefix(&self, n: usize) -> String {
        let end = (self.ptr + n).min(self.buf.len());
        self.buf[self.ptr.min(end)..end].iter().collect()
    }

    fn forward(&mut self, n: usize) {
        for _ in 0..n {
            let Some(&c) = self.buf.get(self.ptr) else {
                return;
            };
            self.ptr += 1;
            if is_in(c, "\n\u{85}\u{2028}\u{2029}")
                || (c == '\r' && self.buf.get(self.ptr).copied() != Some('\n'))
            {
                self.line += 1;
                self.column = 0;
            } else if c != '\u{feff}' {
                self.column += 1;
            }
        }
    }

    fn mark(&self) -> Mark {
        Mark {
            index: self.ptr,
            line: self.line,
            column: self.column,
        }
    }

    fn snippet(&self, m: Mark) -> String {
        let (indent, max_half) = (4usize, 75.0f64 / 2.0 - 1.0);
        let pointer = m.index;
        let mut head = "";
        let mut start = pointer;
        while start > 0 && !is_in(self.buf[start - 1], BREAKZ) {
            start -= 1;
            #[allow(clippy::cast_precision_loss)]
            if (pointer - start) as f64 > max_half {
                head = " ... ";
                start += 5;
                break;
            }
        }
        let mut tail = "";
        let mut end = pointer;
        while end < self.buf.len() && !is_in(self.buf[end], BREAKZ) {
            end += 1;
            #[allow(clippy::cast_precision_loss)]
            if (end - pointer) as f64 > max_half {
                tail = " ... ";
                end -= 5;
                break;
            }
        }
        let snippet: String = self.buf[start..end].iter().collect();
        format!(
            "{}{head}{snippet}{tail}\n{}^",
            " ".repeat(indent),
            " ".repeat(indent + pointer - start + head.chars().count())
        )
    }

    fn mark_str(&self, m: Mark) -> String {
        format!(
            "  in \"<unicode string>\", line {}, column {}:\n{}",
            m.line + 1,
            m.column + 1,
            self.snippet(m)
        )
    }

    fn err(
        &self,
        class: &'static str,
        context: Option<&str>,
        context_mark: Option<Mark>,
        problem: &str,
        problem_mark: Option<Mark>,
    ) -> YamlError {
        let mut lines: Vec<String> = Vec::new();
        if let Some(c) = context {
            lines.push(c.to_owned());
        }
        if let Some(cm) = context_mark
            && (problem_mark.is_none_or(|pm| pm.line != cm.line || pm.column != cm.column))
        {
            lines.push(self.mark_str(cm));
        }
        lines.push(problem.to_owned());
        if let Some(pm) = problem_mark {
            lines.push(self.mark_str(pm));
        }
        YamlError::Marked(Box::new(Marked {
            class,
            context: context.map(str::to_owned),
            context_mark,
            problem: Some(problem.to_owned()),
            problem_mark,
            text: lines.join("\n"),
        }))
    }

    fn scan_err(&self, context: Option<&str>, cm: Option<Mark>, problem: &str) -> YamlError {
        self.err("ScannerError", context, cm, problem, Some(self.mark()))
    }

    // ── scanner: public ─────────────────────────────────────────────────────────────────────

    fn check_token(&mut self, kinds: &[Tk]) -> R<bool> {
        while self.need_more_tokens()? {
            self.fetch_more_tokens()?;
        }
        Ok(self
            .tokens
            .front()
            .is_some_and(|t| kinds.is_empty() || kinds.contains(&t.kind)))
    }

    fn peek_token(&mut self) -> R<Token> {
        while self.need_more_tokens()? {
            self.fetch_more_tokens()?;
        }
        // the parser never peeks past STREAM-END
        Ok(self.tokens.front().cloned().unwrap_or_else(|| {
            let m = self.mark();
            Token::new(Tk::StreamEnd, m, m)
        }))
    }

    fn get_token(&mut self) -> R<Token> {
        while self.need_more_tokens()? {
            self.fetch_more_tokens()?;
        }
        match self.tokens.pop_front() {
            Some(t) => {
                self.tokens_taken += 1;
                Ok(t)
            }
            None => {
                let m = self.mark();
                Ok(Token::new(Tk::StreamEnd, m, m))
            }
        }
    }

    fn need_more_tokens(&mut self) -> R<bool> {
        if self.done {
            return Ok(false);
        }
        if self.tokens.is_empty() {
            return Ok(true);
        }
        self.stale_possible_simple_keys()?;
        Ok(self.next_possible_simple_key() == Some(self.tokens_taken))
    }

    fn fetch_more_tokens(&mut self) -> R<()> {
        self.scan_to_next_token();
        self.stale_possible_simple_keys()?;
        #[allow(clippy::cast_possible_wrap)]
        self.unwind_indent(self.column as isize);
        let ch = self.ch();
        if ch == '\0' {
            return self.fetch_stream_end();
        }
        if ch == '%' && self.column == 0 {
            return self.fetch_directive();
        }
        if ch == '-' && self.check_document_indicator("---") {
            return self.fetch_document_indicator(Tk::DocumentStart);
        }
        if ch == '.' && self.check_document_indicator("...") {
            return self.fetch_document_indicator(Tk::DocumentEnd);
        }
        match ch {
            '[' => return self.fetch_flow_collection_start(Tk::FlowSequenceStart),
            '{' => return self.fetch_flow_collection_start(Tk::FlowMappingStart),
            ']' => return self.fetch_flow_collection_end(Tk::FlowSequenceEnd),
            '}' => return self.fetch_flow_collection_end(Tk::FlowMappingEnd),
            ',' => return self.fetch_flow_entry(),
            _ => {}
        }
        if ch == '-' && is_in(self.peek(1), BLANKZ_T) {
            return self.fetch_block_entry();
        }
        if ch == '?' && (self.flow_level > 0 || is_in(self.peek(1), BLANKZ_T)) {
            return self.fetch_key();
        }
        if ch == ':' && (self.flow_level > 0 || is_in(self.peek(1), BLANKZ_T)) {
            return self.fetch_value();
        }
        match ch {
            '*' => return self.fetch_anchor(Tk::Alias),
            '&' => return self.fetch_anchor(Tk::Anchor),
            '!' => return self.fetch_tag(),
            '|' if self.flow_level == 0 => return self.fetch_block_scalar('|'),
            '>' if self.flow_level == 0 => return self.fetch_block_scalar('>'),
            '\'' => return self.fetch_flow_scalar('\''),
            '"' => return self.fetch_flow_scalar('"'),
            _ => {}
        }
        if self.check_plain() {
            return self.fetch_plain();
        }
        Err(self.scan_err(
            Some("while scanning for the next token"),
            None,
            &format!(
                "found character {} that cannot start any token",
                str_repr(&ch.to_string())
            ),
        ))
    }

    // ── simple keys ─────────────────────────────────────────────────────────────────────────

    fn next_possible_simple_key(&self) -> Option<usize> {
        self.possible_simple_keys
            .values()
            .map(|k| k.token_number)
            .min()
    }

    fn stale_possible_simple_keys(&mut self) -> R<()> {
        let levels: Vec<usize> = self.possible_simple_keys.keys().copied().collect();
        for level in levels {
            let Some(key) = self.possible_simple_keys.get(&level).cloned() else {
                continue;
            };
            if key.line != self.line || self.ptr - key.index > 1024 {
                if key.required {
                    return Err(self.scan_err(
                        Some("while scanning a simple key"),
                        Some(key.mark),
                        "could not find expected ':'",
                    ));
                }
                self.possible_simple_keys.shift_remove(&level);
            }
        }
        Ok(())
    }

    fn save_possible_simple_key(&mut self) -> R<()> {
        #[allow(clippy::cast_possible_wrap)]
        let required = self.flow_level == 0 && self.indent == self.column as isize;
        if self.allow_simple_key {
            self.remove_possible_simple_key()?;
            let token_number = self.tokens_taken + self.tokens.len();
            let key = SimpleKey {
                token_number,
                required,
                index: self.ptr,
                line: self.line,
                mark: self.mark(),
            };
            self.possible_simple_keys.insert(self.flow_level, key);
        }
        Ok(())
    }

    fn remove_possible_simple_key(&mut self) -> R<()> {
        if let Some(key) = self.possible_simple_keys.get(&self.flow_level) {
            if key.required {
                return Err(self.scan_err(
                    Some("while scanning a simple key"),
                    Some(key.mark),
                    "could not find expected ':'",
                ));
            }
            self.possible_simple_keys.shift_remove(&self.flow_level);
        }
        Ok(())
    }

    // ── indentation ─────────────────────────────────────────────────────────────────────────

    fn unwind_indent(&mut self, column: isize) {
        if self.flow_level > 0 {
            return;
        }
        while self.indent > column {
            let m = self.mark();
            self.indent = self.indents.pop().unwrap_or(-1);
            self.tokens.push_back(Token::new(Tk::BlockEnd, m, m));
        }
    }

    fn add_indent(&mut self, column: isize) -> bool {
        if self.indent < column {
            self.indents.push(self.indent);
            self.indent = column;
            return true;
        }
        false
    }

    // ── fetchers ────────────────────────────────────────────────────────────────────────────

    fn fetch_stream_end(&mut self) -> R<()> {
        self.unwind_indent(-1);
        self.remove_possible_simple_key()?;
        self.allow_simple_key = false;
        self.possible_simple_keys.clear();
        let m = self.mark();
        self.tokens.push_back(Token::new(Tk::StreamEnd, m, m));
        self.done = true;
        Ok(())
    }

    fn fetch_directive(&mut self) -> R<()> {
        self.unwind_indent(-1);
        self.remove_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_directive()?;
        self.tokens.push_back(t);
        Ok(())
    }

    fn check_document_indicator(&self, ind: &str) -> bool {
        self.column == 0 && self.prefix(3) == ind && is_in(self.peek(3), BLANKZ_T)
    }

    fn fetch_document_indicator(&mut self, kind: Tk) -> R<()> {
        self.unwind_indent(-1);
        self.remove_possible_simple_key()?;
        self.allow_simple_key = false;
        let s = self.mark();
        self.forward(3);
        let e = self.mark();
        self.tokens.push_back(Token::new(kind, s, e));
        Ok(())
    }

    fn fetch_flow_collection_start(&mut self, kind: Tk) -> R<()> {
        self.save_possible_simple_key()?;
        self.flow_level += 1;
        self.allow_simple_key = true;
        let s = self.mark();
        self.forward(1);
        let e = self.mark();
        self.tokens.push_back(Token::new(kind, s, e));
        Ok(())
    }

    fn fetch_flow_collection_end(&mut self, kind: Tk) -> R<()> {
        self.remove_possible_simple_key()?;
        // Python's flow_level may go negative on a stray ']' / '}' (the parser reports it); saturate here: a
        // negative level and level 0 behave the same in every check that follows.
        self.flow_level = self.flow_level.saturating_sub(1);
        self.allow_simple_key = false;
        let s = self.mark();
        self.forward(1);
        let e = self.mark();
        self.tokens.push_back(Token::new(kind, s, e));
        Ok(())
    }

    fn fetch_flow_entry(&mut self) -> R<()> {
        self.allow_simple_key = true;
        self.remove_possible_simple_key()?;
        let s = self.mark();
        self.forward(1);
        let e = self.mark();
        self.tokens.push_back(Token::new(Tk::FlowEntry, s, e));
        Ok(())
    }

    fn fetch_block_entry(&mut self) -> R<()> {
        if self.flow_level == 0 {
            if !self.allow_simple_key {
                return Err(self.scan_err(None, None, "sequence entries are not allowed here"));
            }
            #[allow(clippy::cast_possible_wrap)]
            if self.add_indent(self.column as isize) {
                let m = self.mark();
                self.tokens
                    .push_back(Token::new(Tk::BlockSequenceStart, m, m));
            }
        }
        self.allow_simple_key = true;
        self.remove_possible_simple_key()?;
        let s = self.mark();
        self.forward(1);
        let e = self.mark();
        self.tokens.push_back(Token::new(Tk::BlockEntry, s, e));
        Ok(())
    }

    fn fetch_key(&mut self) -> R<()> {
        if self.flow_level == 0 {
            if !self.allow_simple_key {
                return Err(self.scan_err(None, None, "mapping keys are not allowed here"));
            }
            #[allow(clippy::cast_possible_wrap)]
            if self.add_indent(self.column as isize) {
                let m = self.mark();
                self.tokens
                    .push_back(Token::new(Tk::BlockMappingStart, m, m));
            }
        }
        self.allow_simple_key = self.flow_level == 0;
        self.remove_possible_simple_key()?;
        let s = self.mark();
        self.forward(1);
        let e = self.mark();
        self.tokens.push_back(Token::new(Tk::Key, s, e));
        Ok(())
    }

    fn fetch_value(&mut self) -> R<()> {
        if let Some(key) = self.possible_simple_keys.shift_remove(&self.flow_level) {
            let at = key.token_number - self.tokens_taken;
            self.tokens
                .insert(at, Token::new(Tk::Key, key.mark, key.mark));
            if self.flow_level == 0 {
                #[allow(clippy::cast_possible_wrap)]
                if self.add_indent(key.mark.column as isize) {
                    self.tokens
                        .insert(at, Token::new(Tk::BlockMappingStart, key.mark, key.mark));
                }
            }
            self.allow_simple_key = false;
        } else {
            if self.flow_level == 0 && !self.allow_simple_key {
                return Err(self.scan_err(None, None, "mapping values are not allowed here"));
            }
            if self.flow_level == 0 {
                #[allow(clippy::cast_possible_wrap)]
                if self.add_indent(self.column as isize) {
                    let m = self.mark();
                    self.tokens
                        .push_back(Token::new(Tk::BlockMappingStart, m, m));
                }
            }
            self.allow_simple_key = self.flow_level == 0;
            self.remove_possible_simple_key()?;
        }
        let s = self.mark();
        self.forward(1);
        let e = self.mark();
        self.tokens.push_back(Token::new(Tk::Value, s, e));
        Ok(())
    }

    fn fetch_anchor(&mut self, kind: Tk) -> R<()> {
        self.save_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_anchor(kind)?;
        self.tokens.push_back(t);
        Ok(())
    }

    fn fetch_tag(&mut self) -> R<()> {
        self.save_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_tag()?;
        self.tokens.push_back(t);
        Ok(())
    }

    fn fetch_block_scalar(&mut self, style: char) -> R<()> {
        self.allow_simple_key = true;
        self.remove_possible_simple_key()?;
        let t = self.scan_block_scalar(style)?;
        self.tokens.push_back(t);
        Ok(())
    }

    fn fetch_flow_scalar(&mut self, style: char) -> R<()> {
        self.save_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_flow_scalar(style)?;
        self.tokens.push_back(t);
        Ok(())
    }

    fn fetch_plain(&mut self) -> R<()> {
        self.save_possible_simple_key()?;
        self.allow_simple_key = false;
        let t = self.scan_plain();
        self.tokens.push_back(t);
        Ok(())
    }

    fn check_plain(&self) -> bool {
        let ch = self.ch();
        !is_in(ch, "\0 \t\r\n\u{85}\u{2028}\u{2029}-?:,[]{}#&*!|>'\"%@`")
            || (!is_in(self.peek(1), BLANKZ_T)
                && (ch == '-' || (self.flow_level == 0 && is_in(ch, "?:"))))
    }

    // ── scanners ────────────────────────────────────────────────────────────────────────────

    fn scan_to_next_token(&mut self) {
        if self.ptr == 0 && self.ch() == '\u{feff}' {
            self.forward(1);
        }
        loop {
            while self.ch() == ' ' {
                self.forward(1);
            }
            if self.ch() == '#' {
                while !is_in(self.ch(), BREAKZ) {
                    self.forward(1);
                }
            }
            if self.scan_line_break().is_empty() {
                break;
            }
            if self.flow_level == 0 {
                self.allow_simple_key = true;
            }
        }
    }

    fn scan_directive(&mut self) -> R<Token> {
        let start = self.mark();
        self.forward(1);
        let name = self.scan_directive_name(start)?;
        let mut tok = Token::new(Tk::Directive, start, start);
        let end;
        if name == "YAML" {
            let (a, b) = self.scan_yaml_directive_value(start)?;
            tok.dir = DirVal::Yaml(a, b);
            end = self.mark();
        } else if name == "TAG" {
            let (h, p) = self.scan_tag_directive_value(start)?;
            tok.dir = DirVal::Tag(h, p);
            end = self.mark();
        } else {
            end = self.mark();
            while !is_in(self.ch(), BREAKZ) {
                self.forward(1);
            }
        }
        self.scan_directive_ignored_line(start)?;
        tok.value = name;
        tok.end = end;
        Ok(tok)
    }

    fn dir_err(&self, start: Mark, problem: &str) -> YamlError {
        self.scan_err(Some("while scanning a directive"), Some(start), problem)
    }

    fn scan_directive_name(&mut self, start: Mark) -> R<String> {
        let mut n = 0;
        while alnum_dash(self.peek(n)) {
            n += 1;
        }
        if n == 0 {
            return Err(self.dir_err(
                start,
                &format!(
                    "expected alphabetic or numeric character, but found {}",
                    str_repr(&self.peek(n).to_string())
                ),
            ));
        }
        let v = self.prefix(n);
        self.forward(n);
        let ch = self.ch();
        if !is_in(ch, BLANKZ) {
            return Err(self.dir_err(
                start,
                &format!(
                    "expected alphabetic or numeric character, but found {}",
                    str_repr(&ch.to_string())
                ),
            ));
        }
        Ok(v)
    }

    fn scan_yaml_directive_value(&mut self, start: Mark) -> R<(u128, u128)> {
        while self.ch() == ' ' {
            self.forward(1);
        }
        let major = self.scan_yaml_directive_number(start)?;
        if self.ch() != '.' {
            return Err(self.dir_err(
                start,
                &format!(
                    "expected a digit or '.', but found {}",
                    str_repr(&self.ch().to_string())
                ),
            ));
        }
        self.forward(1);
        let minor = self.scan_yaml_directive_number(start)?;
        if !is_in(self.ch(), BLANKZ) {
            return Err(self.dir_err(
                start,
                &format!(
                    "expected a digit or ' ', but found {}",
                    str_repr(&self.ch().to_string())
                ),
            ));
        }
        Ok((major, minor))
    }

    fn scan_yaml_directive_number(&mut self, start: Mark) -> R<u128> {
        let ch = self.ch();
        if !ch.is_ascii_digit() {
            return Err(self.dir_err(
                start,
                &format!("expected a digit, but found {}", str_repr(&ch.to_string())),
            ));
        }
        let mut n = 0;
        while self.peek(n).is_ascii_digit() {
            n += 1;
        }
        let v = self.prefix(n).parse::<u128>().unwrap_or(u128::MAX);
        self.forward(n);
        Ok(v)
    }

    fn scan_tag_directive_value(&mut self, start: Mark) -> R<(String, String)> {
        while self.ch() == ' ' {
            self.forward(1);
        }
        let handle = self.scan_tag_handle("directive", start)?;
        if self.ch() != ' ' {
            return Err(self.dir_err(
                start,
                &format!(
                    "expected ' ', but found {}",
                    str_repr(&self.ch().to_string())
                ),
            ));
        }
        while self.ch() == ' ' {
            self.forward(1);
        }
        let prefix = self.scan_tag_uri("directive", start)?;
        if !is_in(self.ch(), BLANKZ) {
            return Err(self.dir_err(
                start,
                &format!(
                    "expected ' ', but found {}",
                    str_repr(&self.ch().to_string())
                ),
            ));
        }
        Ok((handle, prefix))
    }

    fn scan_directive_ignored_line(&mut self, start: Mark) -> R<()> {
        while self.ch() == ' ' {
            self.forward(1);
        }
        if self.ch() == '#' {
            while !is_in(self.ch(), BREAKZ) {
                self.forward(1);
            }
        }
        let ch = self.ch();
        if !is_in(ch, BREAKZ) {
            return Err(self.dir_err(
                start,
                &format!(
                    "expected a comment or a line break, but found {}",
                    str_repr(&ch.to_string())
                ),
            ));
        }
        self.scan_line_break();
        Ok(())
    }

    fn scan_anchor(&mut self, kind: Tk) -> R<Token> {
        let start = self.mark();
        let name = if self.ch() == '*' { "alias" } else { "anchor" };
        self.forward(1);
        let mut n = 0;
        while alnum_dash(self.peek(n)) {
            n += 1;
        }
        let ctx = format!("while scanning an {name}");
        if n == 0 {
            return Err(self.scan_err(
                Some(&ctx),
                Some(start),
                &format!(
                    "expected alphabetic or numeric character, but found {}",
                    str_repr(&self.peek(n).to_string())
                ),
            ));
        }
        let value = self.prefix(n);
        self.forward(n);
        let ch = self.ch();
        if !is_in(ch, "\0 \t\r\n\u{85}\u{2028}\u{2029}?:,]}%@`") {
            return Err(self.scan_err(
                Some(&ctx),
                Some(start),
                &format!(
                    "expected alphabetic or numeric character, but found {}",
                    str_repr(&ch.to_string())
                ),
            ));
        }
        let mut t = Token::new(kind, start, self.mark());
        t.value = value;
        Ok(t)
    }

    fn scan_tag(&mut self) -> R<Token> {
        let start = self.mark();
        let mut ch = self.peek(1);
        let handle: Option<String>;
        let suffix: String;
        if ch == '<' {
            handle = None;
            self.forward(2);
            suffix = self.scan_tag_uri("tag", start)?;
            if self.ch() != '>' {
                return Err(self.scan_err(
                    Some("while parsing a tag"),
                    Some(start),
                    &format!(
                        "expected '>', but found {}",
                        str_repr(&self.ch().to_string())
                    ),
                ));
            }
            self.forward(1);
        } else if is_in(ch, BLANKZ_T) {
            handle = None;
            suffix = "!".to_owned();
            self.forward(1);
        } else {
            let mut n = 1;
            let mut use_handle = false;
            while !is_in(ch, BLANKZ) {
                if ch == '!' {
                    use_handle = true;
                    break;
                }
                n += 1;
                ch = self.peek(n);
            }
            if use_handle {
                handle = Some(self.scan_tag_handle("tag", start)?);
            } else {
                handle = Some("!".to_owned());
                self.forward(1);
            }
            suffix = self.scan_tag_uri("tag", start)?;
        }
        let ch = self.ch();
        if !is_in(ch, BLANKZ) {
            return Err(self.scan_err(
                Some("while scanning a tag"),
                Some(start),
                &format!("expected ' ', but found {}", str_repr(&ch.to_string())),
            ));
        }
        let mut t = Token::new(Tk::Tag, start, self.mark());
        t.tag = Some((handle, suffix));
        Ok(t)
    }

    fn scan_block_scalar(&mut self, style: char) -> R<Token> {
        let folded = style == '>';
        let mut chunks = String::new();
        let start = self.mark();
        self.forward(1);
        let (chomping, increment) = self.scan_block_scalar_indicators(start)?;
        self.scan_block_scalar_ignored_line(start)?;
        let min_indent = (self.indent + 1).max(1);
        let mut breaks: Vec<String>;
        let mut end;
        let indent: isize;
        if let Some(inc) = increment {
            indent = min_indent + inc - 1;
            let (b, e) = self.scan_block_scalar_breaks(indent);
            breaks = b;
            end = e;
        } else {
            let (b, max_indent, e) = self.scan_block_scalar_indentation();
            breaks = b;
            end = e;
            indent = min_indent.max(max_indent);
        }
        let mut line_break = String::new();
        #[allow(clippy::cast_possible_wrap)]
        while self.column as isize == indent && self.ch() != '\0' {
            for b in &breaks {
                chunks.push_str(b);
            }
            let leading_non_space = !is_in(self.ch(), " \t");
            let mut n = 0;
            while !is_in(self.peek(n), BREAKZ) {
                n += 1;
            }
            chunks.push_str(&self.prefix(n));
            self.forward(n);
            line_break = self.scan_line_break();
            let (b, e) = self.scan_block_scalar_breaks(indent);
            breaks = b;
            end = e;
            if self.column as isize == indent && self.ch() != '\0' {
                if folded && line_break == "\n" && leading_non_space && !is_in(self.ch(), " \t") {
                    if breaks.is_empty() {
                        chunks.push(' ');
                    }
                } else {
                    chunks.push_str(&line_break);
                }
            } else {
                break;
            }
        }
        if chomping != Some(false) {
            chunks.push_str(&line_break);
        }
        if chomping == Some(true) {
            for b in &breaks {
                chunks.push_str(b);
            }
        }
        let mut t = Token::new(Tk::Scalar, start, end);
        t.value = chunks;
        t.plain = false;
        t.style = Some(style);
        Ok(t)
    }

    fn scan_block_scalar_indicators(&mut self, start: Mark) -> R<(Option<bool>, Option<isize>)> {
        let ctx = Some("while scanning a block scalar");
        let mut chomping = None;
        let mut increment = None;
        let mut ch = self.ch();
        let zero = "expected indentation indicator in the range 1-9, but found 0";
        if is_in(ch, "+-") {
            chomping = Some(ch == '+');
            self.forward(1);
            ch = self.ch();
            if let Some(d) = ch.to_digit(10).filter(|_| ch.is_ascii_digit()) {
                if d == 0 {
                    return Err(self.scan_err(ctx, Some(start), zero));
                }
                increment = Some(isize::try_from(d).unwrap_or(1));
                self.forward(1);
            }
        } else if let Some(d) = ch.to_digit(10).filter(|_| ch.is_ascii_digit()) {
            if d == 0 {
                return Err(self.scan_err(ctx, Some(start), zero));
            }
            increment = Some(isize::try_from(d).unwrap_or(1));
            self.forward(1);
            ch = self.ch();
            if is_in(ch, "+-") {
                chomping = Some(ch == '+');
                self.forward(1);
            }
        }
        let ch = self.ch();
        if !is_in(ch, BLANKZ) {
            return Err(self.scan_err(
                ctx,
                Some(start),
                &format!(
                    "expected chomping or indentation indicators, but found {}",
                    str_repr(&ch.to_string())
                ),
            ));
        }
        Ok((chomping, increment))
    }

    fn scan_block_scalar_ignored_line(&mut self, start: Mark) -> R<()> {
        while self.ch() == ' ' {
            self.forward(1);
        }
        if self.ch() == '#' {
            while !is_in(self.ch(), BREAKZ) {
                self.forward(1);
            }
        }
        let ch = self.ch();
        if !is_in(ch, BREAKZ) {
            return Err(self.scan_err(
                Some("while scanning a block scalar"),
                Some(start),
                &format!(
                    "expected a comment or a line break, but found {}",
                    str_repr(&ch.to_string())
                ),
            ));
        }
        self.scan_line_break();
        Ok(())
    }

    fn scan_block_scalar_indentation(&mut self) -> (Vec<String>, isize, Mark) {
        let mut chunks = Vec::new();
        let mut max_indent: isize = 0;
        let mut end = self.mark();
        while is_in(self.ch(), " \r\n\u{85}\u{2028}\u{2029}") {
            if self.ch() == ' ' {
                self.forward(1);
                #[allow(clippy::cast_possible_wrap)]
                if self.column as isize > max_indent {
                    max_indent = self.column as isize;
                }
            } else {
                chunks.push(self.scan_line_break());
                end = self.mark();
            }
        }
        (chunks, max_indent, end)
    }

    #[allow(clippy::cast_possible_wrap)]
    fn scan_block_scalar_breaks(&mut self, indent: isize) -> (Vec<String>, Mark) {
        let mut chunks = Vec::new();
        let mut end = self.mark();
        while (self.column as isize) < indent && self.ch() == ' ' {
            self.forward(1);
        }
        while is_in(self.ch(), BREAKS) {
            chunks.push(self.scan_line_break());
            end = self.mark();
            while (self.column as isize) < indent && self.ch() == ' ' {
                self.forward(1);
            }
        }
        (chunks, end)
    }

    fn scan_flow_scalar(&mut self, style: char) -> R<Token> {
        let double = style == '"';
        let mut chunks = String::new();
        let start = self.mark();
        let quote = self.ch();
        self.forward(1);
        self.scan_flow_scalar_non_spaces(double, start, &mut chunks)?;
        while self.ch() != quote {
            self.scan_flow_scalar_spaces(start, &mut chunks)?;
            self.scan_flow_scalar_non_spaces(double, start, &mut chunks)?;
        }
        self.forward(1);
        let mut t = Token::new(Tk::Scalar, start, self.mark());
        t.value = chunks;
        t.plain = false;
        t.style = Some(style);
        Ok(t)
    }

    fn scan_flow_scalar_non_spaces(
        &mut self,
        double: bool,
        start: Mark,
        chunks: &mut String,
    ) -> R<()> {
        let ctx = Some("while scanning a double-quoted scalar");
        loop {
            let mut n = 0;
            while !is_in(self.peek(n), "'\"\\\0 \t\r\n\u{85}\u{2028}\u{2029}") {
                n += 1;
            }
            if n > 0 {
                chunks.push_str(&self.prefix(n));
                self.forward(n);
            }
            let ch = self.ch();
            if !double && ch == '\'' && self.peek(1) == '\'' {
                chunks.push('\'');
                self.forward(2);
            } else if (double && ch == '\'') || (!double && is_in(ch, "\"\\")) {
                chunks.push(ch);
                self.forward(1);
            } else if double && ch == '\\' {
                self.forward(1);
                let ch = self.ch();
                let rep = match ch {
                    '0' => Some('\0'),
                    'a' => Some('\u{07}'),
                    'b' => Some('\u{08}'),
                    't' | '\t' => Some('\t'),
                    'n' => Some('\n'),
                    'v' => Some('\u{0b}'),
                    'f' => Some('\u{0c}'),
                    'r' => Some('\r'),
                    'e' => Some('\u{1b}'),
                    ' ' => Some(' '),
                    '"' => Some('"'),
                    '\\' => Some('\\'),
                    '/' => Some('/'),
                    'N' => Some('\u{85}'),
                    '_' => Some('\u{a0}'),
                    'L' => Some('\u{2028}'),
                    'P' => Some('\u{2029}'),
                    _ => None,
                };
                if let Some(r) = rep {
                    chunks.push(r);
                    self.forward(1);
                } else if let Some(len) = match ch {
                    'x' => Some(2usize),
                    'u' => Some(4),
                    'U' => Some(8),
                    _ => None,
                } {
                    self.forward(1);
                    for k in 0..len {
                        if !self.peek(k).is_ascii_hexdigit() {
                            return Err(self.scan_err(
                                ctx,
                                Some(start),
                                &format!(
                                    "expected escape sequence of {len} hexadecimal numbers, but found {}",
                                    str_repr(&self.peek(k).to_string())
                                ),
                            ));
                        }
                    }
                    let code = u32::from_str_radix(&self.prefix(len), 16).unwrap_or(0);
                    match char::from_u32(code) {
                        Some(c) => chunks.push(c),
                        None if code > 0x10ffff => {
                            return Err(YamlError::Py(PyExc::new(
                                "ValueError",
                                "chr() arg not in range(0x110000)",
                            )));
                        }
                        // a lone surrogate: Rust strings cannot hold it (PORTING.md D-6)
                        None => chunks.push('\u{fffd}'),
                    }
                    self.forward(len);
                } else if is_in(ch, BREAKS) {
                    self.scan_line_break();
                    self.scan_flow_scalar_breaks(start, chunks)?;
                } else {
                    return Err(self.scan_err(
                        ctx,
                        Some(start),
                        &format!(
                            "found unknown escape character {}",
                            str_repr(&ch.to_string())
                        ),
                    ));
                }
            } else {
                return Ok(());
            }
        }
    }

    fn scan_flow_scalar_spaces(&mut self, start: Mark, chunks: &mut String) -> R<()> {
        let mut n = 0;
        while is_in(self.peek(n), " \t") {
            n += 1;
        }
        let ws = self.prefix(n);
        self.forward(n);
        let ch = self.ch();
        if ch == '\0' {
            return Err(self.scan_err(
                Some("while scanning a quoted scalar"),
                Some(start),
                "found unexpected end of stream",
            ));
        } else if is_in(ch, BREAKS) {
            let line_break = self.scan_line_break();
            let mut breaks = String::new();
            let nbreaks = self.scan_flow_scalar_breaks(start, &mut breaks)?;
            if line_break != "\n" {
                chunks.push_str(&line_break);
            } else if nbreaks == 0 {
                chunks.push(' ');
            }
            chunks.push_str(&breaks);
        } else {
            chunks.push_str(&ws);
        }
        Ok(())
    }

    /// Appends the breaks to `chunks`; returns how many there were.
    fn scan_flow_scalar_breaks(&mut self, start: Mark, chunks: &mut String) -> R<usize> {
        let mut count = 0;
        loop {
            let p = self.prefix(3);
            if (p == "---" || p == "...") && is_in(self.peek(3), BLANKZ_T) {
                return Err(self.scan_err(
                    Some("while scanning a quoted scalar"),
                    Some(start),
                    "found unexpected document separator",
                ));
            }
            while is_in(self.ch(), " \t") {
                self.forward(1);
            }
            if is_in(self.ch(), BREAKS) {
                let b = self.scan_line_break();
                chunks.push_str(&b);
                count += 1;
            } else {
                return Ok(count);
            }
        }
    }

    fn scan_plain(&mut self) -> Token {
        let mut chunks = String::new();
        let start = self.mark();
        let mut end = start;
        let indent = self.indent + 1;
        let mut spaces: Option<String> = Some(String::new());
        loop {
            let mut n = 0;
            if self.ch() == '#' {
                break;
            }
            loop {
                let ch = self.peek(n);
                let colon_end = ch == ':'
                    && (is_in(self.peek(n + 1), BLANKZ_T)
                        || (self.flow_level > 0 && is_in(self.peek(n + 1), ",[]{}")));
                if is_in(ch, BLANKZ_T) || colon_end || (self.flow_level > 0 && is_in(ch, ",?[]{}"))
                {
                    break;
                }
                n += 1;
            }
            if n == 0 {
                break;
            }
            self.allow_simple_key = false;
            if let Some(s) = &spaces {
                chunks.push_str(s);
            }
            chunks.push_str(&self.prefix(n));
            self.forward(n);
            end = self.mark();
            spaces = self.scan_plain_spaces();
            #[allow(clippy::cast_possible_wrap)]
            let dedent = self.flow_level == 0 && (self.column as isize) < indent;
            if spaces.as_ref().is_none_or(String::is_empty) || self.ch() == '#' || dedent {
                break;
            }
        }
        let mut t = Token::new(Tk::Scalar, start, end);
        t.value = chunks;
        t.plain = true;
        t
    }

    /// `None` = Python's bare `return` (a document separator follows); `Some("")` = no spaces.
    fn scan_plain_spaces(&mut self) -> Option<String> {
        let mut chunks = String::new();
        let mut n = 0;
        while self.peek(n) == ' ' {
            n += 1;
        }
        let ws = self.prefix(n);
        self.forward(n);
        let ch = self.ch();
        if is_in(ch, BREAKS) {
            let line_break = self.scan_line_break();
            self.allow_simple_key = true;
            let sep = |l: &Loader| {
                let p = l.prefix(3);
                (p == "---" || p == "...") && is_in(l.peek(3), BLANKZ_T)
            };
            if sep(self) {
                return None;
            }
            let mut breaks = Vec::new();
            while is_in(self.ch(), " \r\n\u{85}\u{2028}\u{2029}") {
                if self.ch() == ' ' {
                    self.forward(1);
                } else {
                    breaks.push(self.scan_line_break());
                    if sep(self) {
                        return None;
                    }
                }
            }
            if line_break != "\n" {
                chunks.push_str(&line_break);
            } else if breaks.is_empty() {
                chunks.push(' ');
            }
            for b in breaks {
                chunks.push_str(&b);
            }
        } else if !ws.is_empty() {
            chunks.push_str(&ws);
        }
        Some(chunks)
    }

    fn scan_tag_handle(&mut self, name: &str, start: Mark) -> R<String> {
        let ctx = format!("while scanning a {name}");
        let ch = self.ch();
        if ch != '!' {
            return Err(self.scan_err(
                Some(&ctx),
                Some(start),
                &format!("expected '!', but found {}", str_repr(&ch.to_string())),
            ));
        }
        let mut n = 1;
        let mut ch = self.peek(n);
        if ch != ' ' {
            while alnum_dash(ch) {
                n += 1;
                ch = self.peek(n);
            }
            if ch != '!' {
                self.forward(n);
                return Err(self.scan_err(
                    Some(&ctx),
                    Some(start),
                    &format!("expected '!', but found {}", str_repr(&ch.to_string())),
                ));
            }
            n += 1;
        }
        let v = self.prefix(n);
        self.forward(n);
        Ok(v)
    }

    fn scan_tag_uri(&mut self, name: &str, start: Mark) -> R<String> {
        let mut chunks = String::new();
        let mut n = 0;
        let mut ch = self.peek(n);
        while ch.is_ascii_alphanumeric() || is_in(ch, "-;/?:@&=+$,_.!~*'()[]%") {
            if ch == '%' {
                chunks.push_str(&self.prefix(n));
                self.forward(n);
                n = 0;
                let s = self.scan_uri_escapes(name, start)?;
                chunks.push_str(&s);
            } else {
                n += 1;
            }
            ch = self.peek(n);
        }
        if n > 0 {
            chunks.push_str(&self.prefix(n));
            self.forward(n);
        }
        if chunks.is_empty() {
            return Err(self.scan_err(
                Some(&format!("while parsing a {name}")),
                Some(start),
                &format!("expected URI, but found {}", str_repr(&ch.to_string())),
            ));
        }
        Ok(chunks)
    }

    fn scan_uri_escapes(&mut self, name: &str, start: Mark) -> R<String> {
        let mut codes = Vec::new();
        let mark = self.mark();
        while self.ch() == '%' {
            self.forward(1);
            for k in 0..2 {
                if !self.peek(k).is_ascii_hexdigit() {
                    return Err(self.scan_err(
                        Some(&format!("while scanning a {name}")),
                        Some(start),
                        &format!(
                            "expected URI escape sequence of 2 hexadecimal numbers, but found {}",
                            str_repr(&self.peek(k).to_string())
                        ),
                    ));
                }
            }
            codes.push(u8::from_str_radix(&self.prefix(2), 16).unwrap_or(0));
            self.forward(2);
        }
        match String::from_utf8(codes.clone()) {
            Ok(s) => Ok(s),
            Err(e) => {
                let msg = utf8_error_text(&codes, &e.utf8_error());
                Err(self.err(
                    "ScannerError",
                    Some(&format!("while scanning a {name}")),
                    Some(start),
                    &msg,
                    Some(mark),
                ))
            }
        }
    }

    fn scan_line_break(&mut self) -> String {
        let ch = self.ch();
        if is_in(ch, "\r\n\u{85}") {
            if self.prefix(2) == "\r\n" {
                self.forward(2);
            } else {
                self.forward(1);
            }
            return "\n".to_owned();
        }
        if is_in(ch, "\u{2028}\u{2029}") {
            self.forward(1);
            return ch.to_string();
        }
        String::new()
    }

    // ── parser ──────────────────────────────────────────────────────────────────────────────

    fn check_event(&mut self, kinds: &[Ek]) -> R<bool> {
        if self.current_event.is_none()
            && let Some(st) = self.state
        {
            self.current_event = Some(self.run_state(st)?);
        }
        Ok(self
            .current_event
            .as_ref()
            .is_some_and(|e| kinds.is_empty() || kinds.contains(&e.kind)))
    }

    fn peek_event(&mut self) -> R<Option<Event>> {
        if self.current_event.is_none()
            && let Some(st) = self.state
        {
            self.current_event = Some(self.run_state(st)?);
        }
        Ok(self.current_event.clone())
    }

    fn get_event(&mut self) -> R<Option<Event>> {
        if self.current_event.is_none()
            && let Some(st) = self.state
        {
            self.current_event = Some(self.run_state(st)?);
        }
        Ok(self.current_event.take())
    }

    fn pop_state(&mut self) {
        self.state = self.states.pop();
    }

    fn run_state(&mut self, st: PState) -> R<Event> {
        match st {
            PState::StreamStart => {
                let t = self.get_token()?;
                self.state = Some(PState::ImplicitDocumentStart);
                Ok(Event::new(Ek::StreamStart, t.start, t.end))
            }
            PState::ImplicitDocumentStart => {
                if self.check_token(&[Tk::Directive, Tk::DocumentStart, Tk::StreamEnd])? {
                    self.parse_document_start()
                } else {
                    self.tag_handles = default_tags();
                    let t = self.peek_token()?;
                    self.states.push(PState::DocumentEnd);
                    self.state = Some(PState::BlockNode);
                    Ok(Event::new(Ek::DocumentStart, t.start, t.start))
                }
            }
            PState::DocumentStart => self.parse_document_start(),
            PState::DocumentEnd => {
                let t = self.peek_token()?;
                let start = t.start;
                let mut end = t.start;
                if self.check_token(&[Tk::DocumentEnd])? {
                    end = self.get_token()?.end;
                }
                self.state = Some(PState::DocumentStart);
                Ok(Event::new(Ek::DocumentEnd, start, end))
            }
            PState::DocumentContent => {
                if self.check_token(&[
                    Tk::Directive,
                    Tk::DocumentStart,
                    Tk::DocumentEnd,
                    Tk::StreamEnd,
                ])? {
                    let m = self.peek_token()?.start;
                    self.pop_state();
                    Ok(empty_scalar(m))
                } else {
                    self.parse_node(true, false)
                }
            }
            PState::BlockNode => self.parse_node(true, false),
            PState::BlockSequenceFirstEntry => {
                let t = self.get_token()?;
                self.marks.push(t.start);
                self.parse_block_sequence_entry()
            }
            PState::BlockSequenceEntry => self.parse_block_sequence_entry(),
            PState::IndentlessSequenceEntry => {
                if self.check_token(&[Tk::BlockEntry])? {
                    let t = self.get_token()?;
                    if self.check_token(&[Tk::BlockEntry, Tk::Key, Tk::Value, Tk::BlockEnd])? {
                        self.state = Some(PState::IndentlessSequenceEntry);
                        Ok(empty_scalar(t.end))
                    } else {
                        self.states.push(PState::IndentlessSequenceEntry);
                        self.parse_node(true, false)
                    }
                } else {
                    let t = self.peek_token()?;
                    self.pop_state();
                    Ok(Event::new(Ek::SequenceEnd, t.start, t.start))
                }
            }
            PState::BlockMappingFirstKey => {
                let t = self.get_token()?;
                self.marks.push(t.start);
                self.parse_block_mapping_key()
            }
            PState::BlockMappingKey => self.parse_block_mapping_key(),
            PState::BlockMappingValue => {
                if self.check_token(&[Tk::Value])? {
                    let t = self.get_token()?;
                    if self.check_token(&[Tk::Key, Tk::Value, Tk::BlockEnd])? {
                        self.state = Some(PState::BlockMappingKey);
                        Ok(empty_scalar(t.end))
                    } else {
                        self.states.push(PState::BlockMappingKey);
                        self.parse_node(true, true)
                    }
                } else {
                    self.state = Some(PState::BlockMappingKey);
                    let t = self.peek_token()?;
                    Ok(empty_scalar(t.start))
                }
            }
            PState::FlowSequenceFirstEntry => {
                let t = self.get_token()?;
                self.marks.push(t.start);
                self.parse_flow_sequence_entry(true)
            }
            PState::FlowSequenceEntry => self.parse_flow_sequence_entry(false),
            PState::FlowSequenceEntryMappingKey => {
                let t = self.get_token()?;
                if self.check_token(&[Tk::Value, Tk::FlowEntry, Tk::FlowSequenceEnd])? {
                    self.state = Some(PState::FlowSequenceEntryMappingValue);
                    Ok(empty_scalar(t.end))
                } else {
                    self.states.push(PState::FlowSequenceEntryMappingValue);
                    self.parse_node(false, false)
                }
            }
            PState::FlowSequenceEntryMappingValue => {
                if self.check_token(&[Tk::Value])? {
                    let t = self.get_token()?;
                    if self.check_token(&[Tk::FlowEntry, Tk::FlowSequenceEnd])? {
                        self.state = Some(PState::FlowSequenceEntryMappingEnd);
                        Ok(empty_scalar(t.end))
                    } else {
                        self.states.push(PState::FlowSequenceEntryMappingEnd);
                        self.parse_node(false, false)
                    }
                } else {
                    self.state = Some(PState::FlowSequenceEntryMappingEnd);
                    let t = self.peek_token()?;
                    Ok(empty_scalar(t.start))
                }
            }
            PState::FlowSequenceEntryMappingEnd => {
                self.state = Some(PState::FlowSequenceEntry);
                let t = self.peek_token()?;
                Ok(Event::new(Ek::MappingEnd, t.start, t.start))
            }
            PState::FlowMappingFirstKey => {
                let t = self.get_token()?;
                self.marks.push(t.start);
                self.parse_flow_mapping_key(true)
            }
            PState::FlowMappingKey => self.parse_flow_mapping_key(false),
            PState::FlowMappingValue => {
                if self.check_token(&[Tk::Value])? {
                    let t = self.get_token()?;
                    if self.check_token(&[Tk::FlowEntry, Tk::FlowMappingEnd])? {
                        self.state = Some(PState::FlowMappingKey);
                        Ok(empty_scalar(t.end))
                    } else {
                        self.states.push(PState::FlowMappingKey);
                        self.parse_node(false, false)
                    }
                } else {
                    self.state = Some(PState::FlowMappingKey);
                    let t = self.peek_token()?;
                    Ok(empty_scalar(t.start))
                }
            }
            PState::FlowMappingEmptyValue => {
                self.state = Some(PState::FlowMappingKey);
                let m = self.peek_token()?.start;
                Ok(empty_scalar(m))
            }
        }
    }

    fn parse_document_start(&mut self) -> R<Event> {
        while self.check_token(&[Tk::DocumentEnd])? {
            self.get_token()?;
        }
        if self.check_token(&[Tk::StreamEnd])? {
            let t = self.get_token()?;
            self.state = None;
            return Ok(Event::new(Ek::StreamEnd, t.start, t.end));
        }
        let start = self.peek_token()?.start;
        self.process_directives()?;
        if !self.check_token(&[Tk::DocumentStart])? {
            let t = self.peek_token()?;
            return Err(self.err(
                "ParserError",
                None,
                None,
                &format!(
                    "expected '<document start>', but found {}",
                    str_repr(t.kind.id())
                ),
                Some(t.start),
            ));
        }
        let t = self.get_token()?;
        self.states.push(PState::DocumentEnd);
        self.state = Some(PState::DocumentContent);
        Ok(Event::new(Ek::DocumentStart, start, t.end))
    }

    fn process_directives(&mut self) -> R<()> {
        self.yaml_version = None;
        self.tag_handles = IndexMap::new();
        while self.check_token(&[Tk::Directive])? {
            let t = self.get_token()?;
            if t.value == "YAML" {
                if self.yaml_version.is_some() {
                    return Err(self.err(
                        "ParserError",
                        None,
                        None,
                        "found duplicate YAML directive",
                        Some(t.start),
                    ));
                }
                if let DirVal::Yaml(major, minor) = t.dir {
                    if major != 1 {
                        return Err(self.err(
                            "ParserError",
                            None,
                            None,
                            "found incompatible YAML document (version 1.* is required)",
                            Some(t.start),
                        ));
                    }
                    self.yaml_version = Some((major, minor));
                }
            } else if t.value == "TAG"
                && let DirVal::Tag(handle, prefix) = t.dir.clone()
            {
                if self.tag_handles.contains_key(&handle) {
                    return Err(self.err(
                        "ParserError",
                        None,
                        None,
                        &format!("duplicate tag handle {}", str_repr(&handle)),
                        Some(t.start),
                    ));
                }
                self.tag_handles.insert(handle, prefix);
            }
        }
        for (k, v) in default_tags() {
            self.tag_handles.entry(k).or_insert(v);
        }
        Ok(())
    }

    fn parse_node(&mut self, block: bool, indentless: bool) -> R<Event> {
        if self.check_token(&[Tk::Alias])? {
            let t = self.get_token()?;
            let mut ev = Event::new(Ek::Alias, t.start, t.end);
            ev.anchor = Some(t.value);
            self.pop_state();
            return Ok(ev);
        }
        let mut anchor = None;
        let mut tag: Option<(Option<String>, String)> = None;
        let (mut start, mut end, mut tag_mark) = (None, None, None);
        if self.check_token(&[Tk::Anchor])? {
            let t = self.get_token()?;
            start = Some(t.start);
            end = Some(t.end);
            anchor = Some(t.value);
            if self.check_token(&[Tk::Tag])? {
                let t = self.get_token()?;
                tag_mark = Some(t.start);
                end = Some(t.end);
                tag = t.tag;
            }
        } else if self.check_token(&[Tk::Tag])? {
            let t = self.get_token()?;
            start = Some(t.start);
            tag_mark = Some(t.start);
            end = Some(t.end);
            tag = t.tag;
            if self.check_token(&[Tk::Anchor])? {
                let t = self.get_token()?;
                end = Some(t.end);
                anchor = Some(t.value);
            }
        }
        let mut tag_s: Option<String> = None;
        if let Some((handle, suffix)) = tag {
            match handle {
                Some(h) => {
                    let Some(prefix) = self.tag_handles.get(&h).cloned() else {
                        return Err(self.err(
                            "ParserError",
                            Some("while parsing a node"),
                            start,
                            &format!("found undefined tag handle {}", str_repr(&h)),
                            tag_mark,
                        ));
                    };
                    tag_s = Some(prefix + &suffix);
                }
                None => tag_s = Some(suffix),
            }
        }
        let (start, end) = match start {
            Some(s) => (s, end.unwrap_or(s)),
            None => {
                let m = self.peek_token()?.start;
                (m, m)
            }
        };
        let implicit = tag_s.is_none() || tag_s.as_deref() == Some("!");
        let mut ev = Event::new(Ek::Scalar, start, end);
        ev.anchor.clone_from(&anchor);
        ev.tag.clone_from(&tag_s);
        if indentless && self.check_token(&[Tk::BlockEntry])? {
            ev.kind = Ek::SequenceStart;
            ev.end = self.peek_token()?.end;
            ev.implicit = (implicit, false);
            self.state = Some(PState::IndentlessSequenceEntry);
            return Ok(ev);
        }
        if self.check_token(&[Tk::Scalar])? {
            let t = self.get_token()?;
            ev.end = t.end;
            ev.implicit = if (t.plain && tag_s.is_none()) || tag_s.as_deref() == Some("!") {
                (true, false)
            } else if tag_s.is_none() {
                (false, true)
            } else {
                (false, false)
            };
            ev.value = t.value;
            self.pop_state();
            return Ok(ev);
        }
        ev.implicit = (implicit, false);
        if self.check_token(&[Tk::FlowSequenceStart])? {
            ev.kind = Ek::SequenceStart;
            ev.end = self.peek_token()?.end;
            self.state = Some(PState::FlowSequenceFirstEntry);
        } else if self.check_token(&[Tk::FlowMappingStart])? {
            ev.kind = Ek::MappingStart;
            ev.end = self.peek_token()?.end;
            self.state = Some(PState::FlowMappingFirstKey);
        } else if block && self.check_token(&[Tk::BlockSequenceStart])? {
            ev.kind = Ek::SequenceStart;
            ev.end = self.peek_token()?.start;
            self.state = Some(PState::BlockSequenceFirstEntry);
        } else if block && self.check_token(&[Tk::BlockMappingStart])? {
            ev.kind = Ek::MappingStart;
            ev.end = self.peek_token()?.start;
            self.state = Some(PState::BlockMappingFirstKey);
        } else if anchor.is_some() || tag_s.is_some() {
            ev.kind = Ek::Scalar;
            ev.implicit = (implicit, false);
            ev.value = String::new();
            self.pop_state();
        } else {
            let node = if block { "block" } else { "flow" };
            let t = self.peek_token()?;
            return Err(self.err(
                "ParserError",
                Some(&format!("while parsing a {node} node")),
                Some(start),
                &format!(
                    "expected the node content, but found {}",
                    str_repr(t.kind.id())
                ),
                Some(t.start),
            ));
        }
        Ok(ev)
    }

    fn parse_block_sequence_entry(&mut self) -> R<Event> {
        if self.check_token(&[Tk::BlockEntry])? {
            let t = self.get_token()?;
            if self.check_token(&[Tk::BlockEntry, Tk::BlockEnd])? {
                self.state = Some(PState::BlockSequenceEntry);
                return Ok(empty_scalar(t.end));
            }
            self.states.push(PState::BlockSequenceEntry);
            return self.parse_node(true, false);
        }
        if !self.check_token(&[Tk::BlockEnd])? {
            let t = self.peek_token()?;
            let cm = self.marks.last().copied();
            return Err(self.err(
                "ParserError",
                Some("while parsing a block collection"),
                cm,
                &format!("expected <block end>, but found {}", str_repr(t.kind.id())),
                Some(t.start),
            ));
        }
        let t = self.get_token()?;
        self.pop_state();
        self.marks.pop();
        Ok(Event::new(Ek::SequenceEnd, t.start, t.end))
    }

    fn parse_block_mapping_key(&mut self) -> R<Event> {
        if self.check_token(&[Tk::Key])? {
            let t = self.get_token()?;
            if self.check_token(&[Tk::Key, Tk::Value, Tk::BlockEnd])? {
                self.state = Some(PState::BlockMappingValue);
                return Ok(empty_scalar(t.end));
            }
            self.states.push(PState::BlockMappingValue);
            return self.parse_node(true, true);
        }
        if !self.check_token(&[Tk::BlockEnd])? {
            let t = self.peek_token()?;
            let cm = self.marks.last().copied();
            return Err(self.err(
                "ParserError",
                Some("while parsing a block mapping"),
                cm,
                &format!("expected <block end>, but found {}", str_repr(t.kind.id())),
                Some(t.start),
            ));
        }
        let t = self.get_token()?;
        self.pop_state();
        self.marks.pop();
        Ok(Event::new(Ek::MappingEnd, t.start, t.end))
    }

    fn parse_flow_sequence_entry(&mut self, first: bool) -> R<Event> {
        if !self.check_token(&[Tk::FlowSequenceEnd])? {
            if !first {
                if self.check_token(&[Tk::FlowEntry])? {
                    self.get_token()?;
                } else {
                    let t = self.peek_token()?;
                    let cm = self.marks.last().copied();
                    return Err(self.err(
                        "ParserError",
                        Some("while parsing a flow sequence"),
                        cm,
                        &format!("expected ',' or ']', but got {}", str_repr(t.kind.id())),
                        Some(t.start),
                    ));
                }
            }
            if self.check_token(&[Tk::Key])? {
                let t = self.peek_token()?;
                let mut ev = Event::new(Ek::MappingStart, t.start, t.end);
                ev.implicit = (true, false);
                self.state = Some(PState::FlowSequenceEntryMappingKey);
                return Ok(ev);
            } else if !self.check_token(&[Tk::FlowSequenceEnd])? {
                self.states.push(PState::FlowSequenceEntry);
                return self.parse_node(false, false);
            }
        }
        let t = self.get_token()?;
        self.pop_state();
        self.marks.pop();
        Ok(Event::new(Ek::SequenceEnd, t.start, t.end))
    }

    fn parse_flow_mapping_key(&mut self, first: bool) -> R<Event> {
        if !self.check_token(&[Tk::FlowMappingEnd])? {
            if !first {
                if self.check_token(&[Tk::FlowEntry])? {
                    self.get_token()?;
                } else {
                    let t = self.peek_token()?;
                    let cm = self.marks.last().copied();
                    return Err(self.err(
                        "ParserError",
                        Some("while parsing a flow mapping"),
                        cm,
                        &format!("expected ',' or '}}', but got {}", str_repr(t.kind.id())),
                        Some(t.start),
                    ));
                }
            }
            if self.check_token(&[Tk::Key])? {
                let t = self.get_token()?;
                if self.check_token(&[Tk::Value, Tk::FlowEntry, Tk::FlowMappingEnd])? {
                    self.state = Some(PState::FlowMappingValue);
                    return Ok(empty_scalar(t.end));
                }
                self.states.push(PState::FlowMappingValue);
                return self.parse_node(false, false);
            } else if !self.check_token(&[Tk::FlowMappingEnd])? {
                self.states.push(PState::FlowMappingEmptyValue);
                return self.parse_node(false, false);
            }
        }
        let t = self.get_token()?;
        self.pop_state();
        self.marks.pop();
        Ok(Event::new(Ek::MappingEnd, t.start, t.end))
    }

    // ── composer ────────────────────────────────────────────────────────────────────────────

    fn get_single_node(&mut self) -> R<Option<NodeId>> {
        self.get_event()?;
        let mut document = None;
        if !self.check_event(&[Ek::StreamEnd])? {
            document = Some(self.compose_document()?);
        }
        if !self.check_event(&[Ek::StreamEnd])? {
            let ev = self.get_event()?;
            let ds = document.map(|d| self.nodes[d].start);
            let es = ev.map(|e| e.start);
            return Err(self.err(
                "ComposerError",
                Some("expected a single document in the stream"),
                ds,
                "but found another document",
                es,
            ));
        }
        self.get_event()?;
        Ok(document)
    }

    fn compose_document(&mut self) -> R<NodeId> {
        self.get_event()?;
        let node = self.compose_node()?;
        self.get_event()?;
        self.anchors.clear();
        Ok(node)
    }

    /// `compose_node` without native recursion: an explicit stack of open collections (a deep document must end in
    /// Python's RecursionError, not in a stack overflow). Events are consumed in exactly the recursive order:
    /// a sequence checks for its end before each item, a mapping before each key (never between key and value).
    fn compose_node(&mut self) -> R<NodeId> {
        enum Open {
            Seq(NodeId),
            Map(NodeId, Option<NodeId>),
        }
        let mut stack: Vec<Open> = Vec::new();
        loop {
            // start one node (Python's compose_node body)
            let mut done = if self.check_event(&[Ek::Alias])? {
                let ev = self.get_event()?;
                let (anchor, start) = ev
                    .map(|e| (e.anchor.unwrap_or_default(), e.start))
                    .unwrap_or((String::new(), self.mark()));
                match self.anchors.get(&anchor) {
                    Some(&n) => Some(n),
                    None => {
                        return Err(self.err(
                            "ComposerError",
                            None,
                            None,
                            &format!("found undefined alias {}", str_repr(&anchor)),
                            Some(start),
                        ));
                    }
                }
            } else {
                let Some(ev) = self.peek_event()? else {
                    return Err(YamlError::Py(PyExc::new(
                        "AttributeError",
                        "'NoneType' object has no attribute 'anchor'",
                    )));
                };
                if let Some(a) = &ev.anchor
                    && let Some(&first) = self.anchors.get(a)
                {
                    let first_start = self.nodes[first].start;
                    return Err(self.err(
                        "ComposerError",
                        Some(&format!(
                            "found duplicate anchor {}; first occurrence",
                            str_repr(a)
                        )),
                        Some(first_start),
                        "second occurrence",
                        Some(ev.start),
                    ));
                }
                if stack.len() + 1 > MAX_DEPTH {
                    return Err(YamlError::Py(PyExc::recursion()));
                }
                match ev.kind {
                    Ek::Scalar => Some(self.compose_scalar_node(ev)?),
                    Ek::SequenceStart => {
                        let id = self.open_collection(ev, T_SEQ, NodeVal::Seq(Vec::new()))?;
                        stack.push(Open::Seq(id));
                        None
                    }
                    _ => {
                        let id = self.open_collection(ev, T_MAP, NodeVal::Map(Vec::new()))?;
                        stack.push(Open::Map(id, None));
                        None
                    }
                }
            };
            // attach finished nodes to their parents and close collections whose end event is next
            loop {
                if let Some(child) = done.take() {
                    match stack.last_mut() {
                        None => return Ok(child),
                        Some(Open::Seq(id)) => {
                            if let NodeVal::Seq(v) = &mut self.nodes[*id].val {
                                v.push(child);
                            }
                        }
                        Some(Open::Map(id, key)) => match key.take() {
                            None => *key = Some(child),
                            Some(k) => {
                                if let NodeVal::Map(items) = &mut self.nodes[*id].val {
                                    items.push((k, child));
                                }
                            }
                        },
                    }
                }
                let closed = match stack.last() {
                    None => {
                        return Err(YamlError::Py(PyExc::new(
                            "IndexError",
                            "pop from empty list",
                        )));
                    }
                    Some(Open::Seq(id)) => {
                        let id = *id;
                        if self.check_event(&[Ek::SequenceEnd])? {
                            Some(id)
                        } else {
                            None
                        }
                    }
                    Some(Open::Map(id, None)) => {
                        let id = *id;
                        if self.check_event(&[Ek::MappingEnd])? {
                            Some(id)
                        } else {
                            None
                        }
                    }
                    Some(Open::Map(_, Some(_))) => None,
                };
                let Some(id) = closed else { break };
                self.get_event()?;
                stack.pop();
                done = Some(id);
            }
        }
    }

    fn compose_scalar_node(&mut self, ev: Event) -> R<NodeId> {
        self.get_event()?;
        let tag = match ev.tag {
            Some(t) if t != "!" => t,
            _ => resolve_scalar(&ev.value, ev.implicit.0).to_owned(),
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            tag,
            val: NodeVal::Scalar(ev.value),
            start: ev.start,
        });
        if let Some(a) = ev.anchor {
            self.anchors.insert(a, id);
        }
        Ok(id)
    }

    /// `compose_sequence_node` / `compose_mapping_node` up to the children: consume the start event, create the node
    /// (resolving a non-specific tag) and register its anchor before any child is composed.
    fn open_collection(&mut self, ev: Event, default_tag: &str, val: NodeVal) -> R<NodeId> {
        self.get_event()?;
        let tag = match ev.tag {
            Some(t) if t != "!" => t,
            _ => default_tag.to_owned(),
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            tag,
            val,
            start: ev.start,
        });
        if let Some(a) = ev.anchor {
            self.anchors.insert(a, id);
        }
        Ok(id)
    }

    // ── constructor ─────────────────────────────────────────────────────────────────────────

    fn get_single_data(&mut self) -> R<Doc> {
        let node = self.get_single_node()?;
        let root = match node {
            Some(n) => self.construct_document(n)?,
            None => Py::None,
        };
        Ok(Doc {
            objs: std::mem::take(&mut self.objs),
            root,
        })
    }

    fn construct_document(&mut self, node: NodeId) -> R<Py> {
        let data = self.construct_object(node)?;
        while !self.gens.is_empty() {
            let gens = std::mem::take(&mut self.gens);
            for g in gens {
                self.run_gen(g)?;
            }
        }
        self.constructed.clear();
        self.recursive.clear();
        Ok(data)
    }

    fn cerr(&self, context: Option<&str>, cm: Option<Mark>, problem: &str, pm: Mark) -> YamlError {
        self.err("ConstructorError", context, cm, problem, Some(pm))
    }

    fn construct_object(&mut self, node: NodeId) -> R<Py> {
        if let Some(v) = self.constructed.get(&node) {
            return Ok(v.clone());
        }
        let start = self.nodes[node].start;
        if self.recursive.contains(&node) {
            return Err(self.cerr(None, None, "found unconstructable recursive node", start));
        }
        self.recursive.insert(node);
        let tag = self.nodes[node].tag.clone();
        let data = match tag.as_str() {
            T_NULL => {
                self.construct_scalar(node, 0)?;
                Py::None
            }
            T_BOOL => {
                let v = self.construct_scalar(node, 0)?.to_lowercase();
                match v.as_str() {
                    "yes" | "true" | "on" => Py::Bool(true),
                    "no" | "false" | "off" => Py::Bool(false),
                    _ => return Err(YamlError::Py(PyExc::new("KeyError", str_repr(&v)))),
                }
            }
            T_INT => Py::Int(self.construct_yaml_int(node)?),
            T_FLOAT => Py::Float(self.construct_yaml_float(node)?),
            T_BINARY => Py::Bytes(self.construct_yaml_binary(node)?),
            T_TIMESTAMP => self.construct_yaml_timestamp(node)?,
            T_STR => Py::Str(self.construct_scalar(node, 0)?),
            T_SEQ | T_OMAP | T_PAIRS => {
                let id = self.new_obj(Obj::List(Vec::new()));
                self.gens.push(match tag.as_str() {
                    T_SEQ => Gen::Seq(node, id),
                    T_OMAP => Gen::Omap(node, id),
                    _ => Gen::Pairs(node, id),
                });
                Py::List(id)
            }
            T_SET => {
                let id = self.new_obj(Obj::Set(Vec::new()));
                self.gens.push(Gen::Set(node, id));
                Py::Set(id)
            }
            T_MAP => {
                let id = self.new_obj(Obj::Dict(Vec::new()));
                self.gens.push(Gen::Map(node, id));
                Py::Dict(id)
            }
            _ => {
                return Err(self.cerr(
                    None,
                    None,
                    &format!(
                        "could not determine a constructor for the tag {}",
                        str_repr(&tag)
                    ),
                    start,
                ));
            }
        };
        self.constructed.insert(node, data.clone());
        self.recursive.remove(&node);
        Ok(data)
    }

    fn new_obj(&mut self, o: Obj) -> ObjId {
        self.objs.push(o);
        self.objs.len() - 1
    }

    fn run_gen(&mut self, g: Gen) -> R<()> {
        match g {
            Gen::Seq(node, id) => {
                let items = self.construct_sequence(node)?;
                if let Some(Obj::List(v)) = self.objs.get_mut(id) {
                    v.extend(items);
                }
            }
            Gen::Map(node, id) => {
                let items = self.construct_mapping(node)?;
                if let Some(Obj::Dict(v)) = self.objs.get_mut(id) {
                    v.extend(items);
                }
            }
            Gen::Set(node, id) => {
                let items = self.construct_mapping(node)?;
                if let Some(Obj::Set(v)) = self.objs.get_mut(id) {
                    v.extend(items.into_iter().map(|(k, _)| k));
                }
            }
            Gen::Omap(node, id) | Gen::Pairs(node, id) => {
                let ctx = if matches!(g, Gen::Omap(..)) {
                    "while constructing an ordered map"
                } else {
                    "while constructing pairs"
                };
                let n = self.nodes[node].clone();
                let NodeVal::Seq(subs) = &n.val else {
                    return Err(self.cerr(
                        Some(ctx),
                        Some(n.start),
                        &format!("expected a sequence, but found {}", n.id()),
                        n.start,
                    ));
                };
                for &sub in subs {
                    let sn = self.nodes[sub].clone();
                    let NodeVal::Map(pairs) = &sn.val else {
                        return Err(self.cerr(
                            Some(ctx),
                            Some(n.start),
                            &format!("expected a mapping of length 1, but found {}", sn.id()),
                            sn.start,
                        ));
                    };
                    if pairs.len() != 1 {
                        return Err(self.cerr(
                            Some(ctx),
                            Some(n.start),
                            &format!(
                                "expected a single mapping item, but found {} items",
                                pairs.len()
                            ),
                            sn.start,
                        ));
                    }
                    let (kn, vn) = pairs[0];
                    let k = self.construct_object(kn)?;
                    let v = self.construct_object(vn)?;
                    if let Some(Obj::List(list)) = self.objs.get_mut(id) {
                        list.push(Py::Tuple(vec![k, v]));
                    }
                }
            }
        }
        Ok(())
    }

    fn construct_scalar(&mut self, node: NodeId, depth: usize) -> R<String> {
        if depth > MAX_DEPTH {
            return Err(YamlError::Py(PyExc::recursion()));
        }
        if let NodeVal::Map(items) = &self.nodes[node].val {
            let items = items.clone();
            for (k, v) in items {
                if self.nodes[k].tag == T_VALUE {
                    return self.construct_scalar(v, depth + 1);
                }
            }
        }
        match &self.nodes[node].val {
            NodeVal::Scalar(s) => Ok(s.clone()),
            _ => {
                let n = &self.nodes[node];
                Err(self.cerr(
                    None,
                    None,
                    &format!("expected a scalar node, but found {}", n.id()),
                    n.start,
                ))
            }
        }
    }

    fn construct_sequence(&mut self, node: NodeId) -> R<Vec<Py>> {
        let n = self.nodes[node].clone();
        let NodeVal::Seq(children) = &n.val else {
            return Err(self.cerr(
                None,
                None,
                &format!("expected a sequence node, but found {}", n.id()),
                n.start,
            ));
        };
        let mut out = Vec::with_capacity(children.len());
        for &c in children {
            out.push(self.construct_object(c)?);
        }
        Ok(out)
    }

    fn flatten_mapping(&mut self, node: NodeId, depth: usize) -> R<()> {
        if depth > MAX_DEPTH {
            return Err(YamlError::Py(PyExc::recursion()));
        }
        let mut merge: Vec<(NodeId, NodeId)> = Vec::new();
        let mut index = 0;
        loop {
            let (k, v) = match &self.nodes[node].val {
                NodeVal::Map(items) if index < items.len() => items[index],
                _ => break,
            };
            if self.nodes[k].tag == T_MERGE {
                if let NodeVal::Map(items) = &mut self.nodes[node].val {
                    items.remove(index);
                }
                match self.nodes[v].val.clone() {
                    NodeVal::Map(_) => {
                        self.flatten_mapping(v, depth + 1)?;
                        if let NodeVal::Map(items) = &self.nodes[v].val {
                            merge.extend(items.iter().copied());
                        }
                    }
                    NodeVal::Seq(subs) => {
                        let mut submerge: Vec<Vec<(NodeId, NodeId)>> = Vec::new();
                        for sub in subs {
                            if !matches!(self.nodes[sub].val, NodeVal::Map(_)) {
                                let (ns, ss, sid) = (
                                    self.nodes[node].start,
                                    self.nodes[sub].start,
                                    self.nodes[sub].id(),
                                );
                                return Err(self.cerr(
                                    Some("while constructing a mapping"),
                                    Some(ns),
                                    &format!("expected a mapping for merging, but found {sid}"),
                                    ss,
                                ));
                            }
                            self.flatten_mapping(sub, depth + 1)?;
                            if let NodeVal::Map(items) = &self.nodes[sub].val {
                                submerge.push(items.clone());
                            }
                        }
                        submerge.reverse();
                        for value in submerge {
                            merge.extend(value);
                        }
                    }
                    NodeVal::Scalar(_) => {
                        let (ns, vs, vid) = (
                            self.nodes[node].start,
                            self.nodes[v].start,
                            self.nodes[v].id(),
                        );
                        return Err(self.cerr(
                            Some("while constructing a mapping"),
                            Some(ns),
                            &format!(
                                "expected a mapping or list of mappings for merging, but found {vid}"
                            ),
                            vs,
                        ));
                    }
                }
            } else if self.nodes[k].tag == T_VALUE {
                T_STR.clone_into(&mut self.nodes[k].tag);
                index += 1;
            } else {
                index += 1;
            }
        }
        if !merge.is_empty()
            && let NodeVal::Map(items) = &mut self.nodes[node].val
        {
            merge.append(items);
            *items = merge;
        }
        Ok(())
    }

    fn construct_mapping(&mut self, node: NodeId) -> R<Vec<(Py, Py)>> {
        if matches!(self.nodes[node].val, NodeVal::Map(_)) {
            self.flatten_mapping(node, 0)?;
        }
        let n = self.nodes[node].clone();
        let NodeVal::Map(items) = &n.val else {
            return Err(self.cerr(
                None,
                None,
                &format!("expected a mapping node, but found {}", n.id()),
                n.start,
            ));
        };
        let mut out: Vec<(Py, Py)> = Vec::new();
        let mut index: HashMap<u64, Vec<usize>> = HashMap::new();
        for &(kn, vn) in items {
            let key = self.construct_object(kn)?;
            if !Doc::hashable(&key) {
                let ks = self.nodes[kn].start;
                return Err(self.cerr(
                    Some("while constructing a mapping"),
                    Some(n.start),
                    "found unhashable key",
                    ks,
                ));
            }
            let value = self.construct_object(vn)?;
            let doc = Doc {
                objs: std::mem::take(&mut self.objs),
                root: Py::None,
            };
            let h = doc.key_hash(&key);
            let slot = index
                .get(&h)
                .and_then(|is| is.iter().copied().find(|&i| doc.eq(&out[i].0, &key)));
            self.objs = doc.objs;
            match slot {
                Some(i) => out[i].1 = value,
                None => {
                    index.entry(h).or_default().push(out.len());
                    out.push((key, value));
                }
            }
        }
        Ok(out)
    }

    fn construct_yaml_int(&mut self, node: NodeId) -> R<PyInt> {
        let raw = self.construct_scalar(node, 0)?;
        let value: String = raw.chars().filter(|c| *c != '_').collect();
        let index_error = || YamlError::Py(PyExc::new("IndexError", "string index out of range"));
        let first = value.chars().next().ok_or_else(index_error)?;
        let neg = first == '-';
        let v: &str = if first == '-' || first == '+' {
            &value[1..]
        } else {
            &value
        };
        let apply = |i: PyInt| -> PyInt { if neg { negate(i) } else { i } };
        if v == "0" {
            return Ok(PyInt::Small(0));
        }
        if let Some(rest) = v.strip_prefix("0b") {
            return Ok(apply(py_int(rest, 2)?));
        }
        if let Some(rest) = v.strip_prefix("0x") {
            return Ok(apply(py_int(rest, 16)?));
        }
        let first_v = v.chars().next().ok_or_else(index_error)?;
        if first_v == '0' {
            return Ok(apply(py_int(v, 8)?));
        }
        if v.contains(':') {
            let mut parts = Vec::new();
            for p in v.split(':') {
                parts.push(py_int(p, 10)?);
            }
            parts.reverse();
            return Ok(apply(sexagesimal(&parts)?));
        }
        Ok(apply(py_int(v, 10)?))
    }

    fn construct_yaml_float(&mut self, node: NodeId) -> R<f64> {
        let raw = self.construct_scalar(node, 0)?;
        let value: String = raw
            .chars()
            .filter(|c| *c != '_')
            .collect::<String>()
            .to_lowercase();
        let Some(first) = value.chars().next() else {
            return Err(PyExc::new("IndexError", "string index out of range").into());
        };
        let sign = if first == '-' { -1.0 } else { 1.0 };
        let v: &str = if first == '-' || first == '+' {
            &value[1..]
        } else {
            &value
        };
        if v == ".inf" {
            return Ok(sign * f64::INFINITY);
        }
        if v == ".nan" {
            return Ok(canonical_nan());
        }
        if v.contains(':') {
            let mut digits = Vec::new();
            for p in v.split(':') {
                digits.push(py_float(p)?);
            }
            digits.reverse();
            let mut base = BigUint::zero();
            base.mul_add(1, 1);
            let mut total = 0.0f64;
            for d in digits {
                total += d * base.to_f64();
                base = base.times(60);
            }
            return Ok(sign * total);
        }
        Ok(sign * py_float(v)?)
    }

    fn construct_yaml_binary(&mut self, node: NodeId) -> R<Vec<u8>> {
        let s = self.construct_scalar(node, 0)?;
        let start = self.nodes[node].start;
        if let Some(msg) = ascii_encode_error(&s) {
            return Err(self.cerr(
                None,
                None,
                &format!("failed to convert base64 data into ascii: {msg}"),
                start,
            ));
        }
        a2b_base64(s.as_bytes()).map_err(|m| {
            self.cerr(
                None,
                None,
                &format!("failed to decode base64 data: {m}"),
                start,
            )
        })
    }

    fn construct_yaml_timestamp(&mut self, node: NodeId) -> R<Py> {
        self.construct_scalar(node, 0)?;
        // a mapping with a `=` key passes construct_scalar; its node.value is then a list of pairs
        let NodeVal::Scalar(value) = &self.nodes[node].val else {
            return Err(PyExc::new(
                "TypeError",
                "expected string or bytes-like object, got 'list'",
            )
            .into());
        };
        let Some(c) = py_dollar_captures(&TIMESTAMP_RE, value) else {
            return Err(PyExc::new(
                "AttributeError",
                "'NoneType' object has no attribute 'groupdict'",
            )
            .into());
        };
        let num =
            |name: &str| -> Option<u32> { c.name(name).and_then(|m| m.as_str().parse().ok()) };
        let year = c
            .name("year")
            .and_then(|m| m.as_str().parse::<i32>().ok())
            .unwrap_or(0);
        let month = num("month").unwrap_or(0);
        let day = num("day").unwrap_or(0);
        let date = Date { year, month, day };
        let hour_s = c.name("hour").map(|m| m.as_str()).unwrap_or("");
        if hour_s.is_empty() {
            check_date(&date)?;
            return Ok(Py::Date(date));
        }
        let hour = num("hour").unwrap_or(0);
        let minute = num("minute").unwrap_or(0);
        let second = num("second").unwrap_or(0);
        let mut micro = 0;
        if let Some(f) = c
            .name("fraction")
            .map(|m| m.as_str())
            .filter(|f| !f.is_empty())
        {
            let mut f: String = f.chars().take(6).collect();
            while f.len() < 6 {
                f.push('0');
            }
            micro = f.parse().unwrap_or(0);
        }
        let mut tz = None;
        if let Some(sign) = c.name("tz_sign").map(|m| m.as_str()) {
            let th = i64::from(num("tz_hour").unwrap_or(0));
            let tm = i64::from(num("tz_minute").unwrap_or(0));
            let mut delta = th * 3600 + tm * 60;
            if sign == "-" {
                delta = -delta;
            }
            if delta <= -86_400 || delta >= 86_400 {
                return Err(PyExc::new(
                    "ValueError",
                    format!(
                        "offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24), not {}.",
                        timedelta_repr(delta)
                    ),
                )
                .into());
            }
            tz = Some(i32::try_from(delta).unwrap_or(0));
        } else if c.name("tz").is_some() {
            tz = Some(0);
        }
        check_date(&date)?;
        if hour > 23 {
            return Err(PyExc::new("ValueError", "hour must be in 0..23").into());
        }
        if minute > 59 {
            return Err(PyExc::new("ValueError", "minute must be in 0..59").into());
        }
        if second > 59 {
            return Err(PyExc::new("ValueError", "second must be in 0..59").into());
        }
        Ok(Py::DateTime(DateTime {
            date,
            hour,
            minute,
            second,
            micro,
            tz,
        }))
    }
}

/// PyYAML's sexagesimal int: `value += digit*base; base *= 60` over the reversed parts (parts may be negative or
/// huge, as Python's int() allows them).
fn sexagesimal(parts: &[PyInt]) -> R<PyInt> {
    let mut total: Option<i128> = Some(0);
    let mut base: Option<i128> = Some(1);
    for d in parts {
        total = match (total, base, d) {
            (Some(t), Some(b), PyInt::Small(x)) => x.checked_mul(b).and_then(|p| t.checked_add(p)),
            _ => None,
        };
        base = base.and_then(|b| b.checked_mul(60));
    }
    if let Some(t) = total {
        return Ok(PyInt::Small(t));
    }
    // beyond i128: only non-negative parts are supported (PORTING.md workstream C deviations)
    let mut big = BigUint::zero();
    let mut bbase = BigUint::zero();
    bbase.mul_add(1, 1);
    for d in parts {
        let digits = match d {
            PyInt::Small(x) if *x >= 0 => x.to_string(),
            PyInt::Big(s) if !s.starts_with('-') => s.clone(),
            _ => {
                return Err(PyExc::new(
                    "OverflowError",
                    "negative sexagesimal part beyond 128 bits",
                )
                .into());
            }
        };
        let mut term = bbase.clone();
        term.mul_decimal(&digits);
        big.add(&term);
        bbase = bbase.times(60);
    }
    Ok(big.into_int(false))
}

fn negate(i: PyInt) -> PyInt {
    match i {
        PyInt::Small(v) => PyInt::Small(-v),
        PyInt::Big(s) => match s.strip_prefix('-') {
            Some(rest) => PyInt::Big(rest.to_owned()),
            None => PyInt::Big(format!("-{s}")),
        },
    }
}

fn check_date(d: &Date) -> R<()> {
    if !(1..=9999).contains(&d.year) {
        return Err(PyExc::new("ValueError", format!("year {} is out of range", d.year)).into());
    }
    if !(1..=12).contains(&d.month) {
        return Err(PyExc::new("ValueError", "month must be in 1..12").into());
    }
    let leap = (d.year % 4 == 0 && d.year % 100 != 0) || d.year % 400 == 0;
    let dim = match d.month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if d.day < 1 || d.day > dim {
        return Err(PyExc::new("ValueError", "day is out of range for month").into());
    }
    Ok(())
}

fn default_tags() -> IndexMap<String, String> {
    let mut m = IndexMap::new();
    m.insert("!".to_owned(), "!".to_owned());
    m.insert("!!".to_owned(), "tag:yaml.org,2002:".to_owned());
    m
}

fn empty_scalar(m: Mark) -> Event {
    let mut ev = Event::new(Ek::Scalar, m, m);
    ev.implicit = (true, false);
    ev
}

// ── resolver ──────────────────────────────────────────────────────────────────────────────────────

#[allow(clippy::expect_used)]
fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

static BOOL_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\A(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF)\z")
});
static FLOAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\A(?:[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?|\.[0-9][0-9_]*(?:[eE][-+][0-9]+)?|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*|[-+]?\.(?:inf|Inf|INF)|\.(?:nan|NaN|NAN))\z",
    )
});
static INT_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\A(?:[-+]?0b[0-1_]+|[-+]?0[0-7_]+|[-+]?(?:0|[1-9][0-9_]*)|[-+]?0x[0-9a-fA-F_]+|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)\z",
    )
});
static MERGE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\A(?:<<)\z"));
static NULL_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\A(?:~|null|Null|NULL|)\z"));
static TS_RESOLVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\A(?:[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]|[0-9][0-9][0-9][0-9]-[0-9][0-9]?-[0-9][0-9]?(?:[Tt]|[ \t]+)[0-9][0-9]?:[0-9][0-9]:[0-9][0-9](?:\.[0-9]*)?(?:[ \t]*(?:Z|[-+][0-9][0-9]?(?::[0-9][0-9])?))?)\z",
    )
});
static VALUE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\A(?:=)\z"));
static YAML_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\A(?:!|&|\*)\z"));
static TIMESTAMP_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\A(?P<year>[0-9][0-9][0-9][0-9])-(?P<month>[0-9][0-9]?)-(?P<day>[0-9][0-9]?)(?:(?:[Tt]|[ \t]+)(?P<hour>[0-9][0-9]?):(?P<minute>[0-9][0-9]):(?P<second>[0-9][0-9])(?:\.(?P<fraction>[0-9]*))?(?:[ \t]*(?P<tz>Z|(?P<tz_sign>[-+])(?P<tz_hour>[0-9][0-9]?)(?::(?P<tz_minute>[0-9][0-9]))?))?)?\z",
    )
});

/// Python `re.match` of a pattern ending in `$` (non-MULTILINE): the end may also sit before ONE final `\n`.
fn py_dollar_match(re: &Regex, s: &str) -> bool {
    re.is_match(s) || s.strip_suffix('\n').is_some_and(|t| re.is_match(t))
}

fn py_dollar_captures<'a>(re: &Regex, s: &'a str) -> Option<regex::Captures<'a>> {
    re.captures(s)
        .or_else(|| s.strip_suffix('\n').and_then(|t| re.captures(t)))
}

/// `Resolver.resolve(ScalarNode, value, implicit)` (no path resolvers).
fn resolve_scalar(value: &str, implicit: bool) -> &'static str {
    if implicit {
        let first = value.chars().next();
        let candidates: &[(&'static str, &LazyLock<Regex>)] = match first {
            None => &[("tag:yaml.org,2002:null", &NULL_RE)],
            Some('y' | 'Y' | 't' | 'T' | 'f' | 'F' | 'o' | 'O') => &[(T_BOOL, &BOOL_RE)],
            Some('n' | 'N') => &[(T_BOOL, &BOOL_RE), (T_NULL, &NULL_RE)],
            Some('-' | '+') => &[(T_FLOAT, &FLOAT_RE), (T_INT, &INT_RE)],
            Some('.') => &[(T_FLOAT, &FLOAT_RE)],
            Some('0'..='9') => &[
                (T_FLOAT, &FLOAT_RE),
                (T_INT, &INT_RE),
                (T_TIMESTAMP, &TS_RESOLVE_RE),
            ],
            Some('<') => &[(T_MERGE, &MERGE_RE)],
            Some('~') => &[(T_NULL, &NULL_RE)],
            Some('=') => &[(T_VALUE, &VALUE_RE)],
            Some('!' | '&' | '*') => &[("tag:yaml.org,2002:yaml", &YAML_RE)],
            _ => &[],
        };
        for (tag, r) in candidates {
            if py_dollar_match(r, value) {
                return tag;
            }
        }
    }
    T_STR
}

// ── codec helpers ─────────────────────────────────────────────────────────────────────────────────

/// `str(UnicodeDecodeError)` of `bytes.decode('utf-8')`.
fn utf8_error_text(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let start = e.valid_up_to();
    let (end, reason) = match e.error_len() {
        None => (bytes.len(), "unexpected end of data"),
        Some(n) => {
            let b0 = bytes[start];
            let reason = if (0x80..0xc2).contains(&b0) || b0 > 0xf4 {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            (start + n, reason)
        }
    };
    if end - start <= 1 {
        format!(
            "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            bytes[start]
        )
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
            end - 1
        )
    }
}

/// `str(UnicodeEncodeError)` of `s.encode('ascii')`, or `None` when it encodes.
fn ascii_encode_error(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.iter().position(|c| !c.is_ascii())?;
    let mut end = start;
    while end < chars.len() && !chars[end].is_ascii() {
        end += 1;
    }
    if end - start == 1 {
        let v = u32::from(chars[start]);
        let esc = if v < 0x100 {
            format!("\\x{v:02x}")
        } else if v < 0x10000 {
            format!("\\u{v:04x}")
        } else {
            format!("\\U{v:08x}")
        };
        Some(format!(
            "'ascii' codec can't encode character '{esc}' in position {start}: ordinal not in range(128)"
        ))
    } else {
        Some(format!(
            "'ascii' codec can't encode characters in position {start}-{}: ordinal not in range(128)",
            end - 1
        ))
    }
}

/// `binascii.a2b_base64(data)` (non-strict mode: characters outside the alphabet are skipped).
fn a2b_base64(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut quad_pos = 0;
    let mut pads = 0;
    let mut leftchar: u32 = 0;
    for &c in data {
        if c == b'=' {
            pads += 1;
            if quad_pos >= 2 && quad_pos + pads >= 4 {
                return Ok(out);
            }
            continue;
        }
        let v = match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        pads = 0;
        #[allow(clippy::cast_possible_truncation)]
        match quad_pos {
            0 => {
                quad_pos = 1;
                leftchar = v;
            }
            1 => {
                quad_pos = 2;
                out.push(((leftchar << 2) | (v >> 4)) as u8);
                leftchar = v & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push(((leftchar << 4) | (v >> 2)) as u8);
                leftchar = v & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push(((leftchar << 6) | v) as u8);
                leftchar = 0;
            }
        }
    }
    match quad_pos {
        0 => Ok(out),
        1 => Err(format!(
            "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
            out.len() / 3 * 4 + 1
        )),
        _ => Err("Incorrect padding".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(text: &str) -> (Option<String>, Option<usize>) {
        match safe_load(text) {
            Ok(_) => (None, None),
            Err(e) => (
                e.problem().map(str::to_owned),
                e.problem_mark().map(|m| m.line),
            ),
        }
    }

    #[test]
    fn errors_match_pyyaml_texts() {
        assert_eq!(
            problem("version: 1\nsuites: [unclosed\n"),
            (
                Some("expected ',' or ']', but got '<stream end>'".to_owned()),
                Some(2)
            )
        );
        assert_eq!(
            problem("suites: [\n"),
            (
                Some("expected the node content, but found '<stream end>'".to_owned()),
                Some(1)
            )
        );
        let e = safe_load("a: b: c").err();
        assert_eq!(
            e.map(|e| e.text()).as_deref(),
            Some(
                "mapping values are not allowed here\n  in \"<unicode string>\", line 1, column 5:\n    a: b: c\n        ^"
            )
        );
    }

    #[test]
    fn values_like_pyyaml() {
        let d =
            safe_load("a: 1\nb: 1.5\nc: yes\nd: ~\ne: 2026-10-23\nf: 0x1F\ng: 1:30\nh: [x, 'y']\n")
                .ok();
        let d = d.as_ref();
        let items = d.and_then(|d| d.dict(&d.root));
        let get = |k: &str| items.and_then(|i| Doc::get(i, k)).cloned();
        assert_eq!(get("a"), Some(Py::Int(PyInt::Small(1))));
        assert_eq!(get("b"), Some(Py::Float(1.5)));
        assert_eq!(get("c"), Some(Py::Bool(true)));
        assert_eq!(get("d"), Some(Py::None));
        assert_eq!(get("f"), Some(Py::Int(PyInt::Small(31))));
        assert_eq!(get("g"), Some(Py::Int(PyInt::Small(90))));
        assert!(matches!(get("e"), Some(Py::Date(_))));
    }
}
