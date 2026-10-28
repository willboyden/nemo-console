//! Python `str` / `float` / `datetime` behaviours the workstream A adapters (Switchyard, Relay, Dynamo) depend on, ported so
//! their outputs match byte for byte: `str.splitlines`, `str.strip`, `str.split()`, `float(str)` (PEP 515
//! underscores, `inf`/`nan` spellings, its ValueError text) and `datetime.fromisoformat` (CPython 3.13's C parser).
//! Written by workstream A (adapters/switchyard/pyutil.rs); moved into core at the workstream A merge (PORTING.md A-6), for every
//! adapter. The `str` helpers delegate to `services::pystr` (one implementation, A-7).

use crate::services::pyfmt::str_repr;

/// `str.isspace()` for one character (Unicode White_Space plus the ASCII separators U+001C..U+001F).
#[must_use]
pub fn py_isspace(c: char) -> bool {
    crate::services::pystr::is_space(c)
}

/// `s.strip()`.
#[must_use]
pub fn py_strip(s: &str) -> &str {
    crate::services::pystr::strip(s)
}

/// `s.rstrip()`.
#[must_use]
pub fn py_rstrip(s: &str) -> &str {
    crate::services::pystr::rstrip(s)
}

/// `s.split()` (runs of whitespace, no empty parts).
#[must_use]
pub fn py_split_ws(s: &str) -> Vec<&str> {
    crate::services::pystr::split_ws(s)
}

/// `s.splitlines()`: every Unicode line boundary, `\r\n` as one, no trailing empty item.
#[must_use]
pub fn py_splitlines(s: &str) -> Vec<&str> {
    crate::services::pystr::splitlines(s)
}

/// `float(s)`; `None` where Python raises ValueError.
#[must_use]
pub fn py_float(s: &str) -> Option<f64> {
    let t = py_strip(s);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let lower = body.to_ascii_lowercase();
    let v = match lower.as_str() {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        _ => {
            // digits with single underscores between digits, optional fraction, optional exponent
            let b = body.as_bytes();
            let mut clean = String::with_capacity(b.len());
            let mut i = 0usize;
            let mut seen_digit = false;
            let digit_run = |i: &mut usize, clean: &mut String| -> bool {
                let start = *i;
                while *i < b.len() {
                    if b[*i].is_ascii_digit() {
                        clean.push(char::from(b[*i]));
                        *i += 1;
                    } else if b[*i] == b'_'
                        && *i > start
                        && b[*i - 1].is_ascii_digit()
                        && b.get(*i + 1).is_some_and(u8::is_ascii_digit)
                    {
                        *i += 1;
                    } else {
                        break;
                    }
                }
                *i > start
            };
            if digit_run(&mut i, &mut clean) {
                seen_digit = true;
            }
            if b.get(i) == Some(&b'.') {
                clean.push('.');
                i += 1;
                if digit_run(&mut i, &mut clean) {
                    seen_digit = true;
                }
            }
            if !seen_digit {
                return None;
            }
            if matches!(b.get(i), Some(b'e' | b'E')) {
                clean.push('e');
                i += 1;
                if matches!(b.get(i), Some(b'+' | b'-')) {
                    clean.push(char::from(b[i]));
                    i += 1;
                }
                if !digit_run(&mut i, &mut clean) {
                    return None;
                }
            }
            if i != b.len() {
                return None;
            }
            clean.parse::<f64>().ok()?
        }
    };
    Some(if neg { -v } else { v })
}

/// The ValueError text of `float(s)`.
#[must_use]
pub fn float_error(s: &str) -> String {
    format!("could not convert string to float: {}", str_repr(s))
}

// ── datetime.fromisoformat (CPython 3.13, Modules/_datetimemodule.c) ───────────────────────────

/// A parsed `datetime`: civil fields plus the UTC offset in microseconds (`None` = naive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyDateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub micro: u32,
    pub offset_us: Option<i64>,
}

impl PyDateTime {
    /// Days since 1970-01-01 of the civil date.
    #[must_use]
    pub fn days_from_epoch(&self) -> i64 {
        days_from_civil(i64::from(self.year), self.month, self.day)
    }

