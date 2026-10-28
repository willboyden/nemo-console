//! Python objects as `yaml.safe_load` builds them, and the few Python built-ins the workstream C adapters apply to them
//! (`==` / dict-key equality, truthiness, `str()`, `repr()`, `int()`, `float()`, `str.strip()`, `str.splitlines()`).
//! Owned by workstream C (PORTING.md section 9: each workstream keeps its YAML handling in its own files; the integrator folds the
//! copies into one `services::pyyaml` after the merge).
//!
//! Containers live in an arena ([`Doc::objs`]) and are referenced by index, exactly like Python object references:
//! an alias shares the object instead of copying it (no exponential blow-up on "billion laughs" documents), and a
//! recursive alias (`&a [*a]`) is a cycle, as in Python. Every traversal that may follow a cycle or a very deep chain
//! is depth-guarded and fails with Python's `RecursionError` text instead of overflowing the stack.

use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use crate::services::pyfmt::{float_repr, str_repr};

/// Index of a container in [`Doc::objs`].
pub type ObjId = usize;

/// Nesting depth past which a recursive traversal raises `RecursionError` (Python's limit is 1000 frames; the
/// exact depth Python reaches depends on its call stack, see PORTING.md deviations of workstream C).
pub const MAX_DEPTH: usize = 490;

/// A Python `int`: machine-sized, or arbitrarily large kept as its decimal text (`-` sign included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PyInt {
    Small(i128),
    Big(String),
}

impl PyInt {
    /// The value as `i128`, when it fits.
    #[must_use]
    pub fn small(&self) -> Option<i128> {
        match self {
            PyInt::Small(i) => Some(*i),
            PyInt::Big(_) => None,
        }
    }

    /// `float(i)`: `None` is Python's `OverflowError` ("int too large to convert to float").
    #[must_use]
    pub fn to_f64(&self) -> Option<f64> {
        let f = match self {
            #[allow(clippy::cast_precision_loss)]
            PyInt::Small(i) => *i as f64,
            PyInt::Big(s) => s.parse::<f64>().unwrap_or(f64::INFINITY),
        };
        f.is_finite().then_some(f)
    }

    fn text(&self) -> String {
        match self {
            PyInt::Small(i) => i.to_string(),
            PyInt::Big(s) => s.clone(),
        }
    }
}

/// `datetime.date`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

/// `datetime.datetime` (naive when `tz` is `None`; `tz` = UTC offset in seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub date: Date,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub micro: u32,
    pub tz: Option<i32>,
}

impl DateTime {
    /// Seconds (and microseconds) since 0001-01-01 in UTC, for comparing aware datetimes.
    fn utc_key(&self) -> (i64, u32) {
        let days = days_from_civil(self.date.year, self.date.month, self.date.day);
        let secs = days * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
            - i64::from(self.tz.unwrap_or(0));
        (secs, self.micro)
    }
}

/// Days since the epoch of the proleptic Gregorian calendar (any fixed epoch works for comparisons).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe
}

/// A constructed Python object. Containers are references into [`Doc::objs`].
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Py {
    #[default]
    None,
    Bool(bool),
    Int(PyInt),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Date(Date),
    DateTime(DateTime),
    List(ObjId),
    Dict(ObjId),
    Set(ObjId),
    /// `(key, value)` items of `!!omap` / `!!pairs` lists.
    Tuple(Vec<Py>),
}

/// PyYAML's `nan_value`: ONE float object shared by every `.nan` scalar (so `.nan` dict keys are the same key).
/// Marked by a NaN payload no arithmetic produces.
pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_beef;

/// The canonical `.nan` value.
#[must_use]
pub fn canonical_nan() -> f64 {
    f64::from_bits(CANONICAL_NAN_BITS)
}

/// A container object.
#[derive(Debug, Clone, PartialEq)]
pub enum Obj {
    List(Vec<Py>),
    /// Insertion-ordered (key, value) pairs with unique keys (Python dict semantics).
    Dict(Vec<(Py, Py)>),
    Set(Vec<Py>),
}

/// A Python exception raised while building or reading the objects (not a YAML error): its class and `str(e)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyExc {
    pub class: &'static str,
    pub message: String,
}

