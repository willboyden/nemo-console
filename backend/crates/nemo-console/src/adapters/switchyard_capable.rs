//! `capable_models` from `adapters/switchyard.py`, owned by CORE because the core's capable-tier check
//! (services/engines.rs) needs it too. Core contract. Reads a `services::pytoml` document (CPython 3.13 `tomllib`), so the
//! routes files it accepts and refuses are exactly Python's.

use indexmap::IndexMap;

use crate::services::pytoml::{self, Table, TomlVal};

/// Python's `_dict(x)`: `x` when it is a dict, else `{}`.
fn dict(v: Option<&TomlVal>) -> Option<&Table> {
    v.and_then(TomlVal::as_table)
}

/// Parsed routes.toml -> {capable-tier model id: [route ids]}. stage_router routes only.
///
/// `capable_target` names a `[targets.<key>]` table and the model id is that table's `id` (a router alias).
/// Passthroughs (direct-*) and targets no route escalates to are NOT capable tiers. Never fails on a malformed doc.
#[must_use]
pub fn capable_models(cfg: &Table) -> IndexMap<String, Vec<String>> {
    let mut out: IndexMap<String, Vec<String>> = IndexMap::new();
    let targets = dict(cfg.get("targets"));
    let Some(routes) = dict(cfg.get("routes")) else {
        return out;
    };
    for (key, route) in routes {
        let Some(route) = route.as_table() else {
            continue;
        };
        if route.get("type").and_then(TomlVal::as_str) != Some("stage_router") {
            continue;
        }
        let tgt = route
            .get("capable_target")
            .and_then(TomlVal::as_str)
            .and_then(|r| targets.and_then(|t| t.get(r)));
        if let Some(t) = tgt.and_then(TomlVal::as_table)
            && let Some(id) = t.get("id").and_then(TomlVal::as_str)
        {
            let rid = route.get("id").map_or_else(|| key.clone(), TomlVal::py_str);
            out.entry(id.to_owned()).or_default().push(rid);
        }
    }
    out
}

/// `tomllib.loads(text)` then [`capable_models`]; `None` when the document does not parse.
#[must_use]
pub fn capable_models_text(text: &str) -> Option<IndexMap<String, Vec<String>>> {
    pytoml::loads(text).ok().map(|t| capable_models(&t))
}
