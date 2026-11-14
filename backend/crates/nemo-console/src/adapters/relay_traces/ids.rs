//! Small value types of the trace index: span ids, interned identifier strings and timestamps.
//!
//! The index holds identifier-shaped strings only (the adapter's `IDENT` rule). Every one of them is interned once
//! and referred to by a 4-byte symbol, and a UUID-shaped id is kept as its 128-bit value, so a span costs a fixed
//! number of bytes whatever names a capture uses.

use std::collections::HashMap;
use std::sync::Arc;

use super::chunkvec::ChunkVec;

/// The adapter's `IDENT` rule (`^[A-Za-z0-9_.:/@+\-]{1,120}$`) without a regex: this runs on every string field of
/// every event. `relay::ident_ok` is the reference; a unit test keeps the two equal.
#[must_use]
pub fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=120).contains(&b.len())
        && b.iter().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'/' | b'@' | b'+' | b'-')
        })
}

fn hex(c: u8) -> Option<u128> {
    match c {
        b'0'..=b'9' => Some(u128::from(c - b'0')),
        b'a'..=b'f' => Some(u128::from(c - b'a' + 10)),
        b'A'..=b'F' => Some(u128::from(c - b'A' + 10)),
        _ => None,
    }
}

/// `8-4-4-4-12` hexadecimal digits (either case) as a number.
#[must_use]
pub fn parse_uuid(s: &str) -> Option<u128> {
    let b = s.as_bytes();
    if b.len() != 36 {
        return None;
    }
    let mut v: u128 = 0;
    for (i, &c) in b.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if c != b'-' {
                return None;
            }
        } else {
            v = (v << 4) | hex(c)?;
        }
    }
    Some(v)
}

