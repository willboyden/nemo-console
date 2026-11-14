//! `services::pytoml` (core, stable API: PORTING.md A-6): a hand port of CPython 3.13's `tomllib`
//! (`Lib/tomllib/_parser.py` + `_re.py`): TOML 1.0, exactly what the Python adapters accept and reject, with the same
//! `TOMLDecodeError` texts ("... (at line L, column C)"). The `toml` crate parses TOML 1.1 and words its errors
//! differently (PORTING.md risk R-3), so every `tomllib.loads` of the backend (Switchyard, Relay, OpenShell and the
//! core's `capable_models`) goes through this module. Positions and columns count code points, like Python `str`
//! indices. Written by workstream A (adapters/switchyard/tomllib.rs), moved into core at integration at the workstream A merge; pinned by
//! tests/switchyard_tomllib.rs (tests/fixtures/switchyard/tomllib_golden.json, from CPython 3.13).
//!
//! Values mirror what `tomllib.loads` returns (dict / list / str / int / float / bool / datetime / date / time), with
//! Python's `repr()` and `str()` renderings for the messages that embed them.

use indexmap::IndexMap;

use crate::services::pyfmt::{float_repr, str_repr};

/// A parsed TOML value (Python object returned by `tomllib`).
#[derive(Debug, Clone, PartialEq)]
pub enum TomlVal {
    Str(String),
    /// Python `int` (unbounded); values outside i128 keep their decimal text.
    Int(i128),
    BigInt(String),
    Float(f64),
    Bool(bool),
    /// `datetime.date`
    Date(Date),
    /// `datetime.time`
    Time(Time),
    /// `datetime.datetime`; `tz` = offset in minutes (None = naive / local date-time)
    DateTime(Date, Time, Option<i32>),
    Array(Vec<TomlVal>),
    Table(Table),
}

pub type Table = IndexMap<String, TomlVal>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub micro: u32,
}

/// `tomllib.TOMLDecodeError` (its `str()` is the message).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TomlDecodeError(pub String);

impl TomlDecodeError {
    /// `type(e).__name__`. `tomllib` raises TOMLDecodeError with an "(at line L, column C)" / "(at end of document)"
    /// suffix; the other exceptions it lets escape (RecursionError for a key over 1,000 parts or nesting past the
    /// frame budget, ValueError for a decimal int over 4,300 digits) carry Python's own texts (A-6, A-7).
    #[must_use]
    pub fn class(&self) -> &'static str {
        if self.0.ends_with(" (at end of document)")
            || (self.0.ends_with(')') && self.0.contains(" (at line "))
        {
            "TOMLDecodeError"
        } else if self.0.starts_with("Exceeds the limit (") {
            "ValueError"
        } else {
            "RecursionError"
        }
    }

    /// `isinstance(e, tomllib.TOMLDecodeError)`: what `except tomllib.TOMLDecodeError` catches.
    #[must_use]
    pub fn is_decode(&self) -> bool {
        self.class() == "TOMLDecodeError"
    }
}

