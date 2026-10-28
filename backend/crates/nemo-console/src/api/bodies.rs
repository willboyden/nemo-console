//! Request bodies and query parameters with FastAPI/pydantic validation semantics (core). Each Python request model
//! is a struct plus a validator that reports pydantic's errors in pydantic's order (fields in definition order,
//! then forbidden extra keys in input order). Pinned by tests/fixtures/api_golden.json.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value};

use crate::api::pyjson::PyJson;
use crate::services::pyfmt::char_len;
use crate::services::pyvalidate::{
    Loc, MSG_DICT, MSG_EXTRA, MSG_LIST, MSG_MISSING, MSG_MODEL, MSG_STR, PyError, lax_bool,
    lax_float, lax_int, msg_ge, msg_le, msg_pattern,
};

/// The parsed body as FastAPI hands it to validation: absent (empty body), JSON, or raw bytes (non-JSON type).
#[derive(Debug, Clone)]
pub enum RawBody {
    Absent,
    Json(PyJson),
    Bytes,
}

fn pyjson_to_lax(v: &PyJson) -> Value {
    // the lax coercions only look at the JSON kind and scalar value
    match v {
        PyJson::Float(f) if !f.is_finite() => Value::String(String::new()),
        other => other.to_value(),
    }
}

/// Validation context: collects errors under `base` (e.g. ["body"]).
pub struct V {
    pub errors: Vec<PyError>,
}

impl Default for V {
    fn default() -> Self {
        V::new()
    }
}

impl V {
    #[must_use]
    pub fn new() -> Self {
        V { errors: Vec::new() }
    }

    fn push(&mut self, loc: &[Loc], kind: &'static str, msg: impl Into<String>) {
        self.errors.push(PyError::new(loc.to_vec(), kind, msg));
    }

    fn str_(&mut self, loc: &[Loc], v: &PyJson) -> Option<String> {
        match v {
            PyJson::Str(s) => Some(s.clone()),
            _ => {
                self.push(loc, "string_type", MSG_STR);
                None
            }
        }
    }

    fn bool_(&mut self, loc: &[Loc], v: &PyJson) -> Option<bool> {
        if let PyJson::Float(f) = v
            && !f.is_finite()
        {
            self.push(loc, "bool_type", crate::services::pyvalidate::MSG_BOOL);
            return None;
        }
        match lax_bool(&pyjson_to_lax(v)) {
            Ok(b) => Some(b),
            Err((kind, msg)) => {
                self.push(loc, kind, msg);
                None
            }
        }
    }

    fn int_(&mut self, loc: &[Loc], v: &PyJson) -> Option<i64> {
        if let PyJson::Int(i) = v {
            return i64::try_from(*i).ok().or_else(|| {
                self.push(
                    loc,
                    "int_parsing_size",
                    "Unable to parse input string as an integer, exceeded maximum size",
                );
                None
            });
        }
        if let PyJson::Float(f) = v
            && !f.is_finite()
        {
            self.push(loc, "finite_number", "Input should be a finite number");
            return None;
        }
        match lax_int(&pyjson_to_lax(v)) {
            Ok(i) => Some(i),
            Err((kind, msg)) => {
                self.push(loc, kind, msg);
                None
            }
        }
    }

    fn float_(&mut self, loc: &[Loc], v: &PyJson) -> Option<f64> {
        if let PyJson::Float(f) = v {
            return Some(*f);
        }
        match lax_float(&pyjson_to_lax(v)) {
            Ok(f) => Some(f),
            Err((kind, msg)) => {
                self.push(loc, kind, msg);
                None
            }
        }
    }

    fn dict_any(&mut self, loc: &[Loc], v: &PyJson) -> Option<Map<String, Value>> {
        match v {
            PyJson::Dict(m) => {
                let mut out = Map::new();
                for (k, x) in m {
                    out.insert(k.clone(), x.to_value());
                }
                Some(out)
            }
            _ => {
                self.push(loc, "dict_type", MSG_DICT);
                None
            }
        }
    }

    fn dict_str(&mut self, loc: &[Loc], v: &PyJson) -> Option<IndexMap<String, String>> {
        let PyJson::Dict(m) = v else {
            self.push(loc, "dict_type", MSG_DICT);
            return None;
        };
        let mut out = IndexMap::new();
        let mut ok = true;
        for (k, x) in m {
            let mut l = loc.to_vec();
            l.push(Loc::Key(k.clone()));
            match self.str_(&l, x) {
                Some(s) => {
                    out.insert(k.clone(), s);
                }
                None => ok = false,
            }
        }
        ok.then_some(out)
    }