    /// Microseconds since the epoch, treating a naive value as UTC.
    #[must_use]
    pub fn epoch_micros(&self) -> i64 {
        let secs = self.days_from_epoch() * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second);
        secs * 1_000_000 + i64::from(self.micro) - self.offset_us.unwrap_or(0)
    }

    /// `dt.timestamp()` of an AWARE datetime (`(dt - EPOCH).total_seconds()`: integer microseconds / 10**6). Naive
    /// values are taken as UTC (see the workstream A deviation on local time).
    #[must_use]
    pub fn timestamp(&self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let us = self.epoch_micros() as f64;
        us / 1_000_000.0
    }
}

/// Howard Hinnant's days_from_civil.
#[must_use]
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Bytes with a NUL sentinel past the end (the C code reads its terminator).
struct B<'a>(&'a [u8]);

impl B<'_> {
    fn at(&self, i: usize) -> u8 {
        self.0.get(i).copied().unwrap_or(0)
    }
}

fn parse_digits(b: &B<'_>, p: &mut usize, n: usize) -> Option<i64> {
    let mut v = 0i64;
    for _ in 0..n {
        let c = b.at(*p);
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + i64::from(c - b'0');
        *p += 1;
    }
    Some(v)
}

fn find_separator(b: &B<'_>, len: usize) -> usize {
    if len == 7 {
        return 7;
    }
    if b.at(4) == b'-' {
        if b.at(5) == b'W' {
            if len < 8 {
                return usize::MAX;
            }
            if len > 8 && b.at(8) == b'-' {
                if len == 9 {
                    return usize::MAX;
                }
                if len > 10 && b.at(10).is_ascii_digit() {
                    return 8;
                }
                return 10;
            }
            return 8;
        }
        return 10;
    }
    if b.at(4) == b'W' {
        let mut idx = 7;
        while idx < len && b.at(idx).is_ascii_digit() {
            idx += 1;
        }
        if idx < 9 {
            return idx;
        }
        return if idx % 2 == 0 { 7 } else { 8 };
    }
    8
}

/// `iso_to_ymd`.
fn iso_to_ymd(iso_year: i64, week: i64, day: i64) -> Option<(i64, u32, u32)> {
    if !(1..=53).contains(&week) {
        return None;
    }
    if !(1..=7).contains(&day) {
        return None;
    }
    if week == 53 {
        // only years starting on a Thursday (or leap years starting on a Wednesday) have week 53
        let first_wd = (days_from_civil(iso_year, 1, 1) + 3).rem_euclid(7); // 0 = Monday
        #[allow(clippy::cast_possible_truncation)]
        let leap = is_leap(iso_year as i32);
        if !(first_wd == 3 || (first_wd == 2 && leap)) {
            return None;
        }
    }
    // Monday of ISO week 1
    let jan4 = days_from_civil(iso_year, 1, 4);
    let jan4_wd = (jan4 + 3).rem_euclid(7);
    let week1_monday = jan4 - jan4_wd;
    let ord = week1_monday + (week - 1) * 7 + (day - 1);
    Some(civil_from_days(ord))
}

/// Inverse of [`days_from_civil`].
#[must_use]
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

fn parse_date(b: &B<'_>, len: usize) -> Option<(i64, u32, u32)> {
    let mut p = 0usize;
    let year = parse_digits(b, &mut p, 4)?;
    let sep = b.at(p) == b'-';
    if sep {
        p += 1;
    }
    if b.at(p) == b'W' {
        p += 1;
        let week = parse_digits(b, &mut p, 2)?;
        let day = if p < len {
            if sep {
                let c = b.at(p);
                p += 1;
                if c != b'-' {
                    return None;
                }
            }
            parse_digits(b, &mut p, 1)?
        } else {
            1
        };
        return iso_to_ymd(year, week, day);
    }
    let month = parse_digits(b, &mut p, 2)?;
    if sep {
        let c = b.at(p);
        p += 1;
        if c != b'-' {
            return None;
        }
    }
    let day = parse_digits(b, &mut p, 2)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((year, month as u32, day as u32))
}