impl TomlVal {
    /// `v.get(key)` when `v` is a table (Python's `_dict(v).get(key)`), else None.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&TomlVal> {
        self.as_table().and_then(|t| t.get(key))
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            TomlVal::Str(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            TomlVal::Table(t) => Some(t),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_array(&self) -> Option<&Vec<TomlVal>> {
        match self {
            TomlVal::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Python `repr(value)`.
    #[must_use]
    pub fn repr(&self) -> String {
        match self {
            TomlVal::Str(s) => str_repr(s),
            TomlVal::Int(i) => i.to_string(),
            TomlVal::BigInt(s) => s.clone(),
            TomlVal::Float(f) => float_repr(*f),
            TomlVal::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
            TomlVal::Date(d) => format!("datetime.date({}, {}, {})", d.year, d.month, d.day),
            TomlVal::Time(t) => format!("datetime.time({})", time_args(t)),
            TomlVal::DateTime(d, t, tz) => {
                let mut s = format!(
                    "datetime.datetime({}, {}, {}, {}",
                    d.year,
                    d.month,
                    d.day,
                    time_args(t)
                );
                if let Some(off) = tz {
                    s.push_str(", tzinfo=");
                    s.push_str(&tz_repr(*off));
                }
                s.push(')');
                s
            }
            TomlVal::Array(a) => {
                let inner: Vec<String> = a.iter().map(TomlVal::repr).collect();
                format!("[{}]", inner.join(", "))
            }
            TomlVal::Table(t) => {
                let inner: Vec<String> = t
                    .iter()
                    .map(|(k, v)| format!("{}: {}", str_repr(k), v.repr()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }

    /// Python `str(value)`.
    #[must_use]
    pub fn py_str(&self) -> String {
        match self {
            TomlVal::Str(s) => s.clone(),
            TomlVal::Date(d) => date_iso(d),
            TomlVal::Time(t) => time_iso(t),
            TomlVal::DateTime(d, t, tz) => {
                let mut s = format!("{} {}", date_iso(d), time_iso(t));
                if let Some(off) = tz {
                    s.push_str(&offset_iso(*off));
                }
                s
            }
            other => other.repr(),
        }
    }
}

fn time_args(t: &Time) -> String {
    let mut s = format!("{}, {}", t.hour, t.minute);
    if t.second != 0 || t.micro != 0 {
        s.push_str(&format!(", {}", t.second));
    }
    if t.micro != 0 {
        s.push_str(&format!(", {}", t.micro));
    }
    s
}

fn tz_repr(off_min: i32) -> String {
    if off_min == 0 {
        return "datetime.timezone.utc".to_owned();
    }
    let secs = i64::from(off_min) * 60;
    let td = if secs < 0 {
        let days = secs.div_euclid(86_400);
        let rest = secs.rem_euclid(86_400);
        format!("datetime.timedelta(days={days}, seconds={rest})")
    } else {
        format!("datetime.timedelta(seconds={secs})")
    };
    format!("datetime.timezone({td})")
}

fn date_iso(d: &Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

fn time_iso(t: &Time) -> String {
    if t.micro != 0 {
        format!(
            "{:02}:{:02}:{:02}.{:06}",
            t.hour, t.minute, t.second, t.micro
        )
    } else {
        format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second)
    }
}

fn offset_iso(off_min: i32) -> String {
    let sign = if off_min < 0 { '-' } else { '+' };
    let a = off_min.unsigned_abs();
    format!("{sign}{:02}:{:02}", a / 60, a % 60)
}

// ── the parser ──────────────────────────────────────────────────────────────────────────────────

/// `sys.getrecursionlimit()` default.
const MAX_KEY_PARTS: usize = 1000;
const FROZEN: u8 = 0;
const EXPLICIT_NEST: u8 = 1;

type Key = Vec<String>;

fn is_ascii_ctrl(c: char) -> bool {
    (c as u32) < 32 || c as u32 == 127
}
fn illegal_basic(c: char) -> bool {
    is_ascii_ctrl(c) && c != '\t'
}
fn illegal_multiline(c: char) -> bool {
    is_ascii_ctrl(c) && c != '\t' && c != '\n'
}
fn is_ws(c: char) -> bool {
    c == ' ' || c == '\t'
}
fn is_ws_nl(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\n'
}
fn is_bare(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

struct Src {
    s: Vec<char>,
    /// Python frames the open arrays / inline tables would hold (see [`NESTING_FRAME_BUDGET`]).
    frames: std::cell::Cell<usize>,
}

/// Lead (PORTING.md A-6): nesting is bounded like Python's recursion limit instead of by the Rust stack (an unbounded
/// descent aborted the whole process on a deeply nested document). Python spends 2 frames per array level
/// (`parse_value` + `parse_array`) and 3 per inline-table level (+ `parse_key_value_pair`); called from a shallow stack
/// `tomllib.loads` accepts 496 nested arrays, 330 nested inline tables and 198 `[{` pairs, which is exactly "at most
/// 992 frames". Deeper documents fail with Python's RecursionError text as a decode error (DEVIATION D-12).
const NESTING_FRAME_BUDGET: usize = 992;

/// Releases a nesting level's frames when the array / inline table is done.
struct Nest<'a>(&'a std::cell::Cell<usize>, usize);

impl Drop for Nest<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(self.1));
    }
}

impl Src {
    fn nest(&self, frames: usize) -> Result<Nest<'_>, TomlDecodeError> {
        let now = self.frames.get() + frames;
        if now > NESTING_FRAME_BUDGET {
            return Err(TomlDecodeError(
                "maximum recursion depth exceeded".to_owned(),
            ));
        }
        self.frames.set(now);
        Ok(Nest(&self.frames, frames))
    }
    fn at(&self, pos: usize) -> Option<char> {
        self.s.get(pos).copied()
    }
    fn starts_with(&self, pos: usize, pat: &str) -> bool {
        pat.chars()
            .enumerate()
            .all(|(k, c)| self.at(pos + k) == Some(c))
    }
    fn slice(&self, a: usize, b: usize) -> String {
        self.s[a.min(self.s.len())..b.min(self.s.len())]
            .iter()
            .collect()
    }
    fn len(&self) -> usize {
        self.s.len()
    }
    fn find(&self, pat: &str, from: usize) -> Option<usize> {
        let p: Vec<char> = pat.chars().collect();
        if p.is_empty() {
            return Some(from);
        }
        let mut i = from;
        while i + p.len() <= self.s.len() {
            if self.s[i..i + p.len()] == p[..] {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn err(&self, pos: usize, msg: &str) -> TomlDecodeError {
        let coord = if pos >= self.len() {
            "end of document".to_owned()
        } else {
            let line = self.s[..pos].iter().filter(|c| **c == '\n').count() + 1;
            let column = if line == 1 {
                pos + 1
            } else {
                let last_nl = self.s[..pos].iter().rposition(|c| *c == '\n').unwrap_or(0);
                pos - last_nl
            };
            format!("line {line}, column {column}")
        };
        TomlDecodeError(format!("{msg} (at {coord})"))
    }

    fn skip(&self, mut pos: usize, f: fn(char) -> bool) -> usize {
        while let Some(c) = self.at(pos) {
            if !f(c) {
                break;
            }
            pos += 1;
        }
        pos
    }
}

#[derive(Default)]
struct FlagNode {
    flags: Vec<u8>,
    recursive: Vec<u8>,
    nested: IndexMap<String, FlagNode>,
}

#[derive(Default)]
struct Flags {
    root: IndexMap<String, FlagNode>,
    pending: Vec<(Key, u8)>,
}

impl Flags {
    fn add_pending(&mut self, key: Key, flag: u8) {
        if !self.pending.iter().any(|(k, f)| *k == key && *f == flag) {
            self.pending.push((key, flag));
        }
    }

    fn finalize_pending(&mut self) {
        let p = std::mem::take(&mut self.pending);
        for (k, f) in p {
            self.set(&k, f, false);
        }
    }

    fn unset_all(&mut self, key: &[String]) {
        let mut cont = &mut self.root;
        let Some((stem, parent)) = key.split_last() else {
            return;
        };
        for k in parent {
            match cont.get_mut(k) {
                Some(n) => cont = &mut n.nested,
                None => return,
            }
        }
        cont.shift_remove(stem);
    }

    fn set(&mut self, key: &[String], flag: u8, recursive: bool) {
        let mut cont = &mut self.root;
        let Some((stem, parent)) = key.split_last() else {
            return;
        };
        for k in parent {
            cont = &mut cont.entry(k.clone()).or_default().nested;
        }
        let node = cont.entry(stem.clone()).or_default();
        let set = if recursive {
            &mut node.recursive
        } else {
            &mut node.flags
        };
        if !set.contains(&flag) {
            set.push(flag);
        }
    }

    fn is(&self, key: &[String], flag: u8) -> bool {
        let Some((stem, parent)) = key.split_last() else {
            return false;
        };
        let mut cont = &self.root;
        for k in parent {
            let Some(inner) = cont.get(k) else {
                return false;
            };
            if inner.recursive.contains(&flag) {
                return true;
            }
            cont = &inner.nested;
        }
        match cont.get(stem) {
            Some(n) => n.flags.contains(&flag) || n.recursive.contains(&flag),
            None => false,
        }
    }
}

/// Error of `get_or_create_nest` / `append_nest_to_list` (Python KeyError).
struct NestError;

fn get_or_create_nest<'a>(
    root: &'a mut Table,
    key: &[String],
    access_lists: bool,
) -> Result<&'a mut Table, NestError> {
    let mut cont: &mut Table = root;
    for k in key {
        let entry = cont
            .entry(k.clone())
            .or_insert_with(|| TomlVal::Table(Table::new()));
        let mut v: &mut TomlVal = entry;
        if access_lists && let TomlVal::Array(a) = v {
            match a.last_mut() {
                Some(last) => v = last,
                None => return Err(NestError),
            }
        }
        match v {
            TomlVal::Table(t) => cont = t,
            _ => return Err(NestError),
        }
    }
    Ok(cont)
}

fn append_nest_to_list(root: &mut Table, key: &[String]) -> Result<(), NestError> {
    let Some((last, parent)) = key.split_last() else {
        return Err(NestError);
    };
    let cont = get_or_create_nest(root, parent, true)?;
    match cont.get_mut(last) {
        Some(TomlVal::Array(a)) => {
            a.push(TomlVal::Table(Table::new()));
            Ok(())
        }
        Some(_) => Err(NestError),
        None => {
            cont.insert(
                last.clone(),
                TomlVal::Array(vec![TomlVal::Table(Table::new())]),
            );
            Ok(())
        }
    }
}

fn key_repr(key: &[String]) -> String {
    let parts: Vec<String> = key.iter().map(|k| str_repr(k)).collect();
    if parts.len() == 1 {
        format!("({},)", parts[0])
    } else {
        format!("({})", parts.join(", "))
    }
}

/// A document with at most this many `[` / `{` cannot nest deeper than that: it is parsed on the caller's stack.
const SHALLOW_OPENERS: usize = 32;

/// Lead (PORTING.md A-6): nesting up to [`NESTING_FRAME_BUDGET`] needs up to ~4 MiB of stack in a debug build, more
/// than a tokio worker thread has (2 MiB), so a document that could nest deeper than [`SHALLOW_OPENERS`] levels is
/// parsed on a scoped thread with this much stack (virtual: only the pages a deep document touches are used).
const PARSE_STACK_BYTES: usize = 16 * 1024 * 1024;

/// `tomllib.loads(s)`.
pub fn loads(s: &str) -> Result<Table, TomlDecodeError> {
    // the count of openers is an upper bound of the nesting depth whatever the strings and comments contain
    if s.bytes().filter(|b| matches!(b, b'[' | b'{')).count() <= SHALLOW_OPENERS {
        return loads_here(s);
    }
    std::thread::scope(|scope| {
        match std::thread::Builder::new()
            .name("pytoml".to_owned())
            .stack_size(PARSE_STACK_BYTES)
            .spawn_scoped(scope, || loads_here(s))
        {
            Ok(h) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
            Err(e) => Err(TomlDecodeError(format!(
                "cannot start the TOML parser thread: {e}"
            ))),
        }
    })
}

fn loads_here(s: &str) -> Result<Table, TomlDecodeError> {
    let src = Src {
        s: s.replace("\r\n", "\n").chars().collect(),
        frames: std::cell::Cell::new(0),
    };
    let mut pos = 0usize;
    let mut data = Table::new();
    let mut flags = Flags::default();
    let mut header: Key = Vec::new();
    loop {
        pos = src.skip(pos, is_ws);
        let Some(c) = src.at(pos) else { break };
        if c == '\n' {
            pos += 1;
            continue;
        }
        if is_bare(c) || c == '"' || c == '\'' {
            pos = key_value_rule(&src, pos, &mut data, &mut flags, &header)?;
            pos = src.skip(pos, is_ws);
        } else if c == '[' {
            let second = src.at(pos + 1);
            flags.finalize_pending();
            let (p, h) = if second == Some('[') {
                create_list_rule(&src, pos, &mut data, &mut flags)?
            } else {
                create_dict_rule(&src, pos, &mut data, &mut flags)?
            };
            pos = src.skip(p, is_ws);
            header = h;
        } else if c != '#' {
            return Err(src.err(pos, "Invalid statement"));
        }
        pos = skip_comment(&src, pos)?;
        let Some(c) = src.at(pos) else { break };
        if c != '\n' {
            return Err(src.err(pos, "Expected newline or end of document after a statement"));
        }
        pos += 1;
    }
    Ok(data)
}

fn skip_until(
    src: &Src,
    mut pos: usize,
    expect: &str,
    error_on: fn(char) -> bool,
    error_on_eof: bool,
) -> Result<usize, TomlDecodeError> {
    let new_pos = match src.find(expect, pos) {
        Some(p) => p,
        None => {
            let p = src.len();
            if error_on_eof {
                return Err(src.err(p, &format!("Expected {}", str_repr(expect))));
            }
            p
        }
    };
    if src.s[pos.min(src.len())..new_pos]
        .iter()
        .any(|c| error_on(*c))
    {
        while let Some(c) = src.at(pos) {
            if error_on(c) {
                break;
            }
            pos += 1;
        }
        let ch = src.at(pos).map(|c| c.to_string()).unwrap_or_default();
        return Err(src.err(pos, &format!("Found invalid character {}", str_repr(&ch))));
    }
    Ok(new_pos)
}

fn skip_comment(src: &Src, pos: usize) -> Result<usize, TomlDecodeError> {
    if src.at(pos) == Some('#') {
        return skip_until(src, pos + 1, "\n", illegal_basic, false);
    }
    Ok(pos)
}

fn skip_comments_and_array_ws(src: &Src, mut pos: usize) -> Result<usize, TomlDecodeError> {
    loop {
        let before = pos;
        pos = src.skip(pos, is_ws_nl);
        pos = skip_comment(src, pos)?;
        if pos == before {
            return Ok(pos);
        }
    }
}

fn create_dict_rule(
    src: &Src,
    pos: usize,
    data: &mut Table,
    flags: &mut Flags,
) -> Result<(usize, Key), TomlDecodeError> {
    let pos = src.skip(pos + 1, is_ws);
    let (pos, key) = parse_key(src, pos)?;
    if flags.is(&key, EXPLICIT_NEST) || flags.is(&key, FROZEN) {
        return Err(src.err(pos, &format!("Cannot declare {} twice", key_repr(&key))));
    }
    flags.set(&key, EXPLICIT_NEST, false);
    if get_or_create_nest(data, &key, true).is_err() {
        return Err(src.err(pos, "Cannot overwrite a value"));
    }
    if !src.starts_with(pos, "]") {
        return Err(src.err(pos, "Expected ']' at the end of a table declaration"));
    }
    Ok((pos + 1, key))
}

fn create_list_rule(
    src: &Src,
    pos: usize,
    data: &mut Table,
    flags: &mut Flags,
) -> Result<(usize, Key), TomlDecodeError> {
    let pos = src.skip(pos + 2, is_ws);
    let (pos, key) = parse_key(src, pos)?;
    if flags.is(&key, FROZEN) {
        return Err(src.err(
            pos,
            &format!("Cannot mutate immutable namespace {}", key_repr(&key)),
        ));
    }
    flags.unset_all(&key);
    flags.set(&key, EXPLICIT_NEST, false);
    if append_nest_to_list(data, &key).is_err() {
        return Err(src.err(pos, "Cannot overwrite a value"));
    }
    if !src.starts_with(pos, "]]") {
        return Err(src.err(pos, "Expected ']]' at the end of an array declaration"));
    }
    Ok((pos + 2, key))
}

fn key_value_rule(
    src: &Src,
    pos: usize,
    data: &mut Table,
    flags: &mut Flags,
    header: &[String],
) -> Result<usize, TomlDecodeError> {
    let (pos, key, value) = parse_key_value_pair(src, pos)?;
    let Some((stem, parent)) = key.split_last() else {
        return Err(src.err(pos, "Invalid statement"));
    };
    let mut abs_parent: Key = header.to_vec();
    abs_parent.extend(parent.iter().cloned());
    for i in 1..key.len() {
        let mut cont_key: Key = header.to_vec();
        cont_key.extend(key[..i].iter().cloned());
        if flags.is(&cont_key, EXPLICIT_NEST) {
            return Err(src.err(
                pos,
                &format!("Cannot redefine namespace {}", key_repr(&cont_key)),
            ));
        }
        flags.add_pending(cont_key, EXPLICIT_NEST);
    }
    if flags.is(&abs_parent, FROZEN) {
        return Err(src.err(
            pos,
            &format!(
                "Cannot mutate immutable namespace {}",
                key_repr(&abs_parent)
            ),
        ));
    }
    let Ok(nest) = get_or_create_nest(data, &abs_parent, true) else {
        return Err(src.err(pos, "Cannot overwrite a value"));
    };
    if nest.contains_key(stem) {
        return Err(src.err(pos, "Cannot overwrite a value"));
    }
    if matches!(value, TomlVal::Table(_) | TomlVal::Array(_)) {
        let mut full: Key = header.to_vec();
        full.extend(key.iter().cloned());
        flags.set(&full, FROZEN, true);
    }
    nest.insert(stem.clone(), value);
    Ok(pos)
}

fn parse_key_value_pair(src: &Src, pos: usize) -> Result<(usize, Key, TomlVal), TomlDecodeError> {
    let (pos, key) = parse_key(src, pos)?;
    if src.at(pos) != Some('=') {
        return Err(src.err(pos, "Expected '=' after a key in a key/value pair"));
    }
    let pos = src.skip(pos + 1, is_ws);
    let (pos, value) = parse_value(src, pos)?;
    Ok((pos, key, value))
}

fn parse_key(src: &Src, pos: usize) -> Result<(usize, Key), TomlDecodeError> {
    let (pos, part) = parse_key_part(src, pos)?;
    let mut key = vec![part];
    let mut pos = src.skip(pos, is_ws);
    loop {
        if src.at(pos) != Some('.') {
            return Ok((pos, key));
        }
        pos = src.skip(pos + 1, is_ws);
        let (p, part) = parse_key_part(src, pos)?;
        key.push(part);
        if key.len() > MAX_KEY_PARTS {
            // Python raises RecursionError here (not a TOMLDecodeError); the adapters treat it as a parse failure.
            return Err(TomlDecodeError(format!(
                "TOML key has more than the allowed {MAX_KEY_PARTS} parts"
            )));
        }
        pos = src.skip(p, is_ws);
    }
}

fn parse_key_part(src: &Src, pos: usize) -> Result<(usize, String), TomlDecodeError> {
    match src.at(pos) {
        Some(c) if is_bare(c) => {
            let end = src.skip(pos, is_bare);
            Ok((end, src.slice(pos, end)))
        }
        Some('\'') => parse_literal_str(src, pos),
        Some('"') => parse_basic_str(src, pos + 1, false),
        _ => Err(src.err(pos, "Invalid initial character for a key part")),
    }
}

fn parse_array(src: &Src, pos: usize) -> Result<(usize, TomlVal), TomlDecodeError> {
    let _nest = src.nest(2)?;
    let mut pos = skip_comments_and_array_ws(src, pos + 1)?;
    let mut array = Vec::new();
    if src.starts_with(pos, "]") {
        return Ok((pos + 1, TomlVal::Array(array)));
    }
    loop {
        let (p, val) = parse_value(src, pos)?;
        array.push(val);
        pos = skip_comments_and_array_ws(src, p)?;
        match src.at(pos) {
            Some(']') => return Ok((pos + 1, TomlVal::Array(array))),
            Some(',') => {}
            _ => return Err(src.err(pos, "Unclosed array")),
        }
        pos = skip_comments_and_array_ws(src, pos + 1)?;
        if src.starts_with(pos, "]") {
            return Ok((pos + 1, TomlVal::Array(array)));
        }
    }
}

fn parse_inline_table(src: &Src, pos: usize) -> Result<(usize, TomlVal), TomlDecodeError> {
    let _nest = src.nest(3)?;
    let mut pos = src.skip(pos + 1, is_ws);
    let mut table = Table::new();
    let mut flags = Flags::default();
    if src.starts_with(pos, "}") {
        return Ok((pos + 1, TomlVal::Table(table)));
    }
    loop {
        let (p, key, value) = parse_key_value_pair(src, pos)?;
        pos = p;
        let Some((stem, parent)) = key.split_last() else {
            return Err(src.err(pos, "Invalid statement"));
        };
        if flags.is(&key, FROZEN) {
            return Err(src.err(
                pos,
                &format!("Cannot mutate immutable namespace {}", key_repr(&key)),
            ));
        }
        let Ok(nest) = get_or_create_nest(&mut table, parent, false) else {
            return Err(src.err(pos, "Cannot overwrite a value"));
        };
        if nest.contains_key(stem) {
            return Err(src.err(
                pos,
                &format!("Duplicate inline table key {}", str_repr(stem)),
            ));
        }
        let is_container = matches!(value, TomlVal::Table(_) | TomlVal::Array(_));
        nest.insert(stem.clone(), value);
        pos = src.skip(pos, is_ws);
        match src.at(pos) {
            Some('}') => return Ok((pos + 1, TomlVal::Table(table))),
            Some(',') => {}
            _ => return Err(src.err(pos, "Unclosed inline table")),
        }
        if is_container {
            flags.set(&key, FROZEN, true);
        }
        pos = src.skip(pos + 1, is_ws);
    }
}

fn parse_basic_str_escape(
    src: &Src,
    pos: usize,
    multiline: bool,
) -> Result<(usize, String), TomlDecodeError> {
    let escape_id = src.slice(pos, pos + 2);
    let mut pos = pos + 2;
    if multiline && (escape_id == "\\ " || escape_id == "\\\t" || escape_id == "\\\n") {
        if escape_id != "\\\n" {
            pos = src.skip(pos, is_ws);
            match src.at(pos) {
                None => return Ok((pos, String::new())),
                Some('\n') => {}
                Some(_) => return Err(src.err(pos, "Unescaped '\\' in a string")),
            }
            pos += 1;
        }
        pos = src.skip(pos, is_ws_nl);
        return Ok((pos, String::new()));
    }
    match escape_id.as_str() {
        "\\u" => parse_hex_char(src, pos, 4),
        "\\U" => parse_hex_char(src, pos, 8),
        "\\b" => Ok((pos, "\u{8}".to_owned())),
        "\\t" => Ok((pos, "\t".to_owned())),
        "\\n" => Ok((pos, "\n".to_owned())),
        "\\f" => Ok((pos, "\u{c}".to_owned())),
        "\\r" => Ok((pos, "\r".to_owned())),
        "\\\"" => Ok((pos, "\"".to_owned())),
        "\\\\" => Ok((pos, "\\".to_owned())),
        _ => Err(src.err(pos, "Unescaped '\\' in a string")),
    }
}

fn parse_hex_char(src: &Src, pos: usize, len: usize) -> Result<(usize, String), TomlDecodeError> {
    let hex = src.slice(pos, pos + len);
    if hex.chars().count() != len || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(src.err(pos, "Invalid hex value"));
    }
    let pos = pos + len;
    let v = u32::from_str_radix(&hex, 16).unwrap_or(u32::MAX);
    match char::from_u32(v) {
        Some(c) => Ok((pos, c.to_string())),
        None => Err(src.err(pos, "Escaped character is not a Unicode scalar value")),
    }
}

fn parse_literal_str(src: &Src, pos: usize) -> Result<(usize, String), TomlDecodeError> {
    let start = pos + 1;
    let end = skip_until(src, start, "'", illegal_basic, true)?;
    Ok((end + 1, src.slice(start, end)))
}

fn parse_multiline_str(
    src: &Src,
    pos: usize,
    literal: bool,
) -> Result<(usize, String), TomlDecodeError> {
    let mut pos = pos + 3;
    if src.starts_with(pos, "\n") {
        pos += 1;
    }
    let (delim, mut result);
    if literal {
        delim = "'";
        let end = skip_until(src, pos, "'''", illegal_multiline, true)?;
        result = src.slice(pos, end);
        pos = end + 3;
    } else {
        delim = "\"";
        let (p, r) = parse_basic_str(src, pos, true)?;
        pos = p;
        result = r;
    }
    if !src.starts_with(pos, delim) {
        return Ok((pos, result));
    }
    pos += 1;
    if !src.starts_with(pos, delim) {
        result.push_str(delim);
        return Ok((pos, result));
    }
    pos += 1;
    result.push_str(delim);
    result.push_str(delim);
    Ok((pos, result))
}

fn parse_basic_str(
    src: &Src,
    mut pos: usize,
    multiline: bool,
) -> Result<(usize, String), TomlDecodeError> {
    let error_on: fn(char) -> bool = if multiline {
        illegal_multiline
    } else {
        illegal_basic
    };
    let mut result = String::new();
    let mut start = pos;
    loop {
        let Some(c) = src.at(pos) else {
            return Err(src.err(pos, "Unterminated string"));
        };
        if c == '"' {
            if !multiline {
                result.push_str(&src.slice(start, pos));
                return Ok((pos + 1, result));
            }
            if src.starts_with(pos, "\"\"\"") {
                result.push_str(&src.slice(start, pos));
                return Ok((pos + 3, result));
            }
            pos += 1;
            continue;
        }
        if c == '\\' {
            result.push_str(&src.slice(start, pos));
            let (p, esc) = parse_basic_str_escape(src, pos, multiline)?;
            result.push_str(&esc);
            pos = p;
            start = pos;
            continue;
        }
        if error_on(c) {
            return Err(src.err(
                pos,
                &format!("Illegal character {}", str_repr(&c.to_string())),
            ));
        }
        pos += 1;
    }
}

fn parse_value(src: &Src, pos: usize) -> Result<(usize, TomlVal), TomlDecodeError> {
    let c = src.at(pos);
    match c {
        Some('"') => {
            if src.starts_with(pos, "\"\"\"") {
                let (p, s) = parse_multiline_str(src, pos, false)?;
                return Ok((p, TomlVal::Str(s)));
            }
            let (p, s) = parse_basic_str(src, pos + 1, false)?;
            return Ok((p, TomlVal::Str(s)));
        }
        Some('\'') => {
            if src.starts_with(pos, "'''") {
                let (p, s) = parse_multiline_str(src, pos, true)?;
                return Ok((p, TomlVal::Str(s)));
            }
            let (p, s) = parse_literal_str(src, pos)?;
            return Ok((p, TomlVal::Str(s)));
        }
        Some('t') if src.starts_with(pos, "true") => return Ok((pos + 4, TomlVal::Bool(true))),
        Some('f') if src.starts_with(pos, "false") => {
            return Ok((pos + 5, TomlVal::Bool(false)));
        }
        Some('[') => return parse_array(src, pos),
        Some('{') => return parse_inline_table(src, pos),
        _ => {}
    }
    if let Some(m) = match_datetime(src, pos) {
        return match m.value() {
            Some(v) => Ok((m.end, v)),
            None => Err(src.err(pos, "Invalid date or datetime")),
        };
    }
    if let Some((end, t)) = match_time(src, pos) {
        return Ok((end, TomlVal::Time(t)));
    }
    if let Some(m) = match_number(src, pos) {
        return m;
    }
    let three = src.slice(pos, pos + 3);
    if three == "inf" || three == "nan" {
        return Ok((pos + 3, TomlVal::Float(py_float(&three))));
    }
    let four = src.slice(pos, pos + 4);
    if matches!(four.as_str(), "-inf" | "+inf" | "-nan" | "+nan") {
        return Ok((pos + 4, TomlVal::Float(py_float(&four))));
    }
    Err(src.err(pos, "Invalid value"))
}

fn py_float(s: &str) -> f64 {
    match s {
        "inf" | "+inf" => f64::INFINITY,
        "-inf" => f64::NEG_INFINITY,
        "nan" | "+nan" => f64::NAN,
        "-nan" => -f64::NAN,
        other => other.replace('_', "").parse::<f64>().unwrap_or(f64::NAN),
    }
}

fn digit_at(src: &Src, pos: usize) -> Option<u32> {
    src.at(pos).and_then(|c| c.to_digit(10))
}

/// `[0-9]{n}` at pos.
fn digits(src: &Src, pos: usize, n: usize) -> Option<u32> {
    let mut v = 0u32;
    for i in 0..n {
        v = v * 10 + digit_at(src, pos + i)?;
    }
    Some(v)
}

/// `_TIME_RE_STR`: `([01][0-9]|2[0-3]):([0-5][0-9]):([0-5][0-9])(?:\.([0-9]{1,6})[0-9]*)?`
fn match_time(src: &Src, pos: usize) -> Option<(usize, Time)> {
    let h1 = digit_at(src, pos)?;
    let h2 = digit_at(src, pos + 1)?;
    if !(h1 <= 1 || (h1 == 2 && h2 <= 3)) {
        return None;
    }
    if src.at(pos + 2) != Some(':') {
        return None;
    }
    let m1 = digit_at(src, pos + 3)?;
    let m2 = digit_at(src, pos + 4)?;
    if m1 > 5 || src.at(pos + 5) != Some(':') {
        return None;
    }
    let s1 = digit_at(src, pos + 6)?;
    let s2 = digit_at(src, pos + 7)?;
    if s1 > 5 {
        return None;
    }
    let mut end = pos + 8;
    let mut micro = 0u32;
    if src.at(end) == Some('.') && digit_at(src, end + 1).is_some() {
        end += 1;
        let mut frac = String::new();
        while let Some(d) = src.at(end).filter(char::is_ascii_digit) {
            if frac.len() < 6 {
                frac.push(d);
            }
            end += 1;
        }
        while frac.len() < 6 {
            frac.push('0');
        }
        micro = frac.parse().unwrap_or(0);
    }
    Some((
        end,
        Time {
            hour: h1 * 10 + h2,
            minute: m1 * 10 + m2,
            second: s1 * 10 + s2,
            micro,
        },
    ))
}

struct DtMatch {
    end: usize,
    date: (i32, u32, u32),
    time: Option<Time>,
    tz: Option<i32>,
}

impl DtMatch {
    /// `match_to_datetime` (None = Python ValueError: not a real calendar date).
    fn value(&self) -> Option<TomlVal> {
        let (y, m, d) = self.date;
        // date(0, ...) raises ValueError ("year 0 is out of range"): MINYEAR is 1
        if y == 0 || d > days_in_month(y, m) {
            return None;
        }
        let date = Date {
            year: y,
            month: m,
            day: d,
        };
        Some(match self.time {
            None => TomlVal::Date(date),
            Some(t) => TomlVal::DateTime(date, t, self.tz),
        })
    }
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// `RE_DATETIME`.
fn match_datetime(src: &Src, pos: usize) -> Option<DtMatch> {
    let y = digits(src, pos, 4)?;
    if src.at(pos + 4) != Some('-') {
        return None;
    }
    let mo = digits(src, pos + 5, 2)?;
    if !(1..=12).contains(&mo) || src.at(pos + 7) != Some('-') {
        return None;
    }
    let d1 = digit_at(src, pos + 8)?;
    let d2 = digit_at(src, pos + 9)?;
    let ok_day = match d1 {
        0 => d2 >= 1,
        1 | 2 => true,
        3 => d2 <= 1,
        _ => false,
    };
    if !ok_day {
        return None;
    }
    let mut out = DtMatch {
        end: pos + 10,
        date: (i32::try_from(y).unwrap_or(0), mo, d1 * 10 + d2),
        time: None,
        tz: None,
    };
    if matches!(src.at(pos + 10), Some('T' | 't' | ' '))
        && let Some((end, t)) = match_time(src, pos + 11)
    {
        out.time = Some(t);
        out.end = end;
        match src.at(end) {
            Some('Z' | 'z') => {
                out.tz = Some(0);
                out.end = end + 1;
            }
            Some(sign @ ('+' | '-')) => {
                let h1 = digit_at(src, end + 1);
                let h2 = digit_at(src, end + 2);
                let m1 = digit_at(src, end + 4);
                let m2 = digit_at(src, end + 5);
                if let (Some(h1), Some(h2), Some(m1), Some(m2)) = (h1, h2, m1, m2)
                    && (h1 <= 1 || (h1 == 2 && h2 <= 3))
                    && src.at(end + 3) == Some(':')
                    && m1 <= 5
                {
                    #[allow(clippy::cast_possible_wrap)]
                    let mins = ((h1 * 10 + h2) * 60 + m1 * 10 + m2) as i32;
                    out.tz = Some(if sign == '-' { -mins } else { mins });
                    out.end = end + 6;
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// CPython's default `sys.get_int_max_str_digits()`: `int(text)` refuses longer decimal texts and `str(int)` longer
/// results.
const INT_MAX_STR_DIGITS: usize = 4300;

/// Lead (PORTING.md A-6): above this many bits a 0x/0o/0b literal has more than 4300 decimal digits, so Python can load
/// it but `str()`/`repr()` of it raise ValueError; its decimal text is not computed (quadratic) and a placeholder is
/// kept (DEVIATION D-12).
const MAX_PRINTABLE_INT_BITS: usize = 14_300;

/// `RE_NUMBER` + `match_to_number`. `Err`: Python's ValueError for a decimal int over 4300 digits (an exception
/// `tomllib` lets escape; reported here as a decode error, DEVIATION D-12).
fn match_number(src: &Src, pos: usize) -> Option<Result<(usize, TomlVal), TomlDecodeError>> {
    // 0x / 0b / 0o
    if src.at(pos) == Some('0') {
        let radix_of = |c: Option<char>| match c {
            Some('x') => Some(16),
            Some('b') => Some(2),
            Some('o') => Some(8),
            _ => None,
        };
        if let Some(radix) = radix_of(src.at(pos + 1)) {
            let ok = |c: Option<char>| c.is_some_and(|c| c.is_digit(radix));
            if ok(src.at(pos + 2)) {
                let mut end = pos + 3;
                loop {
                    if ok(src.at(end)) {
                        end += 1;
                    } else if src.at(end) == Some('_') && ok(src.at(end + 1)) {
                        end += 2;
                    } else {
                        break;
                    }
                }
                let digits: String = src.slice(pos + 2, end).replace('_', "");
                let significant = digits.trim_start_matches('0');
                let bits_per_digit = match radix {
                    16 => 4,
                    8 => 3,
                    _ => 1,
                };
                if significant.len().saturating_mul(bits_per_digit) > MAX_PRINTABLE_INT_BITS {
                    return Some(Ok((
                        end,
                        TomlVal::BigInt(format!(
                            "<int with more than {INT_MAX_STR_DIGITS} digits>"
                        )),
                    )));
                }
                return Some(Ok((end, int_from(&digits, radix))));
            }
        }
    }
    // [+-]?(?:0|[1-9](?:_?[0-9])*)
    let mut end = pos;
    if matches!(src.at(end), Some('+' | '-')) {
        end += 1;
    }
    match src.at(end) {
        Some('0') => end += 1,
        Some(c) if c.is_ascii_digit() => {
            end += 1;
            end = dec_run(src, end);
        }
        _ => return None,
    }
    let int_end = end;
    // (?:\.[0-9](?:_?[0-9])*)?
    if src.at(end) == Some('.') && digit_at(src, end + 1).is_some() {
        end = dec_run(src, end + 2);
    }
    // (?:[eE][+-]?[0-9](?:_?[0-9])*)?
    if matches!(src.at(end), Some('e' | 'E')) {
        let mut e = end + 1;
        if matches!(src.at(e), Some('+' | '-')) {
            e += 1;
        }
        if digit_at(src, e).is_some() {
            end = dec_run(src, e + 1);
        }
    }
    let text = src.slice(pos, end);
    if end > int_end {
        return Some(Ok((end, TomlVal::Float(py_float(&text)))));
    }
    let clean = text.replace('_', "");
    let (neg, body) = match clean.strip_prefix('-') {
        Some(b) => (true, b.to_owned()),
        None => (false, clean.trim_start_matches('+').to_owned()),
    };
    if body.len() > INT_MAX_STR_DIGITS {
        return Some(Err(TomlDecodeError(format!(
            "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {} digits; \
             use sys.set_int_max_str_digits() to increase the limit",
            body.len()
        ))));
    }
    let v = int_from(&body, 10);
    Some(Ok((
        end,
        if neg {
            match v {
                TomlVal::Int(i) => TomlVal::Int(-i),
                TomlVal::BigInt(s) => TomlVal::BigInt(format!("-{s}")),
                other => other,
            }
        } else {
            v
        },
    )))
}

/// `(?:_?[0-9])*` after a first digit.
fn dec_run(src: &Src, mut end: usize) -> usize {
    loop {
        if digit_at(src, end).is_some() {
            end += 1;
        } else if src.at(end) == Some('_') && digit_at(src, end + 1).is_some() {
            end += 2;
        } else {
            return end;
        }
    }
}

fn int_from(digits: &str, radix: u32) -> TomlVal {
    match i128::from_str_radix(digits, radix) {
        Ok(v) => TomlVal::Int(v),
        Err(_) => TomlVal::BigInt(big_decimal(digits, radix)),
    }
}

/// Decimal text of an arbitrarily large unsigned integer in `radix`.
fn big_decimal(digits: &str, radix: u32) -> String {
    // little-endian base-1e9 limbs
    let mut limbs: Vec<u64> = vec![0];
    for c in digits.chars() {
        let d = u64::from(c.to_digit(radix).unwrap_or(0));
        let mut carry = d;
        for l in &mut limbs {
            let v = *l * u64::from(radix) + carry;
            *l = v % 1_000_000_000;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push(carry % 1_000_000_000);
            carry /= 1_000_000_000;
        }
    }
    let mut s = String::new();
    for (i, l) in limbs.iter().rev().enumerate() {
        if i == 0 {
            s.push_str(&l.to_string());
        } else {
            s.push_str(&format!("{l:09}"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &str) -> String {
        loads(s).err().map(|e| e.0).unwrap_or_default()
    }

    #[test]
    fn errors_match_tomllib() {
        // texts generated with CPython 3.13 tomllib (tests/fixtures/switchyard/tomllib_golden.json covers more)
        assert_eq!(err("a = ["), "Invalid value (at end of document)");
        assert_eq!(
            err("a = 1\nb"),
            "Expected '=' after a key in a key/value pair (at end of document)"
        );
        assert_eq!(
            err("[a\nx=1"),
            "Expected ']' at the end of a table declaration (at line 1, column 3)"
        );
        assert_eq!(err("= broken"), "Invalid statement (at line 1, column 1)");
    }

    #[test]
    fn values() {
        let t = loads("a = 1_000\nb = 0x1F\nc = 1.5e3\nd = 'x'\ne = [1, 2,]\n[f]\ng = true\n")
            .unwrap_or_default();
        assert_eq!(t["a"], TomlVal::Int(1000));
        assert_eq!(t["b"], TomlVal::Int(31));
        assert_eq!(t["c"], TomlVal::Float(1500.0));
        assert_eq!(t["d"].repr(), "'x'");
        assert_eq!(t["e"].repr(), "[1, 2]");
        assert_eq!(t["f"].repr(), "{'g': True}");
    }
}
