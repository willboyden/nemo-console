//! PyYAML 6.0.3 `representer.py` (`SafeRepresenter`) + `serializer.py` for a [`PyValue`]: the node tree is walked
//! with an explicit stack (no recursion, so any depth is safe) and turned straight into serializer events with
//! PyYAML's tags, implicit flags (`resolve()` against the YAML 1.1 regexes) and styles.

use base64::Engine as _;

use super::DumpOpts;
use super::emitter::DEv;
use super::pyobj;
use super::resolver::{self as tags, resolve_scalar};
use crate::services::pyyaml::value::PyValue;

/// `SafeRepresenter.represent_float`.
fn float_text(f: f64) -> String {
    if f.is_nan() {
        return ".nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            ".inf".into()
        } else {
            "-.inf".into()
        };
    }
    let value = repr_f64(f).to_lowercase();
    if !value.contains('.') && value.contains('e') {
        value.replacen('e', ".0e", 1)
    } else {
        value
    }
}

/// Mantissa digits (no dot, no trailing zeros) and decimal exponent of Rust's `{:e}` / `{:.Ne}` output.
fn sci_digits(s: &str) -> (String, i32) {
    let (mant, exp) = s.split_once('e').unwrap_or((s, "0"));
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    (
        if digits.is_empty() {
            "0".into()
        } else {
            digits.into()
        },
        exp.parse::<i32>().unwrap_or(0) + 1,
    )
}

/// `repr(float)` for a finite float. Same layout as `crate::services::pyyaml::py::float::repr`, but the digits are CPython's
/// (`_Py_dg_dtoa` mode 0): among the shortest strings that round-trip, the one nearest the exact value, ties to EVEN.
/// Rust's `{:e}` breaks such ties upwards (`-1133092563735992.25` -> `...992.3`, CPython `...992.2`), so the
/// correctly rounded string of the same length (`{:.Ne}`, half-even) wins whenever it also round-trips.
/// (Reported to CORE: `py::float::repr` has the same tie issue.)
fn repr_f64(x: f64) -> String {
    let a = x.abs();
    if a == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let (mut digits, mut decpt) = sci_digits(&format!("{a:e}"));
    let n = digits.len();
    let alt = format!("{a:.prec$e}", prec = n.saturating_sub(1));
    if alt.parse::<f64>().ok() == Some(a) {
        (digits, decpt) = sci_digits(&alt);
    }
    let sign = if x.is_sign_negative() { "-" } else { "" };
    let nd = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    if !(-3..=16).contains(&decpt) {
        let e = decpt - 1;
        let mant = if digits.len() == 1 {
            digits
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!(
            "{sign}{mant}e{}{:02}",
            if e < 0 { '-' } else { '+' },
            e.abs()
        )
    } else if decpt <= 0 {
        format!(
            "{sign}0.{}{digits}",
            "0".repeat(usize::try_from(-decpt).unwrap_or(0))
        )
    } else if decpt >= nd {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(usize::try_from(decpt - nd).unwrap_or(0))
        )
    } else {
        let k = usize::try_from(decpt).unwrap_or(0);
        format!("{sign}{}.{}", &digits[..k], &digits[k..])
    }
}

/// `base64.encodebytes(data).decode('ascii')`: 76-character lines, each ending in a newline.
fn encodebytes(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(57) {
        out.push_str(&base64::engine::general_purpose::STANDARD.encode(chunk));
        out.push('\n');
    }
    out
}

fn scalar(tag: &str, value: String, style: Option<char>) -> DEv {
    let detected = resolve_scalar(&value, true);
    DEv::Scalar {
        implicit: (detected == tag, tag == tags::STR),
        tag: tag.to_string(),
        value,
        style,
    }
}

enum Work<'a> {
    Value(&'a PyValue),
    /// A `None` value of a `!!set` entry.
    Null,
    Event(DEv),
}

