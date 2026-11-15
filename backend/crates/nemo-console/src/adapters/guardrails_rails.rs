//! The rail pipeline of each NeMo Guardrails config (addendum 9.16, `GET /systems/guardrails/rails`, Rust-only,
//! PORTING.md D-42). A pure function over the config files the guardrails adapter already reads (`<cfg>/config.yml`,
//! else `<cfg>/config.yaml`): the main model, the input / output / retrieval flow NAMES and the output streaming
//! settings. Nothing else: no prompt text, no `.co` content, no parameters, no URL.
//!
//! Bounds (the wave-1..3 audits, applied from the start):
//!   * every file is opened relative to an `O_NOFOLLOW` directory descriptor (`runviz::RunDir`): no symlink, no
//!     hard link, only regular files, at most [`MAX_CONFIG_BYTES`] (larger reads as unreadable);
//!   * the YAML is COMPOSED, never constructed: aliases point at shared nodes, so an alias bomb or a large aliased
//!     scalar costs nothing (constructing would copy the scalar into every alias), and only the fixed key path below
//!     is walked. A key lookup scans at most [`MAP_SCAN`] entries of a mapping and the `models` list at most
//!     [`MODELS_SCAN`] distinct items, so aliasing one big mapping many times (`models: [*m, *m, ...]`) cannot make
//!     the walk quadratic: the work per config is bounded by a constant plus one pass over each flow list;
//!   * flow names and the main model also pass `redact()` unchanged (a credential-looking value is dropped);
//!   * flow names pass a full-match check (letters, digits, space, `_-.`, at most 80 characters) or are dropped and
//!     counted; at most [`RAILS_MAX_FLOWS`] per list and [`RAILS_MAX_CONFIGS`] configs (the rest counted);
//!   * the main model passes the adapters' model-alias check or is dropped (counted);
//!   * it runs on the blocking pool (`runviz::blocking`), never on the async workers.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::adapters::runviz::RunDir;
use crate::services::pyyaml::{Composed, NodeId, NodeKind, compose};
use crate::services::support::full;

/// The largest config file read (= the adapter's `MAX_CONFIG_BYTES`).
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;
/// At most this many configs are answered (ordered by id; the rest counted in `configs.capped`).
pub const RAILS_MAX_CONFIGS: usize = 64;
/// At most this many flow names per list (the rest counted in `flows.capped`).
pub const RAILS_MAX_FLOWS: usize = 64;
/// The longest flow name kept.
pub const FLOW_MAX_CHARS: usize = 80;
/// A key lookup looks at no more than this many entries of a mapping (a key past them is not found).
pub const MAP_SCAN: usize = 256;
/// The `models` list: no more than this many items are looked at (an aliased item seen before is skipped).
pub const MODELS_SCAN: usize = 32;
/// The config directory: no more than this many entries are looked at.
pub const CONFIG_DIR_SCAN: usize = 4096;

const STR_TAG: &str = "tag:yaml.org,2002:str";
const BOOL_TAG: &str = "tag:yaml.org,2002:bool";
const INT_TAG: &str = "tag:yaml.org,2002:int";

/// A flow name: letters, digits, space, `_-.`, 1..80 characters.
static FLOW_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9 _.\-]{1,80}"));
/// A router model alias (= the adapters' `MODEL_RE`).
static MODEL_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,127}"));
/// A config directory name (= the adapter's `CONFIG_DIR_RE`).
static CONFIG_DIR_RE: LazyLock<Regex> = LazyLock::new(|| full(r"[a-z0-9][a-z0-9_\-]{0,31}"));

