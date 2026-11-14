//! `services::pyyaml` (core, stable API: PORTING.md A-1, A-4, A-7): PyYAML 6.0.3 for every adapter, ONE engine.
//!
//! * Loading: `safe_load` / `compose` (`load.rs`, workstream C's function-by-function port of PyYAML's pure-Python reader,
//!   scanner, parser, composer, resolver and SafeConstructor, moved into core at the workstream C merge). Objects live in an
//!   arena (`services::pyobj::Doc`) like Python references: aliases share objects (no "billion laughs" blow-up), a
//!   recursive alias is a cycle, `!!omap`/`!!pairs` items are tuples, `.nan` is one shared float. Errors carry
//!   PyYAML's class, `str(e)`, the real `problem` / `context` attributes and the marks. Composition and construction
//!   are iterative and depth-guarded (Python's RecursionError, never a stack overflow).
//! * Tree values: [`safe_load_value`] converts a document to a [`PyValue`] tree for the adapters that walk plain
//!   values (a cycle, or aliases expanding past [`MAX_TREE_VALUES`], cannot become a tree: see PORTING.md D-12).
//! * Dumping: `safe_dump` (`dumper.rs`, `emitter.rs`, `resolver.rs`, `value.rs`, `py/`), the Agent Hub's exact
//!   SafeRepresenter / serializer / emitter, COPIED from a sibling project, Agent Hub's Rust port (commit a214dd6)
//!   (Nemo's changes are marked "Nemo": module paths, `py::float::repr` -> `services::pyfmt::float_repr`,
//!   `PyValue::Tuple`, the `DumpOpts::SAFE_DUMP*` sets). The hub's own loader is not used here: it reports the first
//!   line of `str(e)` as `problem` and has no tuples or cycles (reported back to the hub).
//!
//! Pinned by goldens generated with PyYAML itself: tests/core_pyyaml.rs (the hub corpus, parity/gen/
//! gen_yaml_golden.py), tests/evaluator_yaml_golden.rs (workstream C), tests/aiperf_pyyaml_golden.rs and
//! tests/wsb_core_pyyaml_shared.rs (workstream B, incl. `compose` key marks and `safe_dump` bytes), tests/dynamo_pyyaml.rs.

mod dumper;
mod emitter;
/// The loader (`safe_load`, `compose`) and its error type.
pub mod load;
mod pyobj;
/// Nemo: crate-visible so `services::pyfmt::str_repr` uses the exact `str.isprintable()` table.
pub(crate) mod pyunicode;
mod resolver;

/// Python helpers the dumper uses (copied with it).
pub mod py;
/// The tree value model (`PyValue`): what `safe_load_value` returns and `safe_dump` takes.
pub mod value;

pub use crate::services::pyobj::{Doc, Obj, ObjId, Py, PyExc, PyInt};
pub use load::{Composed, ComposedNode, Mark, NodeId, NodeKind, YamlError, compose, safe_load};
pub use value::{DateTime, PyValue};

/// Nemo: compile a regex from a constant pattern (the hub's `crate::re`): a pattern that does not compile is a
/// programming error caught by the tests; it falls back to a never-matching regex instead of panicking.
pub(crate) fn re(pattern: &str) -> regex::Regex {
    regex::Regex::new(pattern).unwrap_or_else(|_| never())
}

fn never() -> regex::Regex {
    regex::Regex::new(r"[^\s\S]")
        .unwrap_or_else(|_| regex::Regex::new("").unwrap_or_else(|_| never()))
}

/// The most values [`safe_load_value`] builds: aliases share objects in Python (and in [`Doc`]), but a tree copies
/// them, so an alias bomb would expand without bound.
pub const MAX_TREE_VALUES: usize = 1_000_000;

/// `yaml.safe_load(text)` as a [`PyValue`] tree (see [`to_value`]).
pub fn safe_load_value(text: &str) -> Result<PyValue, YamlError> {
    let doc = safe_load(text)?;
    to_value(&doc, &doc.root).map_err(YamlError::Py)
}

/// One object of a [`Doc`] as a [`PyValue`] tree. A cycle (a recursive alias) raises Python's RecursionError text
/// (Python would hand the cyclic object over), and so does nesting past `services::pyobj::MAX_DEPTH`; aliases that
/// expand past [`MAX_TREE_VALUES`] values raise MemoryError.
pub fn to_value(doc: &Doc, v: &Py) -> Result<PyValue, PyExc> {
    let mut budget = MAX_TREE_VALUES;
    let mut active = Vec::new();
    tree(doc, v, &mut active, &mut budget)
}

