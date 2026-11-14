//! Python `json.loads` for request bodies (core). FastAPI parses bodies with it and reports `JSONDecodeError.pos`
//! (a CHARACTER offset) as `body.<pos>: JSON decode error`; it also accepts `NaN`, `Infinity`, `-Infinity`, keeps the
//! LAST value of a duplicate key at the FIRST key's position, and refuses undecodable bytes (HTTP 400). This parser
//! reproduces those rules (pinned by tests/fixtures/api_golden.json).

use indexmap::IndexMap;
use serde_json::{Map, Number, Value};

/// A parsed JSON value with Python's number kinds (an int stays an int, NaN/Infinity are floats).
#[derive(Debug, Clone, PartialEq)]
pub enum PyJson {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<PyJson>),
    Dict(IndexMap<String, PyJson>),
}

/// Why a body did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonFail {
    /// `json.JSONDecodeError` at this character position (FastAPI: 422 `body.<pos>`).
    Decode(usize),
    /// A ValueError of `json.loads` that is not a JSONDecodeError (FastAPI: 400 "There was an error parsing the
    /// body"): the bytes are not text (`UnicodeDecodeError`), or an int literal over 4300 digits (see
    /// [`INT_MAX_STR_DIGITS`]).
    NotText,
    /// Nesting past [`MAX_DEPTH`]: Python's `RecursionError` (FastAPI: the same 400; but it is NOT a ValueError, so
    /// the audit middleware's `except ValueError` does not catch it and Python writes no audit row).
    TooDeep,
}

impl PyJson {
    /// To serde_json: non-finite floats become null (PORTING.md deviation D-6), huge ints become floats.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            PyJson::Null => Value::Null,
            PyJson::Bool(b) => Value::Bool(*b),
            PyJson::Int(i) => {
                if let Ok(v) = i64::try_from(*i) {
                    Value::from(v)
                } else if let Ok(v) = u64::try_from(*i) {
                    Value::from(v)
                } else {
                    #[allow(clippy::cast_precision_loss)]
                    Number::from_f64(*i as f64).map_or(Value::Null, Value::Number)
                }
            }
            PyJson::Float(f) => Number::from_f64(*f).map_or(Value::Null, Value::Number),
            PyJson::Str(s) => Value::String(s.clone()),
            PyJson::List(a) => Value::Array(a.iter().map(PyJson::to_value).collect()),
            PyJson::Dict(m) => {
                let mut out = Map::new();
                for (k, v) in m {
                    out.insert(k.clone(), v.to_value());
                }
                Value::Object(out)
            }
        }
    }
}

/// Nesting bound. Python's `json` decodes about 9,998 nested arrays/objects (CPython 3.13's C recursion limit; deeper
/// raises RecursionError, which FastAPI answers with 400). Here the bound is lower: the parser, the conversion to
/// serde_json, the validators and `Drop` all recurse, and an unbounded body used to overflow a 2 MiB worker stack and
/// abort the whole process (PORTING.md A-6). Bodies nested 513..9,998 deep get 400 here where Python validates them
/// (DEVIATION D-12); no API body nests beyond a handful of levels.
pub const MAX_DEPTH: usize = 512;

/// CPython's default `sys.get_int_max_str_digits()`: `int()` of a longer decimal text raises ValueError.
pub const INT_MAX_STR_DIGITS: usize = 4300;