impl PyExc {
    #[must_use]
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        PyExc {
            class,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn recursion() -> Self {
        PyExc::new("RecursionError", "maximum recursion depth exceeded")
    }
}

/// The result of one `safe_load`: the arena and the root object.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Doc {
    pub objs: Vec<Obj>,
    pub root: Py,
}

impl Doc {
    /// A document holding just `root` (no containers).
    #[must_use]
    pub fn scalar(root: Py) -> Self {
        Doc {
            objs: Vec::new(),
            root,
        }
    }

    /// The items of a dict (`None` when `v` is not a dict).
    #[must_use]
    pub fn dict(&self, v: &Py) -> Option<&[(Py, Py)]> {
        match v {
            Py::Dict(id) => match self.objs.get(*id) {
                Some(Obj::Dict(items)) => Some(items),
                _ => None,
            },
            _ => None,
        }
    }

    /// The items of a list (`None` when `v` is not a list).
    #[must_use]
    pub fn list(&self, v: &Py) -> Option<&[Py]> {
        match v {
            Py::List(id) => match self.objs.get(*id) {
                Some(Obj::List(items)) => Some(items),
                _ => None,
            },
            _ => None,
        }
    }

    fn set_items(&self, id: ObjId) -> &[Py] {
        match self.objs.get(id) {
            Some(Obj::Set(items)) => items,
            _ => &[],
        }
    }