/// `parse_hh_mm_ss_ff`: Ok(0|1) or Err.
fn parse_hms(b: &B<'_>, start: usize, end: usize, vals: &mut [i64; 4]) -> Result<i32, ()> {
    let mut p = start;
    let mut has_sep = true;
    for (i, slot) in vals.iter_mut().take(3).enumerate() {
        *slot = parse_digits(b, &mut p, 2).ok_or(())?;
        let c = b.at(p);
        p += 1;
        if i == 0 {
            has_sep = c == b':';
        }
        if p >= end {
            return Ok(i32::from(c != 0));
        } else if has_sep && c == b':' {
            if i == 2 {
                return Err(());
            }
            continue;
        } else if c == b'.' || c == b',' {
            if i < 2 {
                return Err(());
            }
            break;
        } else if !has_sep {
            p -= 1;
        } else {
            return Err(());
        }
    }
    let remains = end.saturating_sub(p);
    let to_parse = remains.min(6);
    let mut us = parse_digits(b, &mut p, to_parse).ok_or(())?;
    if to_parse < 6 {
        const CORR: [i64; 5] = [100_000, 10_000, 1000, 100, 10];
        us *= CORR.get(to_parse.wrapping_sub(1)).copied().unwrap_or(1);
    }
    vals[3] = us;
    while b.at(p).is_ascii_digit() {
        p += 1;
    }
    Ok(i32::from(b.at(p) != 0))
}

/// `datetime.fromisoformat(s)`; `None` where Python raises ValueError.
#[must_use]
pub fn fromisoformat(s: &str) -> Option<PyDateTime> {
    if s.chars().count() < 7 {
        return None;
    }
    let bytes = s.as_bytes();
    let b = B(bytes);
    let len = bytes.len();
    let sep = find_separator(&b, len);
    if sep == usize::MAX {
        return None;
    }
    let (year, month, day) = parse_date(&b, sep)?;
    let mut t = [0i64; 4];
    let mut tz: Option<i64> = None;
    if len > sep {
        let p = sep + 1;
        let end = len;
        let mut tzpos = p;
        loop {
            let c = b.at(tzpos);
            if c == b'Z' || c == b'+' || c == b'-' {
                break;
            }
            tzpos += 1;
            if tzpos >= end {
                break;
            }
        }
        let rv = parse_hms(&b, p, tzpos, &mut t).ok()?;
        if tzpos < end {
            if b.at(tzpos) == b'Z' {
                if b.at(tzpos + 1) != 0 {
                    return None;
                }
                tz = Some(0);
            } else {
                let sign = if b.at(tzpos) == b'-' { -1 } else { 1 };
                let mut z = [0i64; 4];
                let r = parse_hms(&b, tzpos + 1, end, &mut z).ok()?;
                if r != 0 {
                    return None;
                }
                let off = sign * ((z[0] * 3600 + z[1] * 60 + z[2]) * 1_000_000 + z[3]);
                if off.abs() >= 86_400 * 1_000_000 {
                    return None;
                }
                tz = Some(off);
            }
        } else if rv == 1 {
            return None;
        }
    }
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    let y32 = year as i32;
    if day < 1 || day > days_in_month(y32, month) {
        return None;
    }
    if t[0] > 23 || t[1] > 59 || t[2] > 59 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(PyDateTime {
        year: y32,
        month,
        day,
        hour: t[0] as u32,
        minute: t[1] as u32,
        second: t[2] as u32,
        micro: t[3] as u32,
        offset_us: tz,
    })
}

// ── Python objects decoded by `json.loads` (core `PyJson`) ─────────────────────────────────────

use crate::api::pyjson::PyJson;