/// The serializer's event stream for one document.
pub(crate) fn events(value: &PyValue, opts: &DumpOpts) -> Vec<DEv> {
    let mut out = vec![DEv::StreamStart, DEv::DocumentStart];
    let mut stack: Vec<Work<'_>> = vec![Work::Value(value)];
    while let Some(w) = stack.pop() {
        let v = match w {
            Work::Event(e) => {
                out.push(e);
                continue;
            }
            Work::Null => {
                out.push(scalar(tags::NULL, "null".into(), None));
                continue;
            }
            Work::Value(v) => v,
        };
        match v {
            PyValue::None => out.push(scalar(tags::NULL, "null".into(), None)),
            PyValue::Bool(b) => out.push(scalar(
                tags::BOOL,
                if *b { "true" } else { "false" }.into(),
                None,
            )),
            PyValue::Int(i) => out.push(scalar(tags::INT, i.to_string(), None)),
            PyValue::BigInt(s) => out.push(scalar(tags::INT, s.clone(), None)),
            PyValue::Float(f) => out.push(scalar(tags::FLOAT, float_text(*f), None)),
            PyValue::Str(s) => out.push(scalar(tags::STR, s.clone(), None)),
            PyValue::Bytes(b) => out.push(scalar(tags::BINARY, encodebytes(b), Some('|'))),
            PyValue::Date { .. } => out.push(scalar(
                tags::TIMESTAMP,
                v.isoformat().unwrap_or_default(),
                None,
            )),
            PyValue::DateTime(_) => out.push(scalar(
                tags::TIMESTAMP,
                v.isoformat().unwrap_or_default().replacen('T', " ", 1),
                None,
            )),
            // SafeRepresenter.add_representer(tuple, represent_list)
            PyValue::List(items) | PyValue::Tuple(items) => {
                out.push(DEv::SequenceStart {
                    tag: tags::SEQ.into(),
                    implicit: true,
                    flow_style: opts.default_flow_style,
                });
                stack.push(Work::Event(DEv::SequenceEnd));
                for item in items.iter().rev() {
                    stack.push(Work::Value(item));
                }
            }
            PyValue::Dict(pairs) => {
                out.push(DEv::MappingStart {
                    tag: tags::MAP.into(),
                    implicit: true,
                    flow_style: opts.default_flow_style,
                });
                stack.push(Work::Event(DEv::MappingEnd));
                let mut items: Vec<&(PyValue, PyValue)> = pairs.iter().collect();
                if opts.sort_keys {
                    pyobj::sort_items(&mut items, |i| &i.0);
                }
                for (k, val) in items.into_iter().rev() {
                    stack.push(Work::Value(val));
                    stack.push(Work::Value(k));
                }
            }
            PyValue::Set(keys) => {
                out.push(DEv::MappingStart {
                    tag: tags::SET.into(),
                    implicit: false,
                    flow_style: opts.default_flow_style,
                });
                stack.push(Work::Event(DEv::MappingEnd));
                let mut items: Vec<&PyValue> = keys.iter().collect();
                if opts.sort_keys {
                    pyobj::sort_items(&mut items, |k| *k);
                }
                for k in items.into_iter().rev() {
                    stack.push(Work::Null);
                    stack.push(Work::Value(k));
                }
            }
        }
    }
    out.push(DEv::DocumentEnd);
    out.push(DEv::StreamEnd);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_like_cpython() {
        for (x, want) in [
            (-1_133_092_563_735_992.2, "-1133092563735992.2"),
            (1_125_899_906_842_624.2, "1125899906842624.2"),
            (1_125_899_906_842_624.8, "1125899906842624.8"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1e22, "1e+22"),
            (123_456_789.123, "123456789.123"),
            (5e-324, "5e-324"),
            (1e-4, "0.0001"),
            (1e-5, "1e-05"),
            (-0.0, "-0.0"),
            (9_007_199_254_740_992.0, "9007199254740992.0"),
        ] {
            assert_eq!(repr_f64(x), want);
        }
    }
}