fn tree(doc: &Doc, v: &Py, active: &mut Vec<ObjId>, budget: &mut usize) -> Result<PyValue, PyExc> {
    if *budget == 0 {
        return Err(PyExc::new(
            "MemoryError",
            format!("more than {MAX_TREE_VALUES} values"),
        ));
    }
    *budget -= 1;
    Ok(match v {
        Py::None => PyValue::None,
        Py::Bool(b) => PyValue::Bool(*b),
        Py::Int(PyInt::Small(i)) => {
            i64::try_from(*i).map_or_else(|_| PyValue::BigInt(i.to_string()), PyValue::Int)
        }
        Py::Int(PyInt::Big(s)) => PyValue::BigInt(s.clone()),
        Py::Float(f) => PyValue::Float(*f),
        Py::Str(s) => PyValue::Str(s.clone()),
        Py::Bytes(b) => PyValue::Bytes(b.clone()),
        Py::Date(d) => PyValue::Date {
            year: d.year,
            month: u8::try_from(d.month).unwrap_or(0),
            day: u8::try_from(d.day).unwrap_or(0),
        },
        Py::DateTime(d) => PyValue::DateTime(DateTime {
            year: d.date.year,
            month: u8::try_from(d.date.month).unwrap_or(0),
            day: u8::try_from(d.date.day).unwrap_or(0),
            hour: u8::try_from(d.hour).unwrap_or(0),
            minute: u8::try_from(d.minute).unwrap_or(0),
            second: u8::try_from(d.second).unwrap_or(0),
            microsecond: d.micro,
            offset_s: d.tz,
        }),
        Py::Tuple(t) => PyValue::Tuple(
            t.iter()
                .map(|x| tree(doc, x, active, budget))
                .collect::<Result<_, _>>()?,
        ),
        Py::List(id) | Py::Dict(id) | Py::Set(id) => {
            if active.contains(id) || active.len() >= crate::services::pyobj::MAX_DEPTH {
                return Err(PyExc::recursion());
            }
            active.push(*id);
            let out = match doc.objs.get(*id) {
                Some(Obj::List(items)) => PyValue::List(
                    items
                        .iter()
                        .map(|x| tree(doc, x, active, budget))
                        .collect::<Result<_, _>>()?,
                ),
                Some(Obj::Set(items)) => PyValue::Set(
                    items
                        .iter()
                        .map(|x| tree(doc, x, active, budget))
                        .collect::<Result<_, _>>()?,
                ),
                Some(Obj::Dict(items)) => PyValue::Dict(
                    items
                        .iter()
                        .map(|(k, x)| {
                            Ok((tree(doc, k, active, budget)?, tree(doc, x, active, budget)?))
                        })
                        .collect::<Result<_, PyExc>>()?,
                ),
                None => PyValue::None,
            };
            active.pop();
            out
        }
    })
}

/// Nemo: `yaml.safe_dump(value, **opts)` (see [`DumpOpts::SAFE_DUMP`]).
#[must_use]
pub fn safe_dump(value: &PyValue, opts: &DumpOpts) -> String {
    dump(value, opts)
}

/// `yaml.safe_dump` keyword arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DumpOpts {
    pub sort_keys: bool,
    pub allow_unicode: bool,
    /// The hub always passes `default_flow_style=False` (block style; empty containers still print as `[]`/`{}`).
    pub default_flow_style: bool,
    pub width: usize,
}

impl DumpOpts {
    /// Nemo: `yaml.safe_dump(value)` with PyYAML's defaults (sort_keys=True, allow_unicode off, block style, width 80).
    pub const SAFE_DUMP: DumpOpts = DumpOpts {
        sort_keys: true,
        allow_unicode: false,
        default_flow_style: false,
        width: 80,
    };
    /// Nemo: `yaml.safe_dump(value, sort_keys=False)` (adapters/openshell.py).
    pub const SAFE_DUMP_UNSORTED: DumpOpts = DumpOpts {
        sort_keys: false,
        allow_unicode: false,
        default_flow_style: false,
        width: 80,
    };
}

/// `yaml.safe_dump(value, **opts)`, byte for byte (`default_flow_style: true` is PyYAML's `True`: flow style for every
/// collection). Not reproducible from a PyValue tree, by construction: PyYAML's `&id001`/`*id001` anchors for one Python
/// list/dict/date object reached twice (a PyValue has no identity), the hash order of a Python `set` (the PyValue
/// order is used), and `sort_keys` among NaN keys (a stable sort; CPython's timsort may differ).
pub fn dump(value: &PyValue, opts: &DumpOpts) -> String {
    let events = dumper::events(value, opts);
    emitter::Emitter::new(&events, opts.width, opts.allow_unicode).run()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deep documents load (or fail like Python) and dump inside a 2 MiB thread, tokio's default worker stack.
    #[test]
    fn deep_documents_fit_a_small_stack() {
        let run = || {
            let flow = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
            let maps = |n: usize| format!("{}1{}", "{a: ".repeat(n), "}".repeat(n));
            for doc in [flow(400), maps(400)] {
                let v = safe_load_value(&doc).unwrap_or_default();
                assert!(dump(&v, &DumpOpts::SAFE_DUMP).len() > 400);
            }
            for doc in [flow(200_000), maps(100_000)] {
                assert_eq!(
                    safe_load(&doc).err().map(|e| e.class()),
                    Some("RecursionError")
                );
            }
        };
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(run);
        assert!(handle.is_ok_and(|h| h.join().is_ok()));
    }

    #[test]
    fn cycles_and_alias_bombs_do_not_become_trees() {
        assert_eq!(
            safe_load_value("&a [*a]").err().map(|e| e.class()),
            Some("RecursionError")
        );
        let mut bomb = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..9 {
            bomb.push_str(&format!(
                "{}: &{} [*{}, *{}, *{}, *{}, *{}, *{}, *{}, *{}, *{}, *{}]\n",
                (b'a' + i) as char,
                (b'a' + i) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char,
                (b'a' + i - 1) as char
            ));
        }
        assert!(safe_load(&bomb).is_ok());
        assert_eq!(
            safe_load_value(&bomb).err().map(|e| e.class()),
            Some("MemoryError")
        );
        assert_eq!(
            safe_load_value("!!omap [a: 1, b: 2]")
                .ok()
                .map(|v| v.repr()),
            Some("[('a', 1), ('b', 2)]".to_owned())
        );
    }
}