    fn pattern(&mut self, loc: &[Loc], s: &str, rx: &Regex, pat: &str) -> bool {
        if rx.is_match(s) {
            true
        } else {
            self.push(loc, "string_pattern_mismatch", msg_pattern(pat));
            false
        }
    }
}

/// The model wrapper: `body` must be an object; returns its fields or records the model error.
fn model<'a>(v: &mut V, raw: &'a RawBody) -> Option<&'a IndexMap<String, PyJson>> {
    let body = vec![Loc::Key("body".to_owned())];
    match raw {
        RawBody::Absent | RawBody::Json(PyJson::Null) => {
            v.push(&body, "missing", MSG_MISSING);
            None
        }
        RawBody::Json(PyJson::Dict(m)) => Some(m),
        RawBody::Json(_) | RawBody::Bytes => {
            v.push(&body, "model_attributes_type", MSG_MODEL);
            None
        }
    }
}

fn at(parts: &[&str]) -> Vec<Loc> {
    parts.iter().map(|p| Loc::Key((*p).to_owned())).collect()
}

fn extras(v: &mut V, m: &IndexMap<String, PyJson>, known: &[&str], base: &[Loc]) {
    for k in m.keys() {
        if !known.contains(&k.as_str()) {
            let mut l = base.to_vec();
            l.push(Loc::Key(k.clone()));
            v.push(&l, "extra_forbidden", MSG_EXTRA);
        }
    }
}

#[allow(clippy::expect_used)]
fn rx(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

static SHA_RE: LazyLock<Regex> = LazyLock::new(|| rx(r"^[0-9a-f]{64}$"));
static REV_RE: LazyLock<Regex> = LazyLock::new(|| rx(r"^[0-9a-f]{8,32}$"));
static ROLE_RE: LazyLock<Regex> = LazyLock::new(|| rx(r"^(system|user|assistant|tool)$"));

/// A required field: `Some(value)` or a `missing` error.
fn req<'a>(v: &mut V, m: &'a IndexMap<String, PyJson>, name: &str) -> Option<&'a PyJson> {
    let r = m.get(name);
    if r.is_none() {
        v.push(&at(&["body", name]), "missing", MSG_MISSING);
    }
    r
}

// ── models ──────────────────────────────────────────────────────────────────────────────────────

pub struct ValidateBody {
    pub content: String,
}

pub fn validate_body(raw: &RawBody, v: &mut V) -> Option<ValidateBody> {
    let m = model(v, raw)?;
    let content = req(v, m, "content").and_then(|x| v.str_(&at(&["body", "content"]), x));
    extras(v, m, &["content"], &at(&["body"]));
    Some(ValidateBody { content: content? })
}

pub struct ApplyBody {
    pub content: String,
    pub base_sha256: String,
    pub restart: bool,
    pub confirm: bool,
}

pub fn apply_body(raw: &RawBody, v: &mut V) -> Option<ApplyBody> {
    let m = model(v, raw)?;
    let content = req(v, m, "content").and_then(|x| v.str_(&at(&["body", "content"]), x));
    let sha_loc = at(&["body", "base_sha256"]);
    let sha = req(v, m, "base_sha256")
        .and_then(|x| v.str_(&sha_loc, x))
        .filter(|s| v.pattern(&sha_loc, s, &SHA_RE, "^[0-9a-f]{64}$"));
    let restart = m
        .get("restart")
        .map_or(Some(true), |x| v.bool_(&at(&["body", "restart"]), x));
    let confirm = m
        .get("confirm")
        .map_or(Some(false), |x| v.bool_(&at(&["body", "confirm"]), x));
    extras(
        v,
        m,
        &["content", "base_sha256", "restart", "confirm"],
        &at(&["body"]),
    );
    Some(ApplyBody {
        content: content?,
        base_sha256: sha?,
        restart: restart?,
        confirm: confirm?,
    })
}

pub struct RollbackBody {
    pub revision_id: String,
    pub confirm: bool,
}

pub fn rollback_body(raw: &RawBody, v: &mut V) -> Option<RollbackBody> {
    let m = model(v, raw)?;
    let loc = at(&["body", "revision_id"]);
    let rid = req(v, m, "revision_id")
        .and_then(|x| v.str_(&loc, x))
        .filter(|s| v.pattern(&loc, s, &REV_RE, "^[0-9a-f]{8,32}$"));
    let confirm = m
        .get("confirm")
        .map_or(Some(false), |x| v.bool_(&at(&["body", "confirm"]), x));
    extras(v, m, &["revision_id", "confirm"], &at(&["body"]));
    Some(RollbackBody {
        revision_id: rid?,
        confirm: confirm?,
    })
}

pub struct ParamsBody {
    pub values: Map<String, Value>,
    pub confirm: bool,
}