    /// `d.get(key)` for a str key on a dict's items.
    #[must_use]
    pub fn get<'a>(items: &'a [(Py, Py)], key: &str) -> Option<&'a Py> {
        items.iter().find_map(|(k, v)| match k {
            Py::Str(s) if s == key => Some(v),
            _ => None,
        })
    }

    /// `key in d` for a str key.
    #[must_use]
    pub fn has(items: &[(Py, Py)], key: &str) -> bool {
        Doc::get(items, key).is_some()
    }

    /// `bool(v)`.
    #[must_use]
    pub fn truthy(&self, v: &Py) -> bool {
        match v {
            Py::None => false,
            Py::Bool(b) => *b,
            Py::Int(PyInt::Small(i)) => *i != 0,
            Py::Int(PyInt::Big(_)) | Py::Date(_) | Py::DateTime(_) => true,
            Py::Float(f) => *f != 0.0,
            Py::Str(s) => !s.is_empty(),
            Py::Bytes(b) => !b.is_empty(),
            Py::Tuple(t) => !t.is_empty(),
            Py::List(id) | Py::Dict(id) | Py::Set(id) => match self.objs.get(*id) {
                Some(Obj::List(x) | Obj::Set(x)) => !x.is_empty(),
                Some(Obj::Dict(x)) => !x.is_empty(),
                None => false,
            },
        }
    }

    /// Python `a == b` for the objects a YAML document can hold (used for dict keys and `x != 1` checks).
    #[must_use]
    pub fn eq(&self, a: &Py, b: &Py) -> bool {
        self.eq_depth(a, b, 0)
    }

    fn eq_depth(&self, a: &Py, b: &Py, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        if let (Some(x), Some(y)) = (num_of(a), num_of(b)) {
            return num_eq(&x, &y);
        }
        match (a, b) {
            (Py::None, Py::None) => true,
            (Py::Str(x), Py::Str(y)) => x == y,
            (Py::Bytes(x), Py::Bytes(y)) => x == y,
            (Py::Date(x), Py::Date(y)) => x == y,
            (Py::DateTime(x), Py::DateTime(y)) => match (x.tz, y.tz) {
                (None, None) => x == y,
                (Some(_), Some(_)) => x.utc_key() == y.utc_key(),
                _ => false,
            },
            (Py::Tuple(x), Py::Tuple(y)) => {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| self.eq_depth(p, q, depth + 1))
            }
            (Py::List(x), Py::List(y)) | (Py::Set(x), Py::Set(y)) | (Py::Dict(x), Py::Dict(y))
                if x == y =>
            {
                true
            }
            (Py::List(x), Py::List(y)) => match (self.objs.get(*x), self.objs.get(*y)) {
                (Some(Obj::List(p)), Some(Obj::List(q))) => {
                    p.len() == q.len()
                        && p.iter().zip(q).all(|(u, v)| self.eq_depth(u, v, depth + 1))
                }
                _ => false,
            },
            (Py::Dict(x), Py::Dict(y)) => match (self.objs.get(*x), self.objs.get(*y)) {
                (Some(Obj::Dict(p)), Some(Obj::Dict(q))) => {
                    p.len() == q.len()
                        && p.iter().all(|(k, v)| {
                            q.iter().any(|(k2, v2)| {
                                self.eq_depth(k, k2, depth + 1) && self.eq_depth(v, v2, depth + 1)
                            })
                        })
                }
                _ => false,
            },
            (Py::Set(x), Py::Set(y)) => {
                let (p, q) = (self.set_items(*x), self.set_items(*y));
                p.len() == q.len()
                    && p.iter()
                        .all(|u| q.iter().any(|v| self.eq_depth(u, v, depth + 1)))
            }
            _ => false,
        }
    }

    /// `isinstance(key, collections.abc.Hashable)`.
    #[must_use]
    pub fn hashable(v: &Py) -> bool {
        !matches!(v, Py::List(_) | Py::Dict(_) | Py::Set(_))
    }

    /// A hash consistent with [`Doc::eq`] for hashable objects (only used to index keys while building a dict).
    #[must_use]
    pub fn key_hash(&self, v: &Py) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash_into(v, &mut h, 0);
        h.finish()
    }

    fn hash_into(&self, v: &Py, h: &mut impl Hasher, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        if let Some(n) = num_of(v) {
            0u8.hash(h);
            match n {
                Num::Int(i) => i.hash(h),
                Num::Big(s) => s.hash(h),
                Num::Float(f) => {
                    if f.is_nan() {
                        f.to_bits().hash(h);
                    } else if f.fract() == 0.0 && f.abs() < 1e38 {
                        #[allow(clippy::cast_possible_truncation)]
                        (f as i128).hash(h);
                    } else if f.fract() == 0.0 && f.is_finite() {
                        format!("{f:.0}").hash(h);
                    } else {
                        f.to_bits().hash(h);
                    }
                }
            }
            return;
        }
        match v {
            Py::Str(s) => {
                1u8.hash(h);
                s.hash(h);
            }
            Py::Bytes(b) => {
                2u8.hash(h);
                b.hash(h);
            }
            Py::Date(d) => {
                3u8.hash(h);
                (d.year, d.month, d.day).hash(h);
            }
            Py::DateTime(d) => {
                4u8.hash(h);
                if d.tz.is_some() {
                    d.utc_key().hash(h);
                } else {
                    (
                        d.date.year,
                        d.date.month,
                        d.date.day,
                        d.hour,
                        d.minute,
                        d.second,
                        d.micro,
                    )
                        .hash(h);
                }
            }
            Py::Tuple(t) => {
                5u8.hash(h);
                for x in t {
                    self.hash_into(x, h, depth + 1);
                }
            }
            _ => 6u8.hash(h),
        }
    }

    /// `str(v)`.
    pub fn str_of(&self, v: &Py) -> Result<String, PyExc> {
        match v {
            Py::Str(s) => Ok(s.clone()),
            Py::Date(d) => Ok(date_iso(d)),
            Py::DateTime(d) => Ok(datetime_str(d)),
            _ => self.repr(v),
        }
    }

    /// `repr(v)`.
    pub fn repr(&self, v: &Py) -> Result<String, PyExc> {
        let mut out = String::new();
        let mut active = Vec::new();
        self.repr_into(v, &mut out, &mut active)?;
        Ok(out)
    }

    fn repr_into(&self, v: &Py, out: &mut String, active: &mut Vec<ObjId>) -> Result<(), PyExc> {
        if active.len() > MAX_DEPTH {
            return Err(PyExc::recursion());
        }
        match v {
            Py::None => out.push_str("None"),
            Py::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
            Py::Int(i) => out.push_str(&i.text()),
            Py::Float(f) => out.push_str(&float_repr(*f)),
            Py::Str(s) => out.push_str(&str_repr(s)),
            Py::Bytes(b) => out.push_str(&bytes_repr(b)),
            Py::Date(d) => {
                let _ = write!(out, "datetime.date({}, {}, {})", d.year, d.month, d.day);
            }
            Py::DateTime(d) => out.push_str(&datetime_repr(d)),
            Py::Tuple(t) => {
                out.push('(');
                for (i, x) in t.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.repr_into(x, out, active)?;
                }
                if t.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            Py::List(id) | Py::Dict(id) | Py::Set(id) => {
                let id = *id;
                if active.contains(&id) {
                    out.push_str(match v {
                        Py::List(_) => "[...]",
                        Py::Dict(_) => "{...}",
                        _ => "set(...)",
                    });
                    return Ok(());
                }
                active.push(id);
                match self.objs.get(id) {
                    Some(Obj::List(items)) => {
                        out.push('[');
                        for (i, x) in items.iter().enumerate() {
                            if i > 0 {
                                out.push_str(", ");
                            }
                            self.repr_into(x, out, active)?;
                        }
                        out.push(']');
                    }
                    Some(Obj::Dict(items)) => {
                        out.push('{');
                        for (i, (k, x)) in items.iter().enumerate() {
                            if i > 0 {
                                out.push_str(", ");
                            }
                            self.repr_into(k, out, active)?;
                            out.push_str(": ");
                            self.repr_into(x, out, active)?;
                        }
                        out.push('}');
                    }
                    Some(Obj::Set(items)) => {
                        if items.is_empty() {
                            out.push_str("set()");
                        } else {
                            out.push('{');
                            for (i, x) in items.iter().enumerate() {
                                if i > 0 {
                                    out.push_str(", ");
                                }
                                self.repr_into(x, out, active)?;
                            }
                            out.push('}');
                        }
                    }
                    None => out.push_str("None"),
                }
                active.pop();
            }
        }
        Ok(())
    }
}

