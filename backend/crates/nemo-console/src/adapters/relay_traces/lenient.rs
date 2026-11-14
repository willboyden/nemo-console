//! Deserialisation building blocks for capture files.
//!
//! Two rules shape every type here. A capture is data from somewhere else, so a field of an unexpected type is never
//! an error: it reads as "absent". And the big payloads (messages, tool results, tool schemas) must cost nothing, so
//! whatever a field does not keep is skipped by the JSON reader without building a value or copying a string.

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::ids::{is_ident, number_us, parse_rfc3339_us};

pub(super) fn drain_seq<'de, A: SeqAccess<'de>>(mut a: A) -> Result<(), A::Error> {
    while a.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

pub(super) fn drain_map<'de, A: MapAccess<'de>>(mut a: A) -> Result<(), A::Error> {
    while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

/// The `visit_*` methods a lenient visitor does not care about: each answers the type's default ("absent") after
/// skipping the value. A visitor lists the kinds it wants ignored and writes the others itself.
macro_rules! skip_kinds {
    (bool) => {
        fn visit_bool<E: de::Error>(self, _v: bool) -> Result<Self::Value, E> {
            Ok(Self::Value::default())
        }
    };
    (int) => {
        fn visit_i64<E: de::Error>(self, _v: i64) -> Result<Self::Value, E> {
            Ok(Self::Value::default())
        }
        fn visit_u64<E: de::Error>(self, _v: u64) -> Result<Self::Value, E> {
            Ok(Self::Value::default())
        }
    };
    (float) => {
        fn visit_f64<E: de::Error>(self, _v: f64) -> Result<Self::Value, E> {
            Ok(Self::Value::default())
        }
    };
    (str) => {
        fn visit_str<E: de::Error>(self, _v: &str) -> Result<Self::Value, E> {
            Ok(Self::Value::default())
        }
    };
    (unit) => {
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Self::Value::default())
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Self::Value::default())
        }
    };
    (seq) => {
        fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
            $crate::adapters::relay_traces::lenient::drain_seq(a)?;
            Ok(Self::Value::default())
        }
    };
    (map) => {
        fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
            $crate::adapters::relay_traces::lenient::drain_map(a)?;
            Ok(Self::Value::default())
        }
    };
    ($($k:ident),+ $(,)?) => { $( skip_kinds!($k); )+ };
}
pub(super) use skip_kinds;

/// An identifier-shaped string (the adapter's `IDENT` rule), borrowed from the line when it has no escapes.
/// Any other string, and any other JSON type, is `None`: free text can never pass through a `Tok`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tok<'a>(pub Option<Cow<'a, str>>);

impl Tok<'_> {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        self.0.as_deref()
    }

    #[must_use]
    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Tok<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Tok<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(Tok(is_ident(v).then_some(Cow::Borrowed(v))))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Tok(is_ident(v).then(|| Cow::Owned(v.to_owned()))))
            }
            skip_kinds!(bool, int, float, unit, seq, map);
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// A JSON number (never a boolean, never a numeric string).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Num(pub Option<f64>);

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Num;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            #[allow(clippy::cast_precision_loss)]
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(Num(Some(v as f64)))
            }
            #[allow(clippy::cast_precision_loss)]
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Num(Some(v as f64)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(Num(v.is_finite().then_some(v)))
            }
            skip_kinds!(bool, str, unit, seq, map);
        }
        d.deserialize_any(V)
    }
}

/// A whole number: a JSON integer, or a float without a fraction (some writers print counts as `12.0`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Int(pub Option<i64>);

impl<'de> Deserialize<'de> for Int {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Int;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(Int(Some(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Int(i64::try_from(v).ok()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                #[allow(clippy::cast_possible_truncation)]
                Ok(Int(
                    (v.is_finite() && v.fract() == 0.0 && v.abs() < 9e15).then_some(v as i64)
                ))
            }
            skip_kinds!(bool, str, unit, seq, map);
        }
        d.deserialize_any(V)
    }
}

/// A JSON boolean.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Flag(pub Option<bool>);

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Flag;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(Flag(Some(v)))
            }
            skip_kinds!(int, float, str, unit, seq, map);
        }
        d.deserialize_any(V)
    }
}

