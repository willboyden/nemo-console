//! Domain models shared by the core, the adapters and (as JSON) the frontend. Port of `domain/models.py`.
//!
//! Contract v1 (PORTING.md section 6.1). Field names, order, types and defaults mirror the pydantic models:
//! every `Option` serialises as `null` (pydantic `model_dump()` keeps `None`), every Python `float` is an `f64`
//! (serde_json writes `5.0`, never `5`), every `int` is an integer type. Changes are additive addenda only.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The eleven managed systems, in the registry order (`adapters/__init__.py` `SYSTEM_IDS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SystemId {
    Switchyard,
    Relay,
    Dynamo,
    Skillevaluator,
    Datadesigner,
    Openshell,
    Dcgm,
    Aiperf,
    Guardrails,
    Evaluator,
    Modelopt,
}

impl SystemId {
    /// Registry order: overview, /systems and /health list systems in exactly this order.
    pub const ALL: [SystemId; 11] = [
        SystemId::Switchyard,
        SystemId::Relay,
        SystemId::Dynamo,
        SystemId::Skillevaluator,
        SystemId::Datadesigner,
        SystemId::Openshell,
        SystemId::Dcgm,
        SystemId::Aiperf,
        SystemId::Guardrails,
        SystemId::Evaluator,
        SystemId::Modelopt,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SystemId::Switchyard => "switchyard",
            SystemId::Relay => "relay",
            SystemId::Dynamo => "dynamo",
            SystemId::Skillevaluator => "skillevaluator",
            SystemId::Datadesigner => "datadesigner",
            SystemId::Openshell => "openshell",
            SystemId::Dcgm => "dcgm",
            SystemId::Aiperf => "aiperf",
            SystemId::Guardrails => "guardrails",
            SystemId::Evaluator => "evaluator",
            SystemId::Modelopt => "modelopt",
        }
    }

    /// Exact match only (Python compares the path segment with `==`).
    #[must_use]
    pub fn parse(s: &str) -> Option<SystemId> {
        SystemId::ALL.into_iter().find(|id| id.as_str() == s)
    }

    /// Python's `system.capitalize()` (MissingAdapter label).
    #[must_use]
    pub fn capitalized(self) -> String {
        let s = self.as_str();
        let mut c = s.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => String::new(),
        }
    }
}

impl std::fmt::Display for SystemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `Health`: "off" = opt-in and stopped on purpose (addendum 9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    Up,
    Degraded,
    Down,
    Unknown,
    Off,
}

impl Health {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Up => "up",
            Health::Degraded => "degraded",
            Health::Down => "down",
            Health::Unknown => "unknown",
            Health::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Viewer,
}

impl Role {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Viewer => "viewer",
        }
    }
}

// ── status ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    pub name: String,
    /// Display URL, never contains credentials.
    pub url: String,
    /// "api" | "metrics" | "health" | "ui"
    pub role: String,
    /// "none" | "bearer" | "forwarded"
    pub auth: String,
}

/// One moving part behind a system: a container, a systemd user unit, a process, an HTTP peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    /// "container" | "systemd" | "process" | "http"
    pub kind: String,
    pub name: String,
    /// "running" | "stopped" | "failed" | "absent" | "unknown"
    pub state: String,
    pub image: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    pub severity: Severity,
    /// Stable slug, e.g. "relay.plugin.unsigned".
    pub code: String,
    pub message: String,
    /// ONE exact remedy, copy-pasteable.
    pub fix: Option<String>,
}