/// The canonical lower-case text of a UUID value.
#[must_use]
pub fn fmt_uuid(v: u128) -> String {
    let h = format!("{v:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// True when a path segment is UUID-shaped (what the routes accept as a trace, span or trajectory id).
#[must_use]
pub fn is_uuid(s: &str) -> bool {
    parse_uuid(s).is_some()
}

/// A span, mark or trajectory id.
///
/// NeMo Relay writes UUIDs, so the common case is the 128-bit value itself. Any other identifier-shaped id is still
/// data: it gets a slot in [`OddIds`] and is represented by a value whose upper 96 bits are all ones. A real UUID
/// with that prefix is sent through the table as well, so the two kinds can never be confused.
///
/// Two 64-bit halves instead of one `u128`: the rows of the index then need no 16-byte alignment padding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Id(u64, u64);

const ODD_PREFIX: u128 = !0u128 << 32;

impl Id {
    /// "No id" (a root span's parent).
    pub const NONE: Id = Id(u64::MAX, u64::MAX);

    #[must_use]
    pub const fn from_u128(v: u128) -> Id {
        #[allow(clippy::cast_possible_truncation)]
        Id((v >> 64) as u64, v as u64)
    }

    #[must_use]
    pub const fn as_u128(self) -> u128 {
        ((self.0 as u128) << 64) | self.1 as u128
    }

    #[must_use]
    pub fn is_none(self) -> bool {
        self == Id::NONE
    }

    fn odd(index: u32) -> Id {
        Id::from_u128(ODD_PREFIX | u128::from(index))
    }

    fn odd_index(self) -> Option<usize> {
        let v = self.as_u128();
        if v & ODD_PREFIX == ODD_PREFIX && !self.is_none() {
            usize::try_from(v & 0xffff_ffff).ok()
        } else {
            None
        }
    }
}

/// Ids that are not plain UUIDs, kept as text.
#[derive(Clone, Default)]
pub struct OddIds {
    strs: ChunkVec<Arc<str>>,
    map: HashMap<Arc<str>, u32>,
}

/// More distinct non-UUID ids than this are not kept (their events are counted as skipped).
const MAX_ODD_IDS: usize = 4_000_000;

impl OddIds {
    /// The id of `s`, creating a table entry when needed. `None`: not identifier-shaped, or the table is full.
    pub fn parse(&mut self, s: &str) -> Option<Id> {
        if let Some(v) = parse_uuid(s)
            && v & ODD_PREFIX != ODD_PREFIX
        {
            return Some(Id::from_u128(v));
        }
        if !is_ident(s) {
            return None;
        }
        if let Some(&i) = self.map.get(s) {
            return Some(Id::odd(i));
        }
        if self.strs.len() >= MAX_ODD_IDS {
            return None;
        }
        let i = u32::try_from(self.strs.len()).ok()?;
        let a: Arc<str> = Arc::from(s);
        self.strs.push(Arc::clone(&a));
        self.map.insert(a, i);
        Some(Id::odd(i))
    }

    /// The id of `s` when it is already known (requests never add entries).
    #[must_use]
    pub fn lookup(&self, s: &str) -> Option<Id> {
        if let Some(v) = parse_uuid(s)
            && v & ODD_PREFIX != ODD_PREFIX
        {
            return Some(Id::from_u128(v));
        }
        self.map.get(s).map(|&i| Id::odd(i))
    }

    /// The table without its lookup map: all a snapshot needs to print ids.
    #[must_use]
    pub fn frozen(&self) -> OddTable {
        OddTable(self.strs.clone())
    }
}

/// Read-only view of [`OddIds`] for a snapshot.
#[derive(Clone, Default)]
pub struct OddTable(ChunkVec<Arc<str>>);

impl OddTable {
    #[must_use]
    pub fn text(&self, id: Id) -> Option<String> {
        if id.is_none() {
            return None;
        }
        match id.odd_index() {
            Some(i) => self.0.get(i).map(|s| s.to_string()),
            None => Some(fmt_uuid(id.as_u128())),
        }
    }

    /// True when the id is a plain UUID (the only ids the detail routes can be asked for).
    #[must_use]
    pub fn is_uuid(id: Id) -> bool {
        !id.is_none() && id.odd_index().is_none()
    }
}

/// An interned identifier string.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Sym(pub u32);

impl Sym {
    pub const NONE: Sym = Sym(u32::MAX);

    #[must_use]
    pub fn is_none(self) -> bool {
        self == Sym::NONE
    }

    #[must_use]
    pub fn is_some(self) -> bool {
        self != Sym::NONE
    }
}

impl Default for Sym {
    fn default() -> Self {
        Sym::NONE
    }
}

/// The string table. Bounded: once `cap` distinct strings exist, new ones intern to [`Sym::NONE`] and are counted,
/// so a capture with unbounded distinct labels cannot grow the index without limit.
#[derive(Clone)]
pub struct Syms {
    strs: ChunkVec<Arc<str>>,
    map: HashMap<Arc<str>, u32>,
    cap: usize,
    pub dropped: u64,
}

impl Syms {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Syms {
            strs: ChunkVec::default(),
            map: HashMap::new(),
            cap,
            dropped: 0,
        }
    }

    /// Intern an identifier-shaped string; anything else (and `None`) is [`Sym::NONE`].
    pub fn intern(&mut self, s: Option<&str>) -> Sym {
        let Some(s) = s.filter(|s| is_ident(s)) else {
            return Sym::NONE;
        };
        if let Some(&i) = self.map.get(s) {
            return Sym(i);
        }
        let Ok(i) = u32::try_from(self.strs.len()) else {
            return Sym::NONE;
        };
        if self.strs.len() >= self.cap || i == u32::MAX {
            self.dropped += 1;
            return Sym::NONE;
        }
        let a: Arc<str> = Arc::from(s);
        self.strs.push(Arc::clone(&a));
        self.map.insert(a, i);
        Sym(i)
    }

    /// Intern text the index built itself from identifiers (a comma-joined list): no shape check, a length cap.
    pub fn intern_joined(&mut self, s: &str) -> Sym {
        if s.is_empty() || s.len() > 4096 {
            return Sym::NONE;
        }
        if let Some(&i) = self.map.get(s) {
            return Sym(i);
        }
        let Ok(i) = u32::try_from(self.strs.len()) else {
            return Sym::NONE;
        };
        if self.strs.len() >= self.cap || i == u32::MAX {
            self.dropped += 1;
            return Sym::NONE;
        }
        let a: Arc<str> = Arc::from(s);
        self.strs.push(Arc::clone(&a));
        self.map.insert(a, i);
        Sym(i)
    }

    #[must_use]
    pub fn lookup(&self, s: &str) -> Option<Sym> {
        self.map.get(s).map(|&i| Sym(i))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.strs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strs.len() == 0
    }

    #[must_use]
    pub fn frozen(&self) -> SymTable {
        SymTable(self.strs.clone())
    }
}

/// Read-only view of [`Syms`] for a snapshot.
#[derive(Clone, Default)]
pub struct SymTable(ChunkVec<Arc<str>>);

impl SymTable {
    #[must_use]
    pub fn get(&self, s: Sym) -> Option<&str> {
        if s.is_none() {
            return None;
        }
        self.0.get(s.0 as usize).map(|a| &**a)
    }

    /// The symbol of `s`, by a linear walk (requests name at most a handful of values; the snapshot keeps no map).
    #[must_use]
    pub fn find(&self, s: &str) -> Option<Sym> {
        (0..self.0.len())
            .find(|&i| self.0.get(i).is_some_and(|a| &**a == s))
            .and_then(|i| u32::try_from(i).ok())
            .map(Sym)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }
}

// ── timestamps ──────────────────────────────────────────────────────────────────────────────────

/// "No timestamp" in the microsecond fields of the index.
pub const NO_TS: i64 = i64::MIN;

/// Epoch seconds (float) for the API, `None` for [`NO_TS`].
#[must_use]
pub fn secs(us: i64) -> Option<f64> {
    if us == NO_TS {
        None
    } else {
        #[allow(clippy::cast_precision_loss)]
        Some(us as f64 / 1e6)
    }
}

/// Milliseconds between two microsecond stamps, `None` when either is missing.
#[must_use]
pub fn ms_between(start_us: i64, end_us: i64) -> Option<f64> {
    if start_us == NO_TS || end_us == NO_TS {
        None
    } else {
        #[allow(clippy::cast_precision_loss)]
        Some((end_us - start_us) as f64 / 1e3)
    }
}

fn two(b: &[u8], i: usize) -> Option<i64> {
    let (a, c) = (*b.get(i)?, *b.get(i + 1)?);
    if a.is_ascii_digit() && c.is_ascii_digit() {
        Some(i64::from(a - b'0') * 10 + i64::from(c - b'0'))
    } else {
        None
    }
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The common RFC 3339 shapes, straight from the bytes: `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`.
/// `None` means "not one of the plain shapes", not "invalid": the caller then asks the adapter's `parse_ts`.
fn fast_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let year = two(b, 0)? * 100 + two(b, 2)?;
    let (month, day) = (two(b, 5)?, two(b, 8)?);
    let (h, mi, sec) = (two(b, 11)?, two(b, 14)?, two(b, 17)?);
    // only dates every calendar agrees on; the rest (month ends, leap days) goes to the slow path
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=28).contains(&day) {
        return None;
    }
    if h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let mut i = 19;
    let mut frac: i64 = 0;
    if b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            // digits past the sixth are dropped (truncation, like the adapter)
            if i - start < 6 {
                frac = frac * 10 + i64::from(b[i] - b'0');
            }
            i += 1;
        }
        let digits = i - start;
        if digits == 0 {
            return None;
        }
        for _ in digits..6 {
            frac *= 10;
        }
    }
    let offset_s = match b.get(i)? {
        b'Z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let (oh, om) = (two(b, i + 1)?, two(b, i + 4)?);
            if oh > 14 || om > 59 {
                return None;
            }
            let v = oh * 3600 + om * 60;
            if *sign == b'-' { -v } else { v }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + h * 3600 + mi * 60 + sec - offset_s) * 1_000_000 + frac)
}