/// The number of elements of a JSON array, counted without reading them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Count(pub Option<u32>);

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Count;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut n: u32 = 0;
                while a.next_element::<IgnoredAny>()?.is_some() {
                    n = n.saturating_add(1);
                }
                Ok(Count(Some(n)))
            }
            skip_kinds!(bool, int, float, str, unit, map);
        }
        d.deserialize_any(V)
    }
}

/// True when the key holds a value that says "yes, there is something": `true`, a number other than 0, a string, list
/// or object that is not empty. `null`, `false`, `0`, `""`, `[]` and `{}` are false. The value itself is skipped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Truthy(pub bool);

impl<'de> Deserialize<'de> for Truthy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Truthy;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(Truthy(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(Truthy(v != 0))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Truthy(v != 0))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(Truthy(v != 0.0))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Truthy(!v.is_empty()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut any = false;
                while a.next_element::<IgnoredAny>()?.is_some() {
                    any = true;
                }
                Ok(Truthy(any))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut any = false;
                while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
                    any = true;
                }
                Ok(Truthy(any))
            }
            skip_kinds!(unit);
        }
        d.deserialize_any(V)
    }
}

/// A nested object read as `T`; `null`, a string, a list or a number read as "absent".
#[derive(Debug, Clone, PartialEq)]
pub struct Opt<T>(pub Option<T>);

impl<T> Default for Opt<T> {
    fn default() -> Self {
        Opt(None)
    }
}

impl<T: Copy> Copy for Opt<T> {}

impl<T> Opt<T> {
    #[must_use]
    pub fn get(&self) -> Option<&T> {
        self.0.as_ref()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Opt<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Opt<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
                T::deserialize(de::value::MapAccessDeserializer::new(a)).map(|t| Opt(Some(t)))
            }
            skip_kinds!(bool, int, float, str, unit, seq);
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// A nested object read as `T`, plus whether the key held anything at all (a payload that is a bare string or a list
/// is still a payload, even though no field can be taken from it).
#[derive(Debug, Clone, PartialEq)]
pub struct Payload<T> {
    pub value: Option<T>,
    pub present: bool,
}

impl<T> Default for Payload<T> {
    fn default() -> Self {
        Payload {
            value: None,
            present: false,
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Payload<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        fn some<T>() -> Payload<T> {
            Payload {
                value: None,
                present: true,
            }
        }
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Payload<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
                T::deserialize(de::value::MapAccessDeserializer::new(a)).map(|t| Payload {
                    value: Some(t),
                    present: true,
                })
            }
            fn visit_bool<E: de::Error>(self, _v: bool) -> Result<Self::Value, E> {
                Ok(some())
            }
            fn visit_i64<E: de::Error>(self, _v: i64) -> Result<Self::Value, E> {
                Ok(some())
            }
            fn visit_u64<E: de::Error>(self, _v: u64) -> Result<Self::Value, E> {
                Ok(some())
            }
            fn visit_f64<E: de::Error>(self, _v: f64) -> Result<Self::Value, E> {
                Ok(some())
            }
            fn visit_str<E: de::Error>(self, _v: &str) -> Result<Self::Value, E> {
                Ok(some())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
                drain_seq(a)?;
                Ok(some())
            }
            skip_kinds!(unit);
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// The first `N` elements of a JSON array as `T`, and how many elements there were. The rest is skipped unread.
#[derive(Debug, Clone, PartialEq)]
pub struct Seq<T, const N: usize> {
    pub items: Vec<T>,
    pub total: u32,
}

impl<T, const N: usize> Default for Seq<T, N> {
    fn default() -> Self {
        Seq {
            items: Vec::new(),
            total: 0,
        }
    }
}

impl<'de, T: Deserialize<'de>, const N: usize> Deserialize<'de> for Seq<T, N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T, const N: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for V<T, N> {
            type Value = Seq<T, N>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut out = Seq::default();
                loop {
                    if out.items.len() < N {
                        match a.next_element::<T>()? {
                            Some(t) => out.items.push(t),
                            None => break,
                        }
                    } else if a.next_element::<IgnoredAny>()?.is_none() {
                        break;
                    }
                    out.total = out.total.saturating_add(1);
                }
                Ok(out)
            }
            skip_kinds!(bool, int, float, str, unit, map);
        }
        d.deserialize_any(V(PhantomData))
    }
}

/// A timestamp as epoch microseconds: an RFC 3339 string or a number (see `ids::number_us`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stamp(pub Option<i64>);