/// A number for comparisons: Python compares bool, int and float by value.
enum Num {
    Int(i128),
    Big(String),
    Float(f64),
}

fn num_of(v: &Py) -> Option<Num> {
    match v {
        Py::Bool(b) => Some(Num::Int(i128::from(*b))),
        Py::Int(PyInt::Small(i)) => Some(Num::Int(*i)),
        Py::Int(PyInt::Big(s)) => Some(Num::Big(s.clone())),
        Py::Float(f) => Some(Num::Float(*f)),
        _ => None,
    }
}

fn num_eq(a: &Num, b: &Num) -> bool {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => x == y,
        (Num::Big(x), Num::Big(y)) => x == y,
        (Num::Int(_), Num::Big(_)) | (Num::Big(_), Num::Int(_)) => false,
        (Num::Float(x), Num::Float(y)) => {
            // dict keys: the shared `.nan` object is identical to itself
            x == y || (x.to_bits() == CANONICAL_NAN_BITS && y.to_bits() == CANONICAL_NAN_BITS)
        }
        (Num::Int(i), Num::Float(f)) | (Num::Float(f), Num::Int(i)) => {
            f.is_finite() && f.fract() == 0.0 && f.abs() < 1e38 && {
                #[allow(clippy::cast_possible_truncation)]
                let g = *f as i128;
                g == *i
            }
        }
        (Num::Big(s), Num::Float(f)) | (Num::Float(f), Num::Big(s)) => {
            f.is_finite() && f.fract() == 0.0 && format!("{f:.0}") == *s
        }
    }
}