/// Problems counted while reading, per config (`None` = not about one config).
#[derive(Default)]
struct Problems(Vec<(Option<String>, &'static str, usize)>);

impl Problems {
    fn add(&mut self, config: Option<&str>, code: &'static str, n: usize) {
        if n == 0 {
            return;
        }
        if let Some(e) = self
            .0
            .iter_mut()
            .find(|(c, k, _)| c.as_deref() == config && *k == code)
        {
            e.2 += n;
        } else {
            self.0.push((config.map(str::to_owned), code, n));
        }
    }

    fn to_value(&self) -> Value {
        Value::Array(
            self.0
                .iter()
                .map(|(c, code, n)| json!({"config": c, "code": code, "count": n}))
                .collect(),
        )
    }
}

/// The value node of `key` in the mapping `node` (the last one when a key repeats, as PyYAML's dict does).
fn map_get(c: &Composed, node: NodeId, key: &str) -> Option<NodeId> {
    let NodeKind::Map(pairs) = &c.node(node)?.kind else {
        return None;
    };
    let mut found = None;
    for (k, v) in pairs.iter().take(MAP_SCAN) {
        if let Some(n) = c.node(*k)
            && n.tag == STR_TAG
            && matches!(&n.kind, NodeKind::Scalar(s) if s == key)
        {
            found = Some(*v);
        }
    }
    found
}

fn path(c: &Composed, mut node: NodeId, keys: &[&str]) -> Option<NodeId> {
    for k in keys {
        node = map_get(c, node, k)?;
    }
    Some(node)
}

fn str_scalar(c: &Composed, node: NodeId) -> Option<&str> {
    let n = c.node(node)?;
    match &n.kind {
        NodeKind::Scalar(s) if n.tag == STR_TAG => Some(s),
        _ => None,
    }
}

/// A YAML 1.1 bool as PyYAML resolves it (`yes/no/true/false/on/off`, any case form it accepts).
fn bool_scalar(c: &Composed, node: NodeId) -> Option<bool> {
    let n = c.node(node)?;
    match &n.kind {
        NodeKind::Scalar(s) if n.tag == BOOL_TAG => match s.to_ascii_lowercase().as_str() {
            "yes" | "true" | "on" => Some(true),
            "no" | "false" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A plain decimal int from 0 to 1,000,000,000 (other forms and other values: null).
fn int_scalar(c: &Composed, node: NodeId) -> Option<i64> {
    let n = c.node(node)?;
    let NodeKind::Scalar(s) = &n.kind else {
        return None;
    };
    if n.tag != INT_TAG {
        return None;
    }
    let t: String = s.chars().filter(|ch| *ch != '_').collect();
    let t = t.strip_prefix('+').unwrap_or(&t);
    if t.is_empty() || t.len() > 10 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok().filter(|v| *v <= 1_000_000_000)
}

/// True when `redact()` leaves the value as it is (nothing in it looks like a credential).
fn unredacted(s: &str) -> bool {
    crate::services::common::redact(s) == s
}

/// The flow names of `rails.<kind>.flows`: checked, capped, the rest counted.
fn flows(c: &Composed, root: NodeId, kind: &str, cfg: &str, p: &mut Problems) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(list) = path(c, root, &["rails", kind, "flows"]) else {
        return out;
    };
    let Some(NodeKind::Seq(items)) = c.node(list).map(|n| &n.kind) else {
        return out;
    };
    let (mut dropped, mut capped) = (0usize, 0usize);
    for item in items {
        match str_scalar(c, *item) {
            Some(name) if FLOW_RE.is_match(name) && unredacted(name) => {
                if out.len() < RAILS_MAX_FLOWS {
                    out.push(Value::String(name.to_owned()));
                } else {
                    capped += 1;
                }
            }
            _ => dropped += 1,
        }
    }
    p.add(Some(cfg), "flows.dropped", dropped);
    p.add(Some(cfg), "flows.capped", capped);
    out
}

/// The `type: main` model of `models` (the first one, as the adapter's `main_model()` picks it).
fn main_model(c: &Composed, root: NodeId, cfg: &str, p: &mut Problems) -> Value {
    let Some(models) = map_get(c, root, "models") else {
        return Value::Null;
    };
    let Some(NodeKind::Seq(items)) = c.node(models).map(|n| &n.kind) else {
        return Value::Null;
    };
    let mut seen: Vec<NodeId> = Vec::new();
    for m in items {
        if seen.contains(m) {
            continue; // an alias of an item already looked at
        }
        if seen.len() == MODELS_SCAN {
            break;
        }
        seen.push(*m);
        if map_get(c, *m, "type").and_then(|t| str_scalar(c, t)) != Some("main") {
            continue;
        }
        let Some(model) = map_get(c, *m, "model").and_then(|n| str_scalar(c, n)) else {
            continue;
        };
        if MODEL_RE.is_match(model) && unredacted(model) {
            return Value::String(model.to_owned());
        }
        p.add(Some(cfg), "main_model.dropped", 1);
        return Value::Null;
    }
    Value::Null
}

/// `rails.output.streaming` (nemoguardrails 0.24) as `{enabled, chunk_size, context_size}`; a top-level boolean
/// `streaming` (older configs) as `{enabled, chunk_size: null, context_size: null}`; else null.
fn streaming(c: &Composed, root: NodeId) -> Value {
    if let Some(s) = path(c, root, &["rails", "output", "streaming"])
        && matches!(c.node(s).map(|n| &n.kind), Some(NodeKind::Map(_)))
    {
        let num = |k: &str| map_get(c, s, k).and_then(|n| int_scalar(c, n));
        return json!({
            "enabled": map_get(c, s, "enabled").and_then(|n| bool_scalar(c, n)),
            "chunk_size": num("chunk_size"),
            "context_size": num("context_size"),
        });
    }
    if let Some(s) = map_get(c, root, "streaming")
        && let Some(b) = bool_scalar(c, s)
    {
        return json!({"enabled": b, "chunk_size": null, "context_size": null});
    }
    Value::Null
}

/// The rails of one config from its YAML text (`None` with a problem code when it cannot be read).
fn one(text: &str, cfg: &str, p: &mut Problems) -> Value {
    let Ok(c) = compose(text) else {
        p.add(Some(cfg), "config.yaml_error", 1);
        return empty(cfg);
    };
    let Some(root) = c.root else {
        return empty(cfg);
    };
    if !matches!(c.node(root).map(|n| &n.kind), Some(NodeKind::Map(_))) {
        p.add(Some(cfg), "config.not_a_mapping", 1);
        return empty(cfg);
    }
    json!({
        "id": cfg,
        "main_model": main_model(&c, root, cfg, p),
        "input": flows(&c, root, "input", cfg, p),
        "output": flows(&c, root, "output", cfg, p),
        "streaming": streaming(&c, root),
        "retrieval": flows(&c, root, "retrieval", cfg, p),
    })
}

fn empty(cfg: &str) -> Value {
    json!({"id": cfg, "main_model": null, "input": [], "output": [], "streaming": null, "retrieval": []})
}

/// Read `<cfg>/config.yml` (else `config.yaml`) of one config directory: the text, or the problem code.
fn read_config(root: &RunDir, cfg: &str) -> Result<String, &'static str> {
    let Some(dir) = root.sub(cfg) else {
        return Err("config.unreadable");
    };
    for name in ["config.yml", "config.yaml"] {
        let (f, len) = match dir.file(name) {
            Ok(x) => x,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("config.unreadable"),
        };
        if len > MAX_CONFIG_BYTES {
            return Err("config.too_large");
        }
        let mut buf = Vec::new();
        if std::io::Read::read_to_end(&mut std::io::Read::take(f, MAX_CONFIG_BYTES + 1), &mut buf)
            .is_err()
        {
            return Err("config.unreadable");
        }
        if buf.len() as u64 > MAX_CONFIG_BYTES {
            return Err("config.too_large");
        }
        return String::from_utf8(buf).map_err(|_| "config.not_utf8");
    }
    Err("config.unreadable")
}

/// The config ids the adapter lists (`GuardrailsAdapter::config_ids`): directories (not symlinks) whose name passes
/// the adapter's check and that hold a `config.yml` or `config.yaml`, sorted.
#[must_use]
pub fn config_ids_in(root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = rd
        .take(CONFIG_DIR_SCAN)
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && !p.is_symlink()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| CONFIG_DIR_RE.is_match(n))
                && (p.join("config.yml").is_file() || p.join("config.yaml").is_file())
        })
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
        .collect();
    ids.sort();
    ids
}