impl<'de> Deserialize<'de> for Stamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Stamp;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Stamp(if v.len() <= 64 {
                    parse_rfc3339_us(v)
                } else {
                    None
                }))
            }
            #[allow(clippy::cast_precision_loss)]
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(Stamp(number_us(v as f64)))
            }
            #[allow(clippy::cast_precision_loss)]
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Stamp(number_us(v as f64)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(Stamp(number_us(v)))
            }
            skip_kinds!(bool, unit, seq, map);
        }
        d.deserialize_any(V)
    }
}

/// The product token of a `user-agent` value (`opencode/2.0.0 (linux)` -> `opencode`): the text before the first
/// `/` or blank, kept only when it is a short identifier. The rest of the header never leaves the line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Product(pub Option<String>);

/// The product token of a user-agent string, or `None` when it is not a plain short name.
#[must_use]
pub fn product_token(ua: &str) -> Option<&str> {
    let end = ua
        .find(|c: char| c == '/' || c.is_whitespace())
        .unwrap_or(ua.len());
    let tok = &ua[..end];
    let ok = (1..=40).contains(&tok.len())
        && tok
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'+' | b'-'));
    ok.then_some(tok)
}

impl<'de> Deserialize<'de> for Product {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Product;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Product(product_token(v).map(str::to_owned)))
            }
            skip_kinds!(bool, int, float, unit, seq, map);
        }
        d.deserialize_any(V)
    }
}

/// Identifier pairs of a small JSON object (a metric's attributes): at most `N`, keys and values identifier-shaped.
/// Numbers and booleans count as their text (`upstream_status = 502`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pairs<const N: usize>(pub Vec<(String, String)>);

