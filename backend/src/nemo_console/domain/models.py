"""Domain models shared by the core, the adapters and (as JSON) the frontend.

LOCKED (contract v1). Changes after fan-out are ADDITIVE ONLY (new optional fields / new
models), recorded as a numbered addendum in docs/ARCHITECTURE.md section 9. Never rename or
retype a field: the frontend mock server and three adapters depend on these names.
"""
from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, Field

SystemId = Literal["switchyard", "relay", "dynamo"]
Health = Literal["up", "degraded", "down", "unknown"]
Severity = Literal["info", "warn", "error"]
Role = Literal["admin", "viewer"]


# ── status ──────────────────────────────────────────────────────────────────────────────────
class Endpoint(BaseModel):
    name: str
    url: str                                         # display URL, never contains credentials
    role: Literal["api", "metrics", "health", "ui"]
    auth: Literal["none", "bearer", "forwarded"]


class Component(BaseModel):
    """One moving part behind a system: a container, a systemd user unit, a process, an HTTP peer."""
    kind: Literal["container", "systemd", "process", "http"]
    name: str
    state: Literal["running", "stopped", "failed", "absent", "unknown"]
    image: str | None = None
    detail: str | None = None


class Problem(BaseModel):
    severity: Severity
    code: str                                        # stable slug, e.g. "relay.plugin.unsigned"
    message: str
    fix: str | None = None                           # ONE exact remedy, copy-pasteable


class SystemStatus(BaseModel):
    system: SystemId
    health: Health
    version: str | None = None
    summary: str                                     # one line for the overview tile
    endpoints: list[Endpoint] = []
    components: list[Component] = []
    problems: list[Problem] = []
    checked_at: float                                # unix seconds


# ── metrics ─────────────────────────────────────────────────────────────────────────────────
class MetricPoint(BaseModel):
    key: str                                         # canonical, see ARCHITECTURE.md section 5
    value: float
    unit: str = ""                                   # "1", "ms", "tok", "tok/s", "B", "ratio"
    labels: dict[str, str] = Field(default_factory=dict)


class MetricsSnapshot(BaseModel):
    system: SystemId
    ts: float
    available: bool                                  # False when the system is down: points == []
    points: list[MetricPoint] = []
    error: str | None = None


# ── configuration ───────────────────────────────────────────────────────────────────────────
class ConfigFileSpec(BaseModel):
    id: str                                          # stable, e.g. "routes"
    label: str
    description: str
    display_path: str                                # repo-relative or ~-prefixed, for humans only
    format: Literal["toml", "yaml", "text"]
    editable: bool
    why_readonly: str | None = None                  # REQUIRED when editable is False
    validators: list[str] = []                       # human names, e.g. "switchyard-server --dry-run"
    authoritative: bool = True                       # False: validators are only a proxy (say so in UI)
    apply_effect: str = ""                           # e.g. "recreates the container; ~5 s of dropped requests"


class ConfigFile(BaseModel):
    spec: ConfigFileSpec
    content: str
    sha256: str
    mtime: float


class Diagnostic(BaseModel):
    severity: Severity
    message: str
    line: int | None = None


class ValidationResult(BaseModel):
    ok: bool
    validator: str
    authoritative: bool
    diagnostics: list[Diagnostic] = []
    note: str | None = None                          # e.g. "the dry-run does not load the Relay plugin schema"


class ApplyResult(BaseModel):
    ok: bool
    new_sha256: str | None = None
    revision_id: str | None = None                   # the backup taken before the write
    restarted: bool = False
    post_check: Literal["healthy", "unhealthy", "skipped"] = "skipped"
    detail: str = ""                                 # redacted, capped
    rolled_back: bool = False                        # True if post_check failed and core restored the backup


class ConfigRevision(BaseModel):
    id: str
    ts: float
    actor: str
    sha256: str
    note: str = ""


# ── actions ─────────────────────────────────────────────────────────────────────────────────
class ParamSpec(BaseModel):
    key: str
    flag: str | None = None                          # CLI flag / env name this renders to
    label: str
    help: str
    type: Literal["int", "float", "bool", "enum", "string", "string_list"]
    group: str = "general"
    default: Any = None
    choices: list[str] | None = None
    min: float | None = None
    max: float | None = None
    advanced: bool = False
    requires_restart: bool = True


class ActionSpec(BaseModel):
    id: str
    label: str
    description: str
    danger: Literal["safe", "disruptive", "destructive"]   # disruptive/destructive need confirm=true
    params: list[ParamSpec] = []
    role: Role = "admin"


class ActionResult(BaseModel):
    ok: bool
    output: str = ""                                 # redacted + capped by the runner
    duration_ms: int = 0
    problems: list[Problem] = []


# ── events / playground ─────────────────────────────────────────────────────────────────────
class SystemEvent(BaseModel):
    ts: float
    system: SystemId
    kind: str                                        # "routing.decision" | "relay.atof" | "dynamo.log" | ...
    summary: str                                     # one line, already redacted
    data: dict[str, Any] = Field(default_factory=dict)   # metadata only, never prompt/completion text


class PlaygroundSpec(BaseModel):
    """How the UI may send one chat request THROUGH this system. Chat text is never persisted."""
    wire: Literal["openai_chat", "anthropic_messages"]
    models: list[str]                                # route ids / model ids the UI offers
    default_model: str
    note: str = ""


# ── topology ────────────────────────────────────────────────────────────────────────────────
class TopologyNode(BaseModel):
    id: str                                          # "litellm", "switchyard", "relay", "dynamo-frontend", ...
    label: str
    kind: Literal["client", "router", "engine", "gateway", "observability"]
    health: Health = "unknown"
    system: SystemId | None = None                   # set when the node is one of the three managed systems


class TopologyEdge(BaseModel):
    src: str
    dst: str
    label: str = ""
    active: bool = True                              # False for opt-in paths that are not taking traffic


class Topology(BaseModel):
    nodes: list[TopologyNode]
    edges: list[TopologyEdge]
    ts: float