/// `ParamsBody` (`with_confirm` false) and `ParamsPutBody` (true).
pub fn params_body(raw: &RawBody, v: &mut V, with_confirm: bool) -> Option<ParamsBody> {
    let m = model(v, raw)?;
    let values = m.get("values").map_or(Some(Map::new()), |x| {
        v.dict_any(&at(&["body", "values"]), x)
    });
    let confirm = if with_confirm {
        m.get("confirm")
            .map_or(Some(false), |x| v.bool_(&at(&["body", "confirm"]), x))
    } else {
        Some(false)
    };
    let known: &[&str] = if with_confirm {
        &["values", "confirm"]
    } else {
        &["values"]
    };
    extras(v, m, known, &at(&["body"]));
    Some(ParamsBody {
        values: values?,
        confirm: confirm?,
    })
}

pub struct ActionBody {
    pub params: Map<String, Value>,
    pub confirm: bool,
}

pub fn action_body(raw: &RawBody, v: &mut V) -> Option<ActionBody> {
    let m = model(v, raw)?;
    let params = m.get("params").map_or(Some(Map::new()), |x| {
        v.dict_any(&at(&["body", "params"]), x)
    });
    let confirm = m
        .get("confirm")
        .map_or(Some(false), |x| v.bool_(&at(&["body", "confirm"]), x));
    extras(v, m, &["params", "confirm"], &at(&["body"]));
    Some(ActionBody {
        params: params?,
        confirm: confirm?,
    })
}

pub struct ChatBody {
    pub model: String,
    /// `[{role, content}]` as validated (content: str or list of objects).
    pub messages: Vec<Value>,
    pub max_tokens: Option<i64>,
    pub stream: bool,
    pub system: Option<String>,
}

impl ChatBody {
    /// `body.model_dump(exclude_none=True)`, keys in field order.
    #[must_use]
    pub fn dump(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("model".to_owned(), Value::String(self.model.clone()));
        m.insert("messages".to_owned(), Value::Array(self.messages.clone()));
        if let Some(mt) = self.max_tokens {
            m.insert("max_tokens".to_owned(), Value::from(mt));
        }
        m.insert("stream".to_owned(), Value::Bool(self.stream));
        if let Some(s) = &self.system {
            m.insert("system".to_owned(), Value::String(s.clone()));
        }
        m
    }
}

fn chat_message(v: &mut V, loc: &[Loc], x: &PyJson) -> Option<Value> {
    let PyJson::Dict(m) = x else {
        v.push(loc, "model_attributes_type", MSG_MODEL);
        return None;
    };
    let mut l_role = loc.to_vec();
    l_role.push(Loc::Key("role".to_owned()));
    let role = match m.get("role") {
        None => {
            v.push(&l_role, "missing", MSG_MISSING);
            None
        }
        Some(r) => v
            .str_(&l_role, r)
            .filter(|s| v.pattern(&l_role, s, &ROLE_RE, "^(system|user|assistant|tool)$")),
    };
    let mut l_content = loc.to_vec();
    l_content.push(Loc::Key("content".to_owned()));
    let content = match m.get("content") {
        None => {
            v.push(&l_content, "missing", MSG_MISSING);
            None
        }
        Some(PyJson::Str(s)) => Some(Value::String(s.clone())),
        Some(PyJson::List(items)) if items.iter().all(|i| matches!(i, PyJson::Dict(_))) => {
            Some(Value::Array(items.iter().map(PyJson::to_value).collect()))
        }
        Some(other) => {
            // smart union: both members fail; pydantic reports each member's errors under its tag
            let mut l_str = l_content.clone();
            l_str.push(Loc::Key("str".to_owned()));
            v.push(&l_str, "string_type", MSG_STR);
            let mut l_list = l_content.clone();
            l_list.push(Loc::Key("list[dict[str,any]]".to_owned()));
            match other {
                PyJson::List(items) => {
                    for (i, it) in items.iter().enumerate() {
                        if !matches!(it, PyJson::Dict(_)) {
                            let mut l = l_list.clone();
                            l.push(Loc::Idx(i));
                            v.push(&l, "dict_type", MSG_DICT);
                        }
                    }
                }
                _ => v.push(&l_list, "list_type", MSG_LIST),
            }
            None
        }
    };
    extras(v, m, &["role", "content"], loc);
    let (role, content) = (role?, content?);
    let mut out = Map::new();
    out.insert("role".to_owned(), Value::String(role));
    out.insert("content".to_owned(), content);
    Some(Value::Object(out))
}