/// `repr(obj)` of a JSON-decoded Python object.
#[must_use]
pub fn pj_repr(v: &PyJson) -> String {
    match v {
        PyJson::Null => "None".to_owned(),
        PyJson::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        PyJson::Int(i) => i.to_string(),
        PyJson::Float(f) => crate::services::pyfmt::float_repr(*f),
        PyJson::Str(s) => str_repr(s),
        PyJson::List(a) => {
            let inner: Vec<String> = a.iter().map(pj_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        PyJson::Dict(d) => {
            let inner: Vec<String> = d
                .iter()
                .map(|(k, v)| format!("{}: {}", str_repr(k), pj_repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// `str(obj)`; a missing key (`dict.get` -> None) is `"None"`.
#[must_use]
pub fn pj_str(v: Option<&PyJson>) -> String {
    match v {
        None => "None".to_owned(),
        Some(PyJson::Str(s)) => s.clone(),
        Some(other) => pj_repr(other),
    }
}

/// Python truthiness.
#[must_use]
pub fn pj_truthy(v: Option<&PyJson>) -> bool {
    match v {
        None | Some(PyJson::Null) => false,
        Some(PyJson::Bool(b)) => *b,
        Some(PyJson::Int(i)) => *i != 0,
        Some(PyJson::Float(f)) => *f != 0.0,
        Some(PyJson::Str(s)) => !s.is_empty(),
        Some(PyJson::List(a)) => !a.is_empty(),
        Some(PyJson::Dict(d)) => !d.is_empty(),
    }
}

/// `type(obj).__name__`.
#[must_use]
pub fn pj_type(v: &PyJson) -> &'static str {
    match v {
        PyJson::Null => "NoneType",
        PyJson::Bool(_) => "bool",
        PyJson::Int(_) => "int",
        PyJson::Float(_) => "float",
        PyJson::Str(_) => "str",
        PyJson::List(_) => "list",
        PyJson::Dict(_) => "dict",
    }
}

/// `obj.get(key)` on a dict (None for other types: callers check `isinstance` first, as the Python does).
#[must_use]
pub fn pj_get<'a>(v: Option<&'a PyJson>, key: &str) -> Option<&'a PyJson> {
    match v {
        Some(PyJson::Dict(d)) => d.get(key),
        _ => None,
    }
}

/// `json.loads(bytes)`: a leading UTF-8 BOM is accepted (Python detects `utf-8-sig` for bytes input).
pub fn json_loads_bytes(b: &[u8]) -> Option<PyJson> {
    let b = b.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(b);
    crate::api::pyjson::loads(b).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_and_strip() {
        assert_eq!(py_splitlines("a\r\nb\rc\n\nd\n"), ["a", "b", "c", "", "d"]);
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_strip("\u{1c} x \n"), "x");
        assert_eq!(py_split_ws(" a  b\nc "), ["a", "b", "c"]);
    }

    #[test]
    fn floats() {
        assert_eq!(py_float("1_000.5"), Some(1000.5));
        assert_eq!(py_float(" +Inf "), Some(f64::INFINITY));
        assert!(py_float("nan").is_some_and(f64::is_nan));
        assert_eq!(py_float("1e3"), Some(1000.0));
        assert_eq!(py_float(".5"), Some(0.5));
        assert_eq!(py_float("5."), Some(5.0));
        for bad in ["_1", "1_", "1__0", "", ".", "e5", "1e", "x"] {
            assert_eq!(py_float(bad), None, "{bad}");
        }
    }

    #[test]
    fn isoformat_matches_cpython() {
        let ts = |s: &str| fromisoformat(s).map(|d| d.timestamp());
        assert_eq!(ts("2026-09-24T07:43:41.664Z"), Some(1_790_235_821.664));
        assert!(fromisoformat("2026-01-01T24:00:00").is_none());
        assert!(fromisoformat("2026-09-24T07:43:41+25:00").is_none());
        assert!(fromisoformat("2026-09-24T07:43:41.Z").is_some());
        let w = fromisoformat("2026-W39-4").unwrap_or(PyDateTime {
            year: 0,
            month: 0,
            day: 0,
            hour: 0,
            minute: 0,
            second: 0,
            micro: 0,
            offset_us: None,
        });
        assert_eq!((w.year, w.month, w.day), (2026, 9, 24));
        assert!(fromisoformat("bad").is_none());
    }
}