fn date_iso(d: &Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

fn offset_text(off: i32) -> String {
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.unsigned_abs();
    let (h, m, s) = (a / 3600, (a % 3600) / 60, a % 60);
    if s != 0 {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    } else {
        format!("{sign}{h:02}:{m:02}")
    }
}

/// `str(datetime)` = `isoformat(sep=' ')`.
fn datetime_str(d: &DateTime) -> String {
    let mut s = format!(
        "{} {:02}:{:02}:{:02}",
        date_iso(&d.date),
        d.hour,
        d.minute,
        d.second
    );
    if d.micro != 0 {
        let _ = write!(s, ".{:06}", d.micro);
    }
    if let Some(off) = d.tz {
        s.push_str(&offset_text(off));
    }
    s
}

/// `repr(datetime.timedelta(seconds=secs))`.
#[must_use]
pub fn timedelta_repr(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let mut parts = Vec::new();
    if days != 0 {
        parts.push(format!("days={days}"));
    }
    if rest != 0 {
        parts.push(format!("seconds={rest}"));
    }
    if parts.is_empty() {
        "datetime.timedelta(0)".to_owned()
    } else {
        format!("datetime.timedelta({})", parts.join(", "))
    }
}

fn datetime_repr(d: &DateTime) -> String {
    let mut s = format!(
        "datetime.datetime({}, {}, {}, {}, {}",
        d.date.year, d.date.month, d.date.day, d.hour, d.minute
    );
    if d.second != 0 || d.micro != 0 {
        let _ = write!(s, ", {}", d.second);
    }
    if d.micro != 0 {
        let _ = write!(s, ", {}", d.micro);
    }
    match d.tz {
        Some(0) => s.push_str(", tzinfo=datetime.timezone.utc"),
        Some(off) => {
            let _ = write!(
                s,
                ", tzinfo=datetime.timezone({})",
                timedelta_repr(i64::from(off))
            );
        }
        None => {}
    }
    s.push(')');
    s
}

/// `repr(bytes)`.
#[must_use]
pub fn bytes_repr(b: &[u8]) -> String {
    let quote = if b.contains(&b'\'') && !b.contains(&b'"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from("b");
    out.push(quote);
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            c if char::from(c) == quote => {
                out.push('\\');
                out.push(quote);
            }
            0x20..=0x7e => out.push(char::from(c)),
            c => {
                let _ = write!(out, "\\x{c:02x}");
            }
        }
    }
    out.push(quote);
    out
}

// ── Python str built-ins ──────────────────────────────────────────────────────────────────────────

/// `ch.isspace()`.
#[must_use]
pub fn is_py_space(c: char) -> bool {
    crate::services::pystr::is_space(c)
}

/// `s.strip()`.
#[must_use]
pub fn py_strip(s: &str) -> &str {
    crate::services::pystr::strip(s)
}

/// `s.splitlines()`.
#[must_use]
pub fn py_splitlines(s: &str) -> Vec<&str> {
    crate::services::pystr::splitlines(s)
}

/// `int(s, base)` for the inputs PyYAML hands it (ASCII digits; surrounding whitespace allowed; a `0x`/`0o`/`0b`
/// prefix allowed when it matches the base). Errors carry Python's message.
/// CPython's default `sys.get_int_max_str_digits()`.
pub const INT_MAX_STR_DIGITS: usize = 4300;

/// Lead (A-7): above this many bits an int has more than 4300 decimal digits: Python can build it from a power-of-two
/// base but cannot `str()` it, and the decimal text would cost quadratic time here, so a placeholder is kept.
pub const MAX_PRINTABLE_INT_BITS: usize = 14_300;

/// What `int()` / `float()` do first: every Unicode decimal digit becomes its ASCII digit (`'١٢'` reads as `12`).
fn ascii_digits(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_ascii() {
        return std::borrow::Cow::Borrowed(s);
    }
    std::borrow::Cow::Owned(
        s.chars()
            .map(|c| {
                crate::services::pyyaml::pyunicode::decimal(c)
                    .and_then(|d| char::from_digit(d, 10))
                    .unwrap_or(c)
            })
            .collect(),
    )
}

pub fn py_int(s: &str, base: u32) -> Result<PyInt, PyExc> {
    let original = s;
    let ascii = ascii_digits(s);
    let s = ascii.as_ref();
    let bad = || {
        let s = original;
        let r = str_repr(s);
        let r = crate::services::pyfmt::char_prefix(&r, 200).to_owned();
        PyExc::new(
            "ValueError",
            format!("invalid literal for int() with base {base}: {r}"),
        )
    };
    let t = py_strip(s);
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let lower = t.to_ascii_lowercase();
    let body = match base {
        16 => lower.strip_prefix("0x").unwrap_or(&lower),
        8 => lower.strip_prefix("0o").unwrap_or(&lower),
        2 => lower.strip_prefix("0b").unwrap_or(&lower),
        _ => &lower,
    };
    // underscores: only single ones between digits (and right after a base prefix)
    let prefixed = body.len() != lower.len();
    let body = if prefixed {
        body.strip_prefix('_').unwrap_or(body)
    } else {
        body
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return Err(bad());
    }
    // validate first (like CPython's scan), then the digit limit, then the (quadratic) conversion
    if body.chars().any(|c| c != '_' && c.to_digit(base).is_none()) {
        return Err(bad());
    }
    let digits = body.chars().filter(|c| *c != '_').count();
    if base.is_power_of_two() {
        let significant = body
            .trim_start_matches(['0', '_'])
            .chars()
            .filter(|c| *c != '_')
            .count();
        if significant.saturating_mul(base.trailing_zeros() as usize) > MAX_PRINTABLE_INT_BITS {
            let sign = if neg { "-" } else { "" };
            return Ok(PyInt::Big(format!(
                "{sign}<int with more than {INT_MAX_STR_DIGITS} digits>"
            )));
        }
    } else if digits > INT_MAX_STR_DIGITS {
        return Err(PyExc::new(
            "ValueError",
            format!(
                "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {digits} \
                 digits; use sys.set_int_max_str_digits() to increase the limit"
            ),
        ));
    }
    let mut big = BigUint::zero();
    for c in body.chars() {
        if let Some(d) = c.to_digit(base) {
            big.mul_add(base, d);
        }
    }
    Ok(big.into_int(neg))
}