/// `json.loads(body_bytes)`.
pub fn loads(bytes: &[u8]) -> Result<PyJson, JsonFail> {
    let text = std::str::from_utf8(bytes).map_err(|_| JsonFail::NotText)?;
    let chars: Vec<char> = text.chars().collect();
    if chars.first() == Some(&'\u{feff}') {
        return Err(JsonFail::Decode(0)); // "Unexpected UTF-8 BOM"
    }
    let mut p = Parser {
        s: &chars,
        i: 0,
        depth: 0,
    };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != chars.len() {
        return Err(JsonFail::Decode(p.i)); // "Extra data"
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
    /// Open arrays/objects (bounded by [`MAX_DEPTH`]).
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.i += 1;
        }
    }

    fn starts(&self, lit: &str) -> bool {
        let n = lit.chars().count();
        self.s.len() >= self.i + n && self.s[self.i..self.i + n].iter().copied().eq(lit.chars())
    }

    fn value(&mut self) -> Result<PyJson, JsonFail> {
        let start = self.i;
        match self.peek() {
            Some('"') => self.string().map(PyJson::Str),
            Some('{' | '[') => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(JsonFail::TooDeep); // RecursionError
                }
                let r = if self.peek() == Some('{') {
                    self.object()
                } else {
                    self.array()
                };
                self.depth -= 1;
                r
            }
            Some('n') if self.starts("null") => {
                self.i += 4;
                Ok(PyJson::Null)
            }
            Some('t') if self.starts("true") => {
                self.i += 4;
                Ok(PyJson::Bool(true))
            }
            Some('f') if self.starts("false") => {
                self.i += 5;
                Ok(PyJson::Bool(false))
            }
            Some('N') if self.starts("NaN") => {
                self.i += 3;
                Ok(PyJson::Float(f64::NAN))
            }
            Some('I') if self.starts("Infinity") => {
                self.i += 8;
                Ok(PyJson::Float(f64::INFINITY))
            }
            Some('-') if self.starts("-Infinity") => {
                self.i += 9;
                Ok(PyJson::Float(f64::NEG_INFINITY))
            }
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            _ => Err(JsonFail::Decode(start)),
        }
    }

    /// `-?(?:0|[1-9]\d*)(\.\d+)?([eE][-+]?\d+)?`; `Decode(start)` when it does not match at all,
    /// `NotText` for an int over [`INT_MAX_STR_DIGITS`] digits (Python's ValueError).
    fn number(&mut self) -> Result<PyJson, JsonFail> {
        let start = self.i;
        let mut j = self.i;
        if self.s.get(j) == Some(&'-') {
            j += 1;
        }
        match self.s.get(j) {
            Some('0') => j += 1,
            Some(c) if c.is_ascii_digit() => {
                while self.s.get(j).is_some_and(char::is_ascii_digit) {
                    j += 1;
                }
            }
            _ => return Err(JsonFail::Decode(start)),
        }
        let int_end = j;
        let mut is_float = false;
        if self.s.get(j) == Some(&'.') && self.s.get(j + 1).is_some_and(char::is_ascii_digit) {
            j += 1;
            while self.s.get(j).is_some_and(char::is_ascii_digit) {
                j += 1;
            }
            is_float = true;
        }
        if matches!(self.s.get(j), Some('e' | 'E')) {
            let mut k = j + 1;
            if matches!(self.s.get(k), Some('+' | '-')) {
                k += 1;
            }
            if self.s.get(k).is_some_and(char::is_ascii_digit) {
                while self.s.get(k).is_some_and(char::is_ascii_digit) {
                    k += 1;
                }
                j = k;
                is_float = true;
            }
        }
        let text: String = self.s[start..j].iter().collect();
        self.i = j;
        if is_float {
            Ok(PyJson::Float(text.parse::<f64>().unwrap_or(f64::NAN)))
        } else {
            let t: String = self.s[start..int_end].iter().collect();
            if t.trim_start_matches('-').len() > INT_MAX_STR_DIGITS {
                return Err(JsonFail::NotText);
            }
            Ok(match t.parse::<i128>() {
                Ok(v) => PyJson::Int(v),
                Err(_) => PyJson::Float(t.parse::<f64>().unwrap_or(f64::INFINITY)),
            })
        }
    }

    fn string(&mut self) -> Result<String, JsonFail> {
        let open = self.i;
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(JsonFail::Decode(open));
            }; // unterminated
            match c {
                '"' => {
                    self.i += 1;
                    return Ok(out);
                }
                '\\' => {
                    let esc_at = self.i;
                    self.i += 1;
                    let Some(e) = self.peek() else {
                        return Err(JsonFail::Decode(open));
                    };
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let cp = self.hex4(self.i + 1).ok_or(JsonFail::Decode(esc_at))?;
                            self.i += 4;
                            if (0xD800..0xDC00).contains(&cp)
                                && self.s.get(self.i + 1) == Some(&'\\')
                                && self.s.get(self.i + 2) == Some(&'u')
                                && let Some(lo) = self.hex4(self.i + 3)
                                && (0xDC00..0xE000).contains(&lo)
                            {
                                let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
                                self.i += 6;
                            } else {
                                // a lone surrogate is legal Python text but not Rust text (deviation D-6)
                                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                        }
                        _ => return Err(JsonFail::Decode(esc_at)), // "Invalid \escape"
                    }
                    self.i += 1;
                }
                c if u32::from(c) < 0x20 => return Err(JsonFail::Decode(self.i)), // "Invalid control character"
                c => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let digits: String = self.s.get(at..at + 4)?.iter().collect();
        if digits.chars().all(|c| c.is_ascii_hexdigit()) {
            u32::from_str_radix(&digits, 16).ok()
        } else {
            None
        }
    }

    fn object(&mut self) -> Result<PyJson, JsonFail> {
        self.i += 1;
        let mut m: IndexMap<String, PyJson> = IndexMap::new();
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(PyJson::Dict(m));
        }
        loop {
            if self.peek() != Some('"') {
                return Err(JsonFail::Decode(self.i)); // "Expecting property name enclosed in double quotes"
            }
            let key = self.string()?;
            self.ws();
            if self.peek() != Some(':') {
                return Err(JsonFail::Decode(self.i)); // "Expecting ':' delimiter"
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            m.insert(key, v);
            self.ws();
            match self.peek() {
                Some('}') => {
                    self.i += 1;
                    return Ok(PyJson::Dict(m));
                }
                Some(',') => {
                    let comma = self.i;
                    self.i += 1;
                    self.ws();
                    if self.peek() == Some('}') {
                        return Err(JsonFail::Decode(comma)); // "Illegal trailing comma before end of object"
                    }
                }
                _ => return Err(JsonFail::Decode(self.i)), // "Expecting ',' delimiter"
            }
        }
    }

    fn array(&mut self) -> Result<PyJson, JsonFail> {
        self.i += 1;
        let mut a = Vec::new();
        self.ws();
        if self.peek() == Some(']') {
            self.i += 1;
            return Ok(PyJson::List(a));
        }
        loop {
            a.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(']') => {
                    self.i += 1;
                    return Ok(PyJson::List(a));
                }
                Some(',') => {
                    let comma = self.i;
                    self.i += 1;
                    self.ws();
                    if self.peek() == Some(']') {
                        return Err(JsonFail::Decode(comma)); // "Illegal trailing comma before end of array"
                    }
                }
                _ => return Err(JsonFail::Decode(self.i)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_error_positions_match_cpython() {
        for (s, pos) in [
            (r#"{"a": 1,}"#, 7),
            ("[1,]", 2),
            (r#"{"a" 1}"#, 5),
            (r#"{"a": tru}"#, 6),
            (r#""abc"#, 0),
            ("{\"a\": \"x\ny\"}", 8),
            (r#"{"a": "\q"}"#, 7),
            ("  ", 2),
            ("\u{feff}{}", 0),
            (r#"{"a": -}"#, 6),
            (r#"{"a": 01}"#, 7),
            (r#"{"a":1}{}"#, 7),
            ("[1 2]", 3),
            (r#"{"urls": "#, 9),
            ("{,}", 1),
            (r#"{"content": "a"} x"#, 17),
        ] {
            assert_eq!(loads(s.as_bytes()), Err(JsonFail::Decode(pos)), "{s:?}");
        }
    }

    #[test]
    fn python_extensions_and_duplicates() {
        assert_eq!(loads(b"1e400"), Ok(PyJson::Float(f64::INFINITY)));
        assert!(matches!(loads(b"NaN"), Ok(PyJson::Float(f)) if f.is_nan()));
        let PyJson::Dict(m) = loads(br#"{"a":1,"a":2,"b":3}"#).unwrap() else {
            panic!()
        };
        assert_eq!(m.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(m["a"], PyJson::Int(2));
        assert_eq!(loads(b"\xff"), Err(JsonFail::NotText));
    }

    /// A-6: nesting is bounded (an unbounded body overflowed the worker stack and aborted the process), and ints follow
    /// Python's 4300-digit `int()` limit; both are "an error parsing the body" (400), like Python's exceptions.
    #[test]
    fn depth_and_int_limits_are_parse_errors_not_crashes() {
        let deep =
            |n: usize, open: &str, close: &str| format!("{}1{}", open.repeat(n), close.repeat(n));
        let r = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                assert!(loads(deep(MAX_DEPTH, "[", "]").as_bytes()).is_ok());
                assert!(loads(deep(MAX_DEPTH, "{\"a\":", "}").as_bytes()).is_ok());
                assert_eq!(
                    loads(deep(MAX_DEPTH + 1, "[", "]").as_bytes()),
                    Err(JsonFail::TooDeep)
                );
                assert_eq!(
                    loads("[".repeat(1_000_000).as_bytes()),
                    Err(JsonFail::TooDeep)
                );
                assert_eq!(
                    loads("{\"a\":".repeat(300_000).as_bytes()),
                    Err(JsonFail::TooDeep)
                );
            })
            .map(std::thread::JoinHandle::join);
        assert!(matches!(r, Ok(Ok(()))));
        assert!(loads("1".repeat(4300).as_bytes()).is_ok());
        assert_eq!(loads("1".repeat(4301).as_bytes()), Err(JsonFail::NotText));
        assert_eq!(
            loads(format!("-{}", "1".repeat(4301)).as_bytes()),
            Err(JsonFail::NotText)
        );
        assert!(matches!(
            loads(format!("{}.5", "1".repeat(5000)).as_bytes()),
            Ok(PyJson::Float(_))
        ));
    }
}