pub fn chat_body(raw: &RawBody, v: &mut V) -> Option<ChatBody> {
    let m = model(v, raw)?;
    let model_ = req(v, m, "model").and_then(|x| v.str_(&at(&["body", "model"]), x));
    let mloc = at(&["body", "messages"]);
    let messages = match req(v, m, "messages") {
        None => None,
        Some(PyJson::List(items)) => {
            let before = v.errors.len();
            let mut out = Vec::new();
            for (i, it) in items.iter().enumerate() {
                let mut l = mloc.clone();
                l.push(Loc::Idx(i));
                if let Some(msg) = chat_message(v, &l, it) {
                    out.push(msg);
                }
            }
            if v.errors.len() != before {
                None
            } else if out.is_empty() {
                v.push(
                    &mloc,
                    "too_short",
                    "List should have at least 1 item after validation, not 0",
                );
                None
            } else if out.len() > 200 {
                v.push(
                    &mloc,
                    "too_long",
                    format!(
                        "List should have at most 200 items after validation, not {}",
                        out.len()
                    ),
                );
                None
            } else {
                Some(out)
            }
        }
        Some(_) => {
            v.push(&mloc, "list_type", MSG_LIST);
            None
        }
    };
    let mt_loc = at(&["body", "max_tokens"]);
    let max_tokens = match m.get("max_tokens") {
        None | Some(PyJson::Null) => Some(None),
        Some(x) => match v.int_(&mt_loc, x) {
            Some(i) if i < 1 => {
                v.push(&mt_loc, "greater_than_equal", msg_ge("1"));
                None
            }
            Some(i) => Some(Some(i)),
            None => None,
        },
    };
    let stream = m
        .get("stream")
        .map_or(Some(true), |x| v.bool_(&at(&["body", "stream"]), x));
    let sys_loc = at(&["body", "system"]);
    let system = match m.get("system") {
        None | Some(PyJson::Null) => Some(None),
        Some(x) => match v.str_(&sys_loc, x) {
            Some(s) if char_len(&s) > 100_000 => {
                v.push(
                    &sys_loc,
                    "string_too_long",
                    "String should have at most 100000 characters",
                );
                None
            }
            Some(s) => Some(Some(s)),
            None => None,
        },
    };
    extras(
        v,
        m,
        &["model", "messages", "max_tokens", "stream", "system"],
        &at(&["body"]),
    );
    Some(ChatBody {
        model: model_?,
        messages: messages?,
        max_tokens: max_tokens?,
        stream: stream?,
        system: system?,
    })
}

pub struct SettingsBody {
    pub urls: Option<IndexMap<String, String>>,
    pub poll_interval_s: Option<f64>,
    pub playground_key_env: Option<IndexMap<String, String>>,
}

pub fn settings_body(raw: &RawBody, v: &mut V) -> Option<SettingsBody> {
    let m = model(v, raw)?;
    let urls = match m.get("urls") {
        None | Some(PyJson::Null) => Some(None),
        Some(x) => v.dict_str(&at(&["body", "urls"]), x).map(Some),
    };
    let poll = match m.get("poll_interval_s") {
        None | Some(PyJson::Null) => Some(None),
        Some(x) => v.float_(&at(&["body", "poll_interval_s"]), x).map(Some),
    };
    let key_env = match m.get("playground_key_env") {
        None | Some(PyJson::Null) => Some(None),
        Some(x) => v
            .dict_str(&at(&["body", "playground_key_env"]), x)
            .map(Some),
    };
    extras(
        v,
        m,
        &["urls", "poll_interval_s", "playground_key_env"],
        &at(&["body"]),
    );
    Some(SettingsBody {
        urls: urls?,
        poll_interval_s: poll?,
        playground_key_env: key_env?,
    })
}

// ── query parameters ────────────────────────────────────────────────────────────────────────────

/// `Annotated[int, Query(ge=, le=)] = default`: the LAST value of a repeated parameter wins (Starlette).
pub fn query_int(
    v: &mut V,
    query: &[(String, String)],
    name: &str,
    default: i64,
    ge: Option<i64>,
    le: Option<i64>,
) -> Option<i64> {
    let Some(raw) = query
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, x)| x.clone())
    else {
        return Some(default);
    };
    let loc = vec![Loc::Key("query".to_owned()), Loc::Key(name.to_owned())];
    let i = v.int_(&loc, &PyJson::Str(raw))?;
    if let Some(g) = ge
        && i < g
    {
        v.push(&loc, "greater_than_equal", msg_ge(&g.to_string()));
        return None;
    }
    if let Some(l) = le
        && i > l
    {
        v.push(&loc, "less_than_equal", msg_le(&l.to_string()));
        return None;
    }
    Some(i)
}

/// `name: str = default` / `str | None = None`.
#[must_use]
pub fn query_str(query: &[(String, String)], name: &str) -> Option<String> {
    query
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, x)| x.clone())
}