/// An RFC 3339 timestamp as epoch microseconds; the same instants (and the same refusals) as the adapter's
/// `parse_ts`, which stays the reference for every unusual form.
#[must_use]
pub fn parse_rfc3339_us(s: &str) -> Option<i64> {
    if let Some(us) = fast_rfc3339(s) {
        return Some(us);
    }
    let secs =
        crate::adapters::relay::parse_ts(Some(&crate::api::pyjson::PyJson::Str(s.to_owned())))?;
    #[allow(clippy::cast_possible_truncation)]
    Some((secs * 1e6).round() as i64)
}

/// A numeric timestamp as epoch microseconds. ATOF allows integer microseconds; other tools write seconds or
/// milliseconds, so the unit is read from the magnitude (every unit is unambiguous for dates after 1973).
#[must_use]
pub fn number_us(v: f64) -> Option<i64> {
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    let us = if v < 1e11 {
        v * 1e6
    } else if v < 1e14 {
        v * 1e3
    } else if v < 1e17 {
        v
    } else {
        v / 1e3
    };
    #[allow(clippy::cast_possible_truncation)]
    Some(us.round() as i64)
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::api::pyjson::PyJson;

    fn reference(s: &str) -> Option<i64> {
        crate::adapters::relay::parse_ts(Some(&PyJson::Str(s.to_owned())))
            .map(|v| (v * 1e6).round() as i64)
    }

    #[test]
    fn timestamps_equal_the_adapters_parse_ts() {
        for s in [
            "2026-11-01T12:00:00.000000+00:00",
            "2026-11-01T12:01:00.581862+00:00",
            "2026-09-24T01:02:03.123456789+00:00",
            "2026-09-24T01:02:03Z",
            "2026-09-24T01:02:03.5Z",
            "2026-09-24T01:02:03.000001-07:00",
            "2026-02-28T23:59:59.999999+05:30",
            "2026-01-31T00:00:00Z",
            "2024-02-29T10:00:00Z",
            "2026-02-30T10:00:00Z",
            "2026-13-01T10:00:00Z",
            "2026-11-01T24:00:00Z",
            "2026-11-01T12:00:60Z",
            "2026-11-01T12:00:00",
            "2026-11-01 12:00:00Z",
            "2026-11-01T12:00:00.Z",
            "2026-11-01T12:00:00+0000",
            "2026-11-01T12:00:00+15:00",
            "1969-12-31T23:59:59Z",
            "0001-01-01T00:00:00Z",
            "garbage",
            "",
        ] {
            assert_eq!(parse_rfc3339_us(s), reference(s), "{s}");
        }
        assert_eq!(
            parse_rfc3339_us("2026-11-01T12:00:00.000000+00:00"),
            Some(1_793_534_400_000_000)
        );
        // the fast path really is taken for the usual shape
        assert!(fast_rfc3339("2026-11-01T12:01:00.581862+00:00").is_some());
        assert!(fast_rfc3339("2026-01-31T00:00:00Z").is_none());
    }

    #[test]
    fn numeric_timestamps_by_magnitude() {
        assert_eq!(number_us(1_793_534_400.5), Some(1_793_534_400_500_000));
        assert_eq!(number_us(1_793_534_400_123.0), Some(1_793_534_400_123_000));
        assert_eq!(
            number_us(1_793_534_400_123_456.0),
            Some(1_793_534_400_123_456)
        );
        assert_eq!(
            number_us(1.793_534_400_123_456e18),
            Some(1_793_534_400_123_456)
        );
        assert_eq!(number_us(0.0), None);
        assert_eq!(number_us(f64::NAN), None);
    }

    #[test]
    fn ident_rule_equals_the_adapters() {
        for s in [
            "a",
            "A.b:c/d@e+f-g_h",
            "",
            " ",
            "two words",
            "tab\t",
            "ünï",
            "x\n",
            "sk-abc",
            &"a".repeat(120),
            &"a".repeat(121),
            "01a0f1e0-0004-733d-840b-7c9087b09e13",
        ] {
            assert_eq!(is_ident(s), crate::adapters::relay::ident_ok(s), "{s:?}");
        }
    }

    #[test]
    fn ids_round_trip_and_never_collide() {
        let mut odd = OddIds::default();
        let u = "01a0f1e0-0004-733d-840b-7c9087b09e13";
        let a = odd.parse(u).unwrap();
        assert_eq!(a, Id::from_u128(parse_uuid(u).unwrap()));
        let upper = odd.parse(&u.to_uppercase()).unwrap();
        assert_eq!(upper, a); // UUIDs compare case-insensitively
        let b = odd.parse("u-1").unwrap();
        let c = odd.parse("u-2").unwrap();
        assert!(b != c && b != a && !b.is_none());
        assert_eq!(odd.parse("u-1"), Some(b));
        // a UUID that looks like a table slot goes through the table too
        let tricky = "ffffffff-ffff-ffff-ffff-ffff00000000";
        let t = odd.parse(tricky).unwrap();
        assert!(t != b && t != c);
        let frozen = odd.frozen();
        assert_eq!(frozen.text(a).as_deref(), Some(u));
        assert_eq!(frozen.text(b).as_deref(), Some("u-1"));
        assert_eq!(frozen.text(t).as_deref(), Some(tricky));
        assert_eq!(frozen.text(Id::NONE), None);
        assert!(OddTable::is_uuid(a) && !OddTable::is_uuid(b) && !OddTable::is_uuid(t));
        assert_eq!(odd.lookup("u-2"), Some(c));
        assert_eq!(odd.lookup("never-seen"), None);
        assert_eq!(odd.parse("two words"), None);
        assert_eq!(fmt_uuid(parse_uuid(u).unwrap()), u);
        assert!(parse_uuid("01a0f1e0-0004-733d-840b-7c9087b09e1").is_none());
        assert!(parse_uuid("01a0f1e0x0004-733d-840b-7c9087b09e13").is_none());
    }

    #[test]
    fn the_string_table_is_bounded() {
        let mut s = Syms::new(2);
        let a = s.intern(Some("alpha"));
        let b = s.intern(Some("beta"));
        assert!(a.is_some() && b.is_some() && a != b);
        assert_eq!(s.intern(Some("alpha")), a);
        assert_eq!(s.intern(Some("gamma")), Sym::NONE);
        assert_eq!(s.intern(Some("not an ident")), Sym::NONE);
        assert_eq!(s.intern(None), Sym::NONE);
        assert_eq!(s.dropped, 1);
        let t = s.frozen();
        assert_eq!(t.get(a), Some("alpha"));
        assert_eq!(t.get(Sym::NONE), None);
        assert_eq!(t.find("beta"), Some(b));
    }
}