impl<'de, const N: usize> Deserialize<'de> for Pairs<N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        /// A scalar as short identifier text.
        #[derive(Default)]
        struct Scalar(Option<String>);
        impl<'de> Deserialize<'de> for Scalar {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct S;
                impl<'de> Visitor<'de> for S {
                    type Value = Scalar;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("anything")
                    }
                    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                        Ok(Scalar(is_ident(v).then(|| v.to_owned())))
                    }
                    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                        Ok(Scalar(Some(v.to_string())))
                    }
                    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                        Ok(Scalar(Some(v.to_string())))
                    }
                    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                        Ok(Scalar(Some(v.to_string())))
                    }
                    skip_kinds!(float, unit, seq, map);
                }
                d.deserialize_any(S)
            }
        }
        struct V<const N: usize>;
        impl<'de, const N: usize> Visitor<'de> for V<N> {
            type Value = Pairs<N>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(k) = a.next_key::<Cow<'de, str>>()? {
                    if out.len() < N && is_ident(&k) {
                        if let Scalar(Some(v)) = a.next_value::<Scalar>()? {
                            out.push((k.into_owned(), v));
                        }
                    } else {
                        a.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(Pairs(out))
            }
            skip_kinds!(bool, int, float, str, unit, seq);
        }
        d.deserialize_any(V::<N>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, Default, Debug)]
    #[serde(default)]
    struct Probe<'a> {
        #[serde(borrow)]
        tok: Tok<'a>,
        num: Num,
        int: Int,
        flag: Flag,
        count: Count,
        truthy: Truthy,
        #[serde(borrow)]
        opt: Opt<Inner<'a>>,
        #[serde(borrow)]
        payload: Payload<Inner<'a>>,
        #[serde(borrow)]
        seq: Seq<Tok<'a>, 2>,
        stamp: Stamp,
        product: Product,
        pairs: Pairs<2>,
    }

    #[derive(Deserialize, Default, Debug, PartialEq)]
    #[serde(default)]
    struct Inner<'a> {
        #[serde(borrow)]
        name: Tok<'a>,
    }

    #[test]
    fn the_expected_types_are_read() {
        let p: Probe<'_> = serde_json::from_str(
            r#"{"tok":"local-small","num":1.5,"int":12,"flag":true,"count":[1,[2],{"a":3}],"truthy":"x",
                "opt":{"name":"n1","other":[1,2]},"payload":{"name":"n2"},"seq":["a","b","c","d"],
                "stamp":"2026-11-01T12:00:00.000000+00:00","product":"opencode/2.0.0 (linux)",
                "pairs":{"token_type":"input","upstream_status":502,"ok":true,"free text":"x","third":"dropped"},
                "unknown":{"deep":[{"er":1}]}}"#,
        )
        .unwrap();
        assert_eq!(p.tok.as_str(), Some("local-small"));
        assert!(matches!(p.tok.0, Some(Cow::Borrowed(_))));
        assert_eq!(p.num.0, Some(1.5));
        assert_eq!(p.int.0, Some(12));
        assert_eq!(p.flag.0, Some(true));
        assert_eq!(p.count.0, Some(3));
        assert!(p.truthy.0);
        assert_eq!(p.opt.get().unwrap().name.as_str(), Some("n1"));
        assert_eq!(p.payload.value.unwrap().name.as_str(), Some("n2"));
        assert!(p.payload.present);
        assert_eq!(p.seq.total, 4);
        assert_eq!(p.seq.items.len(), 2);
        assert_eq!(p.stamp.0, Some(1_793_534_400_000_000));
        assert_eq!(p.product.0.as_deref(), Some("opencode"));
        assert_eq!(
            p.pairs.0,
            vec![
                ("token_type".to_owned(), "input".to_owned()),
                ("upstream_status".to_owned(), "502".to_owned())
            ]
        );
    }

    #[test]
    fn a_wrong_type_is_absent_never_an_error() {
        for wrong in [
            "null",
            "true",
            "7",
            "7.5",
            r#""two words""#,
            "[1,2]",
            r#"{"a":{"b":[1]}}"#,
        ] {
            let text = format!(
                r#"{{"tok":{wrong},"num":"1","int":true,"flag":"yes","count":{{"n":1}},"opt":{wrong2},
                    "seq":{wrong2},"stamp":[1],"product":{wrong3},"pairs":[1]}}"#,
                wrong2 = if wrong.starts_with('{') { "[3]" } else { wrong },
                wrong3 = if wrong.starts_with('"') { "7" } else { wrong },
            );
            let p: Probe<'_> =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{wrong}: {e}"));
            assert_eq!(p.tok.as_str(), None, "{wrong}");
            assert_eq!(
                (p.num.0, p.int.0, p.flag.0, p.count.0),
                (None, None, None, None)
            );
            assert!(p.opt.get().is_none() && p.stamp.0.is_none());
            assert!(p.seq.items.iter().all(|t| t.as_str().is_none()));
            assert!(p.product.0.is_none() && p.pairs.0.is_empty());
        }
        // a payload that is not an object is still a payload
        let p: Probe<'_> =
            serde_json::from_str(r#"{"payload":"plain text result","truthy":null}"#).unwrap();
        assert!(p.payload.present && p.payload.value.is_none() && !p.truthy.0);
        for (v, want) in [
            ("false", false),
            ("0", false),
            ("\"\"", false),
            ("[]", false),
            ("{}", false),
            ("true", true),
            ("2", true),
            ("[0]", true),
            ("{\"a\":null}", true),
        ] {
            let text = format!(r#"{{"truthy":{v}}}"#);
            let p: Probe<'_> = serde_json::from_str(&text).unwrap();
            assert_eq!(p.truthy.0, want, "{v}");
        }
        let p: Probe<'_> = serde_json::from_str(r#"{"payload":null}"#).unwrap();
        assert!(!p.payload.present);
        // an escaped identifier is still an identifier (owned copy), an escaped text is not
        let p: Probe<'_> = serde_json::from_str(r#"{"tok":"a-b"}"#).unwrap();
        assert_eq!(p.tok.as_str(), Some("a-b"));
        let p: Probe<'_> =
            serde_json::from_str(r#"{"tok":"a\nb","int":12.0,"num":1e400}"#).unwrap_or_default();
        assert_eq!(p.tok.as_str(), None);
    }

    #[test]
    fn product_tokens() {
        assert_eq!(
            product_token("claude-cli/2.1.0 (external, cli)"),
            Some("claude-cli")
        );
        assert_eq!(product_token("example-client"), Some("example-client"));
        assert_eq!(product_token("Mozilla/5.0"), Some("Mozilla"));
        assert_eq!(product_token("/x"), None);
        assert_eq!(product_token(""), None);
        assert_eq!(product_token("we<ird>/1"), None);
        assert_eq!(product_token(&"a".repeat(41)), None);
    }
}