/// `float(s)` (CPython grammar: decimal or exponent forms, `inf`/`infinity`/`nan`, surrounding whitespace).
pub fn py_float(s: &str) -> Result<f64, PyExc> {
    let original = s;
    let ascii = ascii_digits(s);
    let s = ascii.as_ref();
    let bad = || {
        PyExc::new(
            "ValueError",
            format!("could not convert string to float: {}", str_repr(original)),
        )
    };
    let t = py_strip(s);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let low = body.to_ascii_lowercase();
    let v = if low == "inf" || low == "infinity" {
        f64::INFINITY
    } else if low == "nan" {
        f64::NAN
    } else {
        if !float_grammar(&low) {
            return Err(bad());
        }
        let clean: String = low.chars().filter(|c| *c != '_').collect();
        clean.parse::<f64>().map_err(|_| bad())?
    };
    Ok(if neg { -v } else { v })
}

/// digits with optional single underscores between digits, optional fraction, optional exponent.
fn float_grammar(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| -> usize {
        let start = *i;
        let mut last_us = false;
        while *i < b.len() && (b[*i].is_ascii_digit() || (b[*i] == b'_' && *i > start)) {
            if b[*i] == b'_' {
                if last_us {
                    return 0;
                }
                last_us = true;
            } else {
                last_us = false;
            }
            *i += 1;
        }
        if last_us {
            *i -= 1;
        }
        *i - start
    };
    let int_digits = digits(&mut i);
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        frac_digits = digits(&mut i);
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if i < b.len() && b[i] == b'e' {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// A tiny arbitrary-precision unsigned integer (base 1e9 limbs, little endian) for PyYAML's int construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigUint {
    limbs: Vec<u32>,
}

const LIMB: u64 = 1_000_000_000;

impl BigUint {
    #[must_use]
    pub fn zero() -> Self {
        BigUint { limbs: vec![0] }
    }

    /// `self = self * m + a`.
    pub fn mul_add(&mut self, m: u32, a: u32) {
        let mut carry = u64::from(a);
        for l in &mut self.limbs {
            let v = u64::from(*l) * u64::from(m) + carry;
            #[allow(clippy::cast_possible_truncation)]
            {
                *l = (v % LIMB) as u32;
            }
            carry = v / LIMB;
        }
        while carry > 0 {
            #[allow(clippy::cast_possible_truncation)]
            self.limbs.push((carry % LIMB) as u32);
            carry /= LIMB;
        }
    }

    /// `self += other`.
    pub fn add(&mut self, other: &BigUint) {
        let mut carry = 0u64;
        let n = self.limbs.len().max(other.limbs.len());
        self.limbs.resize(n, 0);
        for i in 0..n {
            let v = u64::from(self.limbs[i])
                + u64::from(other.limbs.get(i).copied().unwrap_or(0))
                + carry;
            #[allow(clippy::cast_possible_truncation)]
            {
                self.limbs[i] = (v % LIMB) as u32;
            }
            carry = v / LIMB;
        }
        if carry > 0 {
            #[allow(clippy::cast_possible_truncation)]
            self.limbs.push(carry as u32);
        }
    }

    /// `self = self * n` for a non-negative decimal `n`.
    pub fn mul_decimal(&mut self, n: &str) {
        let mut acc = BigUint::zero();
        for c in n.chars() {
            let d = c.to_digit(10).unwrap_or(0);
            acc.mul_add(10, 0);
            acc.add(&self.times(d));
        }
        *self = acc;
    }

    /// `self * m` for a small multiplier, as a new number.
    #[must_use]
    pub fn times(&self, m: u32) -> BigUint {
        let mut c = self.clone();
        c.mul_add(m, 0);
        c
    }

    /// Decimal text.
    #[must_use]
    pub fn decimal(&self) -> String {
        let mut it = self.limbs.iter().rev().skip_while(|l| **l == 0);
        let Some(first) = it.next() else {
            return "0".to_owned();
        };
        let mut s = first.to_string();
        for l in it {
            let _ = write!(s, "{l:09}");
        }
        s
    }

    /// As a Python int with a sign.
    #[must_use]
    pub fn into_int(self, neg: bool) -> PyInt {
        let d = self.decimal();
        let text = if neg && d != "0" { format!("-{d}") } else { d };
        match text.parse::<i128>() {
            Ok(i) => PyInt::Small(i),
            Err(_) => PyInt::Big(text),
        }
    }

    /// `float(self)` correctly rounded (infinite when out of range).
    #[must_use]
    pub fn to_f64(&self) -> f64 {
        self.decimal().parse::<f64>().unwrap_or(f64::INFINITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_and_strip_like_python() {
        assert_eq!(
            py_splitlines("a\r\nb\rc\n\nd"),
            vec!["a", "b", "c", "", "d"]
        );
        assert_eq!(py_splitlines("a\n"), vec!["a"]);
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_splitlines("x\u{85}y\u{2028}z"), vec!["x", "y", "z"]);
        assert_eq!(py_strip("\u{1c} a \u{85}"), "a");
    }

    #[test]
    fn int_and_float_parse_like_python() {
        assert_eq!(py_int("017", 8).ok(), Some(PyInt::Small(15)));
        assert_eq!(py_int(" 12 ", 10).ok(), Some(PyInt::Small(12)));
        assert_eq!(py_int("0o17", 8).ok(), Some(PyInt::Small(15)));
        assert_eq!(
            py_int("", 2).err().map(|e| e.message),
            Some("invalid literal for int() with base 2: ''".to_owned())
        );
        let big = py_int("123456789012345678901234567890123456789012", 10).ok();
        assert_eq!(
            big,
            Some(PyInt::Big(
                "123456789012345678901234567890123456789012".to_owned()
            ))
        );
        assert_eq!(py_float("1.").ok(), Some(1.0));
        assert_eq!(py_float(".5").ok(), Some(0.5));
        assert_eq!(py_float("1.e+5").ok(), Some(100_000.0));
        assert_eq!(py_float("-0.0").ok().map(f64::is_sign_negative), Some(true));
        assert!(py_float("1e").is_err() && py_float("e5").is_err() && py_float(".").is_err());
    }

    #[test]
    fn reprs_like_python() {
        let d = Doc::default();
        let dt = DateTime {
            date: Date {
                year: 2026,
                month: 1,
                day: 1,
            },
            hour: 10,
            minute: 0,
            second: 0,
            micro: 0,
            tz: Some(0),
        };
        assert_eq!(
            d.repr(&Py::DateTime(dt)).ok().as_deref(),
            Some("datetime.datetime(2026, 1, 1, 10, 0, tzinfo=datetime.timezone.utc)")
        );
        let dt2 = DateTime {
            micro: 500_000,
            tz: Some(-19_800),
            ..dt
        };
        assert_eq!(
            d.str_of(&Py::DateTime(dt2)).ok().as_deref(),
            Some("2026-01-01 10:00:00.500000-05:30")
        );
        assert_eq!(
            timedelta_repr(-1800),
            "datetime.timedelta(days=-1, seconds=84600)"
        );
        assert_eq!(bytes_repr(b"a'b\"c\x00\xff"), "b'a\\'b\"c\\x00\\xff'");
        assert_eq!(bytes_repr(b"it's"), "b\"it's\"");
    }
}