impl Problem {
    #[must_use]
    pub fn new(
        severity: Severity,
        code: &str,
        message: impl Into<String>,
        fix: Option<String>,
    ) -> Self {
        Problem {
            severity,
            code: code.to_owned(),
            message: message.into(),
            fix,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemStatus {
    pub system: SystemId,
    pub health: Health,
    pub version: Option<String>,
    /// One line for the overview tile.
    pub summary: String,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub problems: Vec<Problem>,
    /// Unix seconds.
    pub checked_at: f64,
}

// ── metrics ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricPoint {
    /// Canonical key, see docs/ARCHITECTURE.md section 5.
    pub key: String,
    pub value: f64,
    /// "1", "ms", "tok", "tok/s", "B", "ratio"
    #[serde(default)]
    pub unit: String,
    /// Insertion-ordered like a Python dict.
    #[serde(default)]
    pub labels: indexmap::IndexMap<String, String>,
}

impl MetricPoint {
    #[must_use]
    pub fn new(key: &str, value: f64) -> Self {
        MetricPoint {
            key: key.to_owned(),
            value,
            unit: String::new(),
            labels: indexmap::IndexMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub system: SystemId,
    pub ts: f64,
    /// False when the system is down: points == [].
    pub available: bool,
    #[serde(default)]
    pub points: Vec<MetricPoint>,
    pub error: Option<String>,
}

// ── configuration ───────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigFileSpec {
    /// Stable, e.g. "routes".
    pub id: String,
    pub label: String,
    pub description: String,
    /// Repo-relative or ~-prefixed, for humans only.
    pub display_path: String,
    /// "toml" | "yaml" | "text"
    pub format: String,
    pub editable: bool,
    /// REQUIRED when editable is false.
    pub why_readonly: Option<String>,
    #[serde(default)]
    pub validators: Vec<String>,
    #[serde(default = "yes")]
    pub authoritative: bool,
    #[serde(default)]
    pub apply_effect: String,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigFile {
    pub spec: ConfigFileSpec,
    pub content: String,
    pub sha256: String,
    pub mtime: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub line: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationResult {
    pub ok: bool,
    pub validator: String,
    pub authoritative: bool,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PostCheck {
    Healthy,
    Unhealthy,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyResult {
    pub ok: bool,
    pub new_sha256: Option<String>,
    /// The backup taken before the write.
    pub revision_id: Option<String>,
    #[serde(default)]
    pub restarted: bool,
    #[serde(default = "skipped")]
    pub post_check: PostCheck,
    /// Redacted, capped.
    #[serde(default)]
    pub detail: String,
    /// True if post_check failed and core restored the backup.
    #[serde(default)]
    pub rolled_back: bool,
}

fn skipped() -> PostCheck {
    PostCheck::Skipped
}

impl ApplyResult {
    /// `ApplyResult(ok=..., detail=...)` with every other field at its default.
    #[must_use]
    pub fn new(ok: bool, detail: impl Into<String>) -> Self {
        ApplyResult {
            ok,
            new_sha256: None,
            revision_id: None,
            restarted: false,
            post_check: PostCheck::Skipped,
            detail: detail.into(),
            rolled_back: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigRevision {
    pub id: String,
    pub ts: f64,
    pub actor: String,
    pub sha256: String,
    #[serde(default)]
    pub note: String,
}

// ── actions ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    Int,
    Float,
    Bool,
    Enum,
    String,
    StringList,
}

impl ParamType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ParamType::Int => "int",
            ParamType::Float => "float",
            ParamType::Bool => "bool",
            ParamType::Enum => "enum",
            ParamType::String => "string",
            ParamType::StringList => "string_list",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    pub key: String,
    /// CLI flag / env name this renders to.
    pub flag: Option<String>,
    pub label: String,
    pub help: String,
    #[serde(rename = "type")]
    pub type_: ParamType,
    #[serde(default = "general")]
    pub group: String,
    /// `Any` in Python: kept as JSON (an int stays an int, a float stays a float).
    #[serde(default)]
    pub default: Value,
    pub choices: Option<Vec<String>>,
    /// Python `float | None` (pydantic turns `min=1` into `1.0`).
    pub min: Option<f64>,
    pub max: Option<f64>,
    #[serde(default)]
    pub advanced: bool,
    #[serde(default = "yes")]
    pub requires_restart: bool,
}

fn general() -> String {
    "general".to_owned()
}

impl ParamSpec {
    /// `ParamSpec(key=, label=, help=, type=)` with every other field at its default.
    #[must_use]
    pub fn new(key: &str, label: &str, help: &str, type_: ParamType) -> Self {
        ParamSpec {
            key: key.to_owned(),
            flag: None,
            label: label.to_owned(),
            help: help.to_owned(),
            type_,
            group: general(),
            default: Value::Null,
            choices: None,
            min: None,
            max: None,
            advanced: false,
            requires_restart: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Danger {
    Safe,
    Disruptive,
    Destructive,
}

impl Danger {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Danger::Safe => "safe",
            Danger::Disruptive => "disruptive",
            Danger::Destructive => "destructive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionSpec {
    pub id: String,
    pub label: String,
    pub description: String,
    /// disruptive/destructive need confirm=true.
    pub danger: Danger,
    #[serde(default)]
    pub params: Vec<ParamSpec>,
    #[serde(default = "admin")]
    pub role: Role,
}

fn admin() -> Role {
    Role::Admin
}

impl ActionSpec {
    #[must_use]
    pub fn new(id: &str, label: &str, description: &str, danger: Danger) -> Self {
        ActionSpec {
            id: id.to_owned(),
            label: label.to_owned(),
            description: description.to_owned(),
            danger,
            params: Vec::new(),
            role: Role::Admin,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionResult {
    pub ok: bool,
    /// Redacted + capped by the runner.
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub problems: Vec<Problem>,
}

impl ActionResult {
    #[must_use]
    pub fn new(ok: bool, output: impl Into<String>) -> Self {
        ActionResult {
            ok,
            output: output.into(),
            duration_ms: 0,
            problems: Vec::new(),
        }
    }
}

// ── events / playground ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemEvent {
    pub ts: f64,
    pub system: SystemId,
    /// "routing.decision" | "relay.atof" | "dynamo.log" | ...
    pub kind: String,
    /// One line, already redacted.
    pub summary: String,
    /// Metadata only, never prompt/completion text. Insertion-ordered like a Python dict.
    #[serde(default)]
    pub data: Map<String, Value>,
}

/// How the UI may send one chat request THROUGH this system. Chat text is never persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaygroundSpec {
    /// "openai_chat" | "anthropic_messages"
    pub wire: String,
    pub models: Vec<String>,
    pub default_model: String,
    #[serde(default)]
    pub note: String,
}

// ── topology ────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyNode {
    pub id: String,
    pub label: String,
    /// "client" | "router" | "engine" | "gateway" | "observability"
    pub kind: String,
    pub health: Health,
    pub system: Option<SystemId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyEdge {
    pub src: String,
    pub dst: String,
    #[serde(default)]
    pub label: String,
    /// False for opt-in paths that are not taking traffic.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<TopologyEdge>,
    pub ts: f64,
}

/// Current Unix time as a float (Python `time.time()`).
#[must_use]
pub fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