/// `{configs: [{id, main_model, input, output, streaming, retrieval}], problems: [{config, code, count}]}` for the
/// config directory `root` (None: no directory configured -> no configs). Blocking: call it on the blocking pool.
#[must_use]
pub fn rails_answer(root: Option<&Path>) -> Value {
    let mut p = Problems::default();
    let mut configs = Vec::new();
    if let Some(root) = root {
        let ids = config_ids_in(root);
        p.add(
            None,
            "configs.capped",
            ids.len().saturating_sub(RAILS_MAX_CONFIGS),
        );
        let dir = RunDir::open(root);
        for cfg in ids.iter().take(RAILS_MAX_CONFIGS) {
            let text = match &dir {
                Some(d) => read_config(d, cfg),
                None => Err("config.unreadable"),
            };
            match text {
                Ok(t) => configs.push(one(&t, cfg, &mut p)),
                Err(code) => {
                    p.add(Some(cfg), code, 1);
                    configs.push(empty(cfg));
                }
            }
        }
    }
    json!({"configs": configs, "problems": p.to_value()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rails(text: &str) -> (Value, Value) {
        let mut p = Problems::default();
        let v = one(text, "c", &mut p);
        (v, p.to_value())
    }

    #[test]
    fn reads_the_pipeline() {
        let (v, p) = rails(
            "models:\n  - type: embeddings\n    model: e\n  - type: main\n    model: org/m-1\n\
             rails:\n  input:\n    flows: [self check input, 'bad/flow']\n  output:\n    flows:\n      - a\n    \
             streaming:\n      enabled: yes\n      chunk_size: 2_00\n      context_size: 0x10\n",
        );
        assert_eq!(v["main_model"], "org/m-1");
        assert_eq!(v["input"], json!(["self check input"]));
        assert_eq!(v["output"], json!(["a"]));
        assert_eq!(
            v["streaming"],
            json!({"enabled": true, "chunk_size": 200, "context_size": null})
        );
        assert_eq!(
            p,
            json!([{"config": "c", "code": "flows.dropped", "count": 1}])
        );
    }

    #[test]
    fn aliases_are_not_expanded() {
        let big = "x".repeat(100_000);
        let mut text = format!("a: &big {big}\nrails:\n  input:\n    flows: [");
        for _ in 0..20_000 {
            text.push_str("*big,");
        }
        text.push_str("ok]\n");
        let (v, p) = rails(&text);
        assert_eq!(v["input"], json!(["ok"]));
        assert_eq!(p[0]["count"], 20_000);
    }

    #[test]
    fn old_streaming_flag_and_odd_shapes() {
        assert_eq!(
            rails("streaming: true\n").0["streaming"],
            json!({"enabled": true, "chunk_size": null, "context_size": null})
        );
        assert_eq!(rails("- a\n").1[0]["code"], "config.not_a_mapping");
        assert_eq!(rails("a: [\n").1[0]["code"], "config.yaml_error");
        assert_eq!(rails("").0["input"], json!([]));
    }
}
