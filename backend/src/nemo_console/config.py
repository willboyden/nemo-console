"""Console settings: env (NEMO_CONSOLE_*) plus an optional user env file. Secrets are referenced by env-var NAME only."""
from __future__ import annotations

import os
import re
from importlib import metadata
from pathlib import Path

from pydantic import BaseModel, Field, field_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

try:
    __version__ = metadata.version("nemo-console")
except metadata.PackageNotFoundError:  # pragma: no cover
    __version__ = "0.3.1"

ENV_PREFIX = "NEMO_CONSOLE_"
NO_USER_ENV = "NEMO_CONSOLE_NO_USER_ENV"


def user_env_file() -> Path | None:
    """~/.config/nemo-console/env, unless disabled (tests set NEMO_CONSOLE_NO_USER_ENV=1)."""
    if os.environ.get(NO_USER_ENV):
        return None
    return Path.home() / ".config" / "nemo-console" / "env"


DEFAULT_URLS = {
    "switchyard": "http://127.0.0.1:4010",
    "relay": "http://127.0.0.1:4040",
    "dynamo": "http://127.0.0.1:8020",
    "dynamo_worker_metrics": "http://127.0.0.1:8081",
    "litellm": "http://127.0.0.1:4000",
    # addendum 9.8
    "dcgm": "http://127.0.0.1:9400",           # this host's dcgm-exporter (/metrics)
    "prometheus": "http://127.0.0.1:9090",     # the lab Prometheus: DCGM_* of every scraped host (fleet view)
    "guardrails": "http://127.0.0.1:8806",     # NeMo Guardrails server (security/nemo-guardrails)
}
# Engine containers of the author's deployment (2026-10-17, addendum 9.6). Resident: Nemotron 3.5 Super VL on Dynamo
# (inference/dynamo/docker-compose.yml + docker-compose.super-vl.yml, worker on BOTH cards), nemotron-lightning (vLLM
# TP2, both cards) and chatterbox-tts (Workstation card). Opt-in and exclusive with Super VL: Bolt (one card) and GLM
# (both cards). The flashnext and 27B containers no longer exist there: a container that does not exist is "not
# deployed", never red. Every name below is a setting; nothing here is a fixed identity.
LIGHTNING = "ai-lab-vllm-nemotron-lightning-nemotron-lightning-1"
FLASHNEXT = "ai-lab-sglang-flashnext-flashnext-1"
QWEN38 = "ai-lab-vllm-qwen38-vllm-1"
BOLT = "ai-lab-vllm-nemotron-bolt-nemotron-bolt-1"
GLM = "lil-glm53-tp2"
CHATTERBOX = "ai-lab-chatterbox-tts-tts-1"
DYNAMO_WORKER = "ai-lab-dynamo-dynamo-worker-1"
# router alias -> engine container. Only aliases a stage_router route escalates to (capable_target) are checked: a
# stopped container raises switchyard.tier.capable_dark, a missing one only an info "not deployed". The Dynamo worker
# is the same container for the 27B stack and the Super VL stack: a running worker is taken as "answering" (R-11).
DEFAULT_ENGINE_PROBES = {
    "nemotron-3.5-super-vl": DYNAMO_WORKER,
    "nemotron-3.5-bolt": BOLT,
    "glm-5.3-flash-next-local": GLM,
    "qwen3.8-flash-next": FLASHNEXT,
    "qwen3.8-27b-1m": QWEN38,
}
# engine container -> the command, run from the repo root, that starts it correctly (DISPLAY ONLY). Each says what it
# stops or needs, because starting one of these tears down whatever holds its cards: none of them is "the" default.
# A container not listed gets the generic compose-dir hint.
DEFAULT_ENGINE_START = {
    DYNAMO_WORKER: ("docker compose -f inference/dynamo/docker-compose.yml -f inference/dynamo/docker-compose.super-vl.yml "
                    "up -d dynamo-frontend dynamo-worker   # Super VL, both cards: start lightning (TP2) first"),
    BOLT: "scripts/use-model.sh nemotron-3.5-bolt   # exclusive: this stops Super VL",
    GLM: "scripts/use-model.sh glm53-flash-next   # exclusive, both cards: this stops Super VL",
    FLASHNEXT: "inference/sglang-flashnext/flashnext.sh up   # only if flashnext should be resident: it takes BOTH cards",
    QWEN38: "scripts/use-model.sh qwen3.8-27b-1m   # exclusive: this stops the engine on its card",
}
# Dynamo card model (addendum 9.4, defaults updated by 9.6). Commands run from the repo root and are DISPLAY ONLY
# (problem fixes); the console never runs them. Lists REPLACE the defaults when set
# (NEMO_CONSOLE_DYNAMO_EXCLUSIVE_ENGINES='["a","b"]').
DEFAULT_DYNAMO_START = "scripts/use-model.sh qwen3.8-27b-dynamo"      # the BASE stack: what the console's `up` starts
# with no Dynamo container. Neutral on purpose: without an alias the script only lists the engines; switching away from
# Dynamo (which stops it) is the operator's choice of alias.
DEFAULT_MODEL_START = "scripts/use-model.sh   # lists the engines; `scripts/use-model.sh <alias>` switches away from Dynamo"
# running on one of the worker's cards, these cannot coexist with it (VRAM): `up` is refused while one runs
DEFAULT_DYNAMO_EXCLUSIVE = [BOLT, GLM, FLASHNEXT, QWEN38]
# allowed beside the worker (lightning TP2 at 0.20 per card beside Super VL at 0.55; ollama holds no VRAM until a model
# loads; chatterbox-tts on the Workstation card)
DEFAULT_DYNAMO_COEXISTING = [LIGHTNING, "ai-lab-ollama-ollama-1", CHATTERBOX]
# GPU-visible but holding no meaningful VRAM (monitoring)
DEFAULT_DYNAMO_IGNORED = ["ai-lab-observability-dcgm-exporter-1"]
# what cannot answer while Dynamo holds the card(s), because its engine is exclusive (dynamo.holds_card, info)
DEFAULT_DYNAMO_DARK = ["nemotron-3.5-bolt (route bolt)", "glm-5.3-flash-next-local",
                       "qwen3.8-flash-next and -quick (auto-local and auto-flashnext capable tier)",
                       "qwen3.8-27b-1m (direct-27b)"]
# Engine nodes of the topology (litellm -> engine "alias" edges). opt_in: stopped = "off" (else "down"); a container
# that does not exist is drawn "off" and labelled "not deployed". REPLACES the default when set, e.g.
# NEMO_CONSOLE_TOPOLOGY_ENGINES='[{"id": "my-engine", "label": "My engine", "container": "c-1", "opt_in": true}]'.
DEFAULT_TOPOLOGY_ENGINES = [
    {"id": "vllm-lightning", "label": "vLLM Lightning (efficient tier, judge)", "container": LIGHTNING, "opt_in": False},
    {"id": "vllm-bolt", "label": "vLLM Nemotron Bolt (opt-in, one card)", "container": BOLT, "opt_in": True},
    {"id": "vllm-glm53", "label": "vLLM GLM-5.3-Flash-Next (opt-in, both cards)", "container": GLM, "opt_in": True},
    {"id": "sglang-flashnext", "label": "SGLang FlashNext (opt-in, both cards)", "container": FLASHNEXT, "opt_in": True},
    {"id": "vllm-qwen38", "label": "vLLM Qwen3.8 27B (opt-in)", "container": QWEN38, "opt_in": True},
]
# Both gateways want a bearer: Switchyard forwards it to LiteLLM (forward_auth), Relay's /v1/* answers 401 without one
# (opencode sends LITELLM_API_KEY to :4040). Override per system: NEMO_CONSOLE_PLAYGROUND_KEY_ENV__RELAY=OTHER_API_KEY.
DEFAULT_KEY_ENV = {"switchyard": "LITELLM_API_KEY", "relay": "LITELLM_API_KEY"}
REPO_MARKER = Path("inference") / "dynamo" / "docker-compose.yml"
# addendum 9.8 defaults (each one a setting)
DCGM_EXPORTER_CONTAINER = "ai-lab-observability-dcgm-exporter-1"
# the FULL DCGM image (nv-hostengine + dcgmi + diagnostic plugins); the exporter image ships no diag plugins.
# Tag + digest pulled and run 2026-10-24 (level-1 diag on GPU 0: Pass, ~3 s).
DCGM_DIAG_IMAGE = ("nvcr.io/nvidia/cloud-native/dcgm:4.6.1-1-ubuntu24.04"
                   "@sha256:48464e50aa76671f5b204264cb4999739591112bf74330a727523f5655c6d7f2")
DCGM_PROMETHEUS_JOBS = ["dcgm", "dcgm-aitop100", "dcgm-sparks"]
OPENSHELL_CONTAINERS = ["ai-lab-dd-gateway", "ai-lab-dd-cli-fwd"]
GUARDRAILS_CONTAINER = "ai-lab-nemo-guardrails"
CONTAINER_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.\-]{0,127}")
IMAGE_REF_RE = re.compile(r"[a-z0-9][a-z0-9._/:\-]{0,254}(@sha256:[0-9a-f]{64})?")
COMPOSE_PROJECT_RE = re.compile(r"[a-z0-9][a-z0-9_\-]{0,62}")          # docker compose's own rule for -p
COMPOSE_SERVICE_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.\-]{0,62}")
UNIT_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9@_.:\-]{0,240}\.service")
IN_CONTAINER_PATH_RE = re.compile(r"(/[A-Za-z0-9_.\-]+){1,16}")
LAYOUT_SEGMENT_RE = re.compile(r"[A-Za-z0-9_.\-]{1,64}")
CREDENTIAL_ENV_RE = re.compile(r"[A-Z_][A-Z0-9_]{0,63}")

# Deployment identities (addendum 9.11): the compose projects, services, containers and systemd unit the adapters act
# on, and where each system's files live under repo_root. The defaults are the author's deployment; every value is an
# identifier that reaches docker / systemctl argv, so each is checked with a full match, and none is writable through
# the API (environment or the user env file only, read at start).
SWITCHYARD_CONTAINER = "ai-lab-switchyard-switchyard-1"
SWITCHYARD_COMPOSE_PROJECT = "ai-lab-switchyard"
SWITCHYARD_COMPOSE_SERVICE = "switchyard"
SWITCHYARD_IMAGE = "ai-lab-switchyard:0.3.0"         # the dry-run validator's image when the container cannot be inspected
SWITCHYARD_LOG_PATH = "/var/log/switchyard/routing.jsonl"   # the routing log INSIDE the container
RELAY_UNIT = "nemo-relay-gateway.service"            # systemd --user unit of the Relay gateway
DYNAMO_COMPOSE_PROJECT = "ai-lab-dynamo"
DYNAMO_ROLES = ("frontend", "worker", "mocker", "sv_frontend")
# compose service per role (sv_frontend: the real frontend behind the Super VL gateway, docker-compose.super-vl.yml)
DEFAULT_DYNAMO_SERVICES = {"frontend": "dynamo-frontend", "worker": "dynamo-worker", "mocker": "dynamo-mocker",
                           "sv_frontend": "dynamo-sv-frontend"}
DEFAULT_DYNAMO_CONTAINERS = {"frontend": "ai-lab-dynamo-dynamo-frontend-1", "worker": DYNAMO_WORKER,
                             "mocker": "ai-lab-dynamo-dynamo-mocker-1", "sv_frontend": "ai-lab-dynamo-dynamo-sv-frontend-1"}
GUARDRAILS_COMPOSE_PROJECT = "ai-lab-nemo-guardrails"
DCGM_COMPOSE_PROJECT = "ai-lab-observability"         # a restart is only sent to this project's dcgm-exporter service
# directory of each system under repo_root (relative, '/'-separated). Set single keys, e.g.
# NEMO_CONSOLE_LAYOUT__SWITCHYARD=deploy/switchyard; the others keep their defaults; unknown keys are refused.
DEFAULT_LAYOUT = {
    "switchyard": "inference/switchyard",
    "relay": "clients/nemo-relay",
    "dynamo": "inference/dynamo",
    "skills": ".claude/skills",
    "data_designer": "clients/data-designer",
    "openshell_runner": "clients/data-designer/runner",
    "observability": "observability",
    "aiperf": "clients/aiperf",
    "guardrails": "security/nemo-guardrails",
    "evaluator": "clients/nemo-evaluator",
    "skillevaluator": "clients/skillevaluator",
    "ollama": "inference/ollama",
}


def credential_env_allowed(name: str) -> bool:
    """Only credential-looking env names may be referenced (else an admin could read the length of, or send as a
    Bearer token, ANY variable such as AWS_SECRET_ACCESS_KEY): LITELLM_*, NEMO_CONSOLE_PLAYGROUND_*, or *_API_KEY."""
    return CREDENTIAL_ENV_RE.fullmatch(name) is not None and (
        name.startswith(("LITELLM_", "NEMO_CONSOLE_PLAYGROUND_")) or name.endswith("_API_KEY"))


def _layout_rel(key: str, value: str) -> str:
    """A repo-relative directory: '/'-separated plain segments, no '.', '..', absolute path, backslash or NUL."""
    parts = value.strip("/").split("/") if value and not value.startswith("/") else []
    if not parts or len(parts) > 8 or any(p in {".", ".."} or not LAYOUT_SEGMENT_RE.fullmatch(p) for p in parts):
        raise ValueError(f"layout {key!r}: not a relative directory under the repo root: {value[:80]!r}")
    return "/".join(parts)


class TopologyEngine(BaseModel):
    """One engine node of the topology. `id` and `container` are identifiers; `label` is display text."""
    id: str = Field(pattern=r"^[a-z0-9][a-z0-9_.\-]{0,63}$")
    label: str = Field(min_length=1, max_length=80)
    container: str = Field(pattern=r"^[a-zA-Z0-9][a-zA-Z0-9_.\-]{0,127}$")
    opt_in: bool = True


def default_repo_root() -> Path:
    """Standalone-safe: walk up from this file to the directory holding the compose files, else the CWD.

    NEMO_CONSOLE_REPO_ROOT overrides this entirely (pydantic reads it into `Settings.repo_root`).
    Never raises; `Settings.repo_root_problem()` reports a root that does not contain the repo.
    """
    for parent in Path(__file__).resolve().parents:
        if (parent / REPO_MARKER).is_file():
            return parent
    return Path.cwd()


def default_frontend_dir() -> Path:
    return Path(__file__).resolve().parents[3] / "frontend"


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix=ENV_PREFIX, env_nested_delimiter="__", extra="ignore")

    host: str = "127.0.0.1"
    port: int = 8793
    state_dir: Path = Field(default_factory=lambda: Path.home() / ".local" / "share" / "nemo-console")
    repo_root: Path = Field(default_factory=default_repo_root)
    frontend_dir: Path = Field(default_factory=default_frontend_dir)
    allowed_hosts: list[str] = []            # extra Host header values (rarely needed)

    # named base URLs handed to the adapters (loopback only; validated on PUT /settings)
    urls: dict[str, str] = dict(DEFAULT_URLS)
    allowed_url_hosts: list[str] = ["127.0.0.1", "localhost", "::1"]
    # logical name -> ENV VAR NAME holding the playground credential (the VALUE is never stored)
    playground_key_env: dict[str, str] = dict(DEFAULT_KEY_ENV)

    # router alias -> compose-managed engine container; a stopped one darkens the Switchyard tier that targets the alias
    engine_probes: dict[str, str] = dict(DEFAULT_ENGINE_PROBES)
    # engine container -> start command shown as the capable_dark fix (display only; the console never runs it)
    engine_start: dict[str, str] = dict(DEFAULT_ENGINE_START)

    # engine nodes drawn on the topology (see DEFAULT_TOPOLOGY_ENGINES); a list REPLACES the default
    topology_engines: list[TopologyEngine] = Field(
        default_factory=lambda: [TopologyEngine(**e) for e in DEFAULT_TOPOLOGY_ENGINES])

    # Dynamo card model (see DEFAULT_DYNAMO_* above). opt_in: no running Dynamo container = health "off", not "down"
    dynamo_opt_in: bool = True
    dynamo_start: str = DEFAULT_DYNAMO_START
    default_model_start: str = DEFAULT_MODEL_START
    dynamo_exclusive_engines: list[str] = list(DEFAULT_DYNAMO_EXCLUSIVE)
    dynamo_coexisting_engines: list[str] = list(DEFAULT_DYNAMO_COEXISTING)
    dynamo_ignored_containers: list[str] = list(DEFAULT_DYNAMO_IGNORED)
    dynamo_dark_while_held: list[str] = list(DEFAULT_DYNAMO_DARK)

    trust_loopback: bool = False       # opt-in: any local process (and a same-host proxy) would otherwise be admin
    background_tasks: bool = True
    poll_interval_s: float = 2.0
    ring_seconds: int = 900
    status_ttl_s: float = 2.0
    adapter_timeout_s: float = 8.0
    action_timeout_s: float = 900.0          # Dynamo `up` waits up to 600 s for health; leave headroom
    action_timeouts: dict[str, float] = {}   # optional per-action override, key 'system.action' e.g. 'dynamo.up'

    body_cap_bytes: int = 1024 * 1024
    config_cap_bytes: int = 256 * 1024
    sse_max_connections: int = 8
    sse_idle_timeout_s: float = 300.0
    sse_heartbeat_s: float = 15.0
    # On SIGTERM uvicorn waits for open connections; a browser's SSE stream never closes on its own, so without a bound
    # `nemo-console stop` (10 s) gave up while the process lingered. After this grace the remaining streams are closed.
    shutdown_grace_s: int = Field(default=5, ge=1, le=60)       # seconds; uvicorn takes whole seconds
    playground_max_tokens: int = 4096
    revisions_keep: int = 50

    otlp_endpoint: str | None = None          # e.g. http://localhost:4317 ; needs the optional opentelemetry packages
    extra_bin_dirs: list[str] = ["~/.local/bin"]   # searched for allowlisted binaries (nemo-relay)

    # Data Designer (addendum 9.7): the console OWNS and creates (0700, at startup) the scripts dir and the datasets
    # root; ~/.data-designer is the TOOL's own config dir — never created by the console (the adapter reports it when
    # missing). The key file is checked for EXISTENCE only, never read (the wrapper injects the key itself).
    dd_scripts_dir: Path = Field(default_factory=lambda: Path.home() / "Datasets" / "data-designer" / "scripts")
    dd_datasets_root: Path = Field(default_factory=lambda: Path.home() / "Datasets")
    dd_config_dir: Path = Field(default_factory=lambda: Path.home() / ".data-designer")

    # addendum 9.8 (OpenShell, DCGM, AIPerf, NeMo Guardrails, NeMo Evaluator). Container names are identifiers.
    dcgm_exporter_container: str = DCGM_EXPORTER_CONTAINER
    dcgm_diag_image: str = DCGM_DIAG_IMAGE
    dcgm_prometheus_jobs: list[str] = list(DCGM_PROMETHEUS_JOBS)
    openshell_containers: list[str] = list(OPENSHELL_CONTAINERS)
    guardrails_container: str = GUARDRAILS_CONTAINER

    # deployment identities and repo layout (addendum 9.11; see the constants above). Read at start only.
    switchyard_container: str = SWITCHYARD_CONTAINER
    switchyard_compose_project: str = SWITCHYARD_COMPOSE_PROJECT
    switchyard_compose_service: str = SWITCHYARD_COMPOSE_SERVICE
    switchyard_image: str = SWITCHYARD_IMAGE
    switchyard_log_path: str = SWITCHYARD_LOG_PATH
    relay_unit: str = RELAY_UNIT
    dynamo_compose_project: str = DYNAMO_COMPOSE_PROJECT
    dynamo_services: dict[str, str] = dict(DEFAULT_DYNAMO_SERVICES)
    dynamo_containers: dict[str, str] = dict(DEFAULT_DYNAMO_CONTAINERS)
    guardrails_compose_project: str = GUARDRAILS_COMPOSE_PROJECT
    dcgm_compose_project: str = DCGM_COMPOSE_PROJECT
    layout: dict[str, str] = dict(DEFAULT_LAYOUT)

    # Active capable-tier probe (addendum 9.11, OFF by default): one tiny real completion per capable-tier alias through
    # the LiteLLM router, so an engine whose container runs but cannot answer (a dead rank behind a healthy /health)
    # shows up as switchyard.tier.capable_broken. Spends one router request per alias per interval.
    capable_probe: bool = False
    capable_probe_interval_s: float = Field(default=300.0, ge=300.0, le=86400.0)
    capable_probe_timeout_s: float = Field(default=60.0, ge=2.0, le=300.0)   # a busy engine queues the probe behind long prefills
    capable_probe_retry_s: float = Field(default=30.0, ge=0.0, le=300.0)     # a failure is retried once after this
    capable_probe_key_env: str = "LITELLM_API_KEY"   # env-var NAME of the router credential (never the value)

    @field_validator("dcgm_exporter_container", "guardrails_container", "switchyard_container")
    @classmethod
    def _container_name(cls, v: str) -> str:
        """Addendum 9.8: these reach `docker inspect/restart` argv; a leading '-' would read as an option."""
        if not CONTAINER_NAME_RE.fullmatch(v):
            raise ValueError(f"not a container name: {v!r}")
        return v

    @field_validator("switchyard_compose_project", "dynamo_compose_project", "guardrails_compose_project",
                     "dcgm_compose_project")
    @classmethod
    def _compose_project(cls, v: str) -> str:
        if not COMPOSE_PROJECT_RE.fullmatch(v):
            raise ValueError(f"not a compose project name: {v!r}")
        return v

    @field_validator("switchyard_compose_service")
    @classmethod
    def _compose_service(cls, v: str) -> str:
        if not COMPOSE_SERVICE_RE.fullmatch(v):
            raise ValueError(f"not a compose service name: {v!r}")
        return v

    @field_validator("switchyard_image")
    @classmethod
    def _switchyard_image(cls, v: str) -> str:
        if not IMAGE_REF_RE.fullmatch(v):
            raise ValueError(f"not an image reference: {v!r}")
        return v

    @field_validator("switchyard_log_path")
    @classmethod
    def _log_path(cls, v: str) -> str:
        if not IN_CONTAINER_PATH_RE.fullmatch(v) or any(seg in {".", ".."} for seg in v.split("/")):
            raise ValueError(f"not an absolute in-container path: {v!r}")
        return v

    @field_validator("relay_unit")
    @classmethod
    def _unit(cls, v: str) -> str:
        if not UNIT_RE.fullmatch(v):
            raise ValueError(f"not a systemd .service unit name: {v!r}")
        return v

    @field_validator("dynamo_services")
    @classmethod
    def _dynamo_services(cls, v: dict[str, str]) -> dict[str, str]:
        merged = {**DEFAULT_DYNAMO_SERVICES, **v}
        for role, name in merged.items():
            if role not in DYNAMO_ROLES:
                raise ValueError(f"unknown Dynamo role {role!r} (one of {', '.join(DYNAMO_ROLES)})")
            if not COMPOSE_SERVICE_RE.fullmatch(name):
                raise ValueError(f"dynamo_services {role!r}: not a compose service name: {name!r}")
        return merged

    @field_validator("dynamo_containers")
    @classmethod
    def _dynamo_containers(cls, v: dict[str, str]) -> dict[str, str]:
        merged = {**DEFAULT_DYNAMO_CONTAINERS, **v}
        for role, name in merged.items():
            if role not in DYNAMO_ROLES:
                raise ValueError(f"unknown Dynamo role {role!r} (one of {', '.join(DYNAMO_ROLES)})")
            if not CONTAINER_NAME_RE.fullmatch(name):
                raise ValueError(f"dynamo_containers {role!r}: not a container name: {name!r}")
        if len(set(merged.values())) != len(merged):
            raise ValueError("dynamo_containers: each role needs its own container name")
        return merged

    @field_validator("layout")
    @classmethod
    def _layout(cls, v: dict[str, str]) -> dict[str, str]:
        out = dict(DEFAULT_LAYOUT)
        for key, rel in v.items():
            if key not in DEFAULT_LAYOUT:
                raise ValueError(f"unknown layout key {key!r} (one of {', '.join(DEFAULT_LAYOUT)})")
            out[key] = _layout_rel(key, rel)
        return out

    @field_validator("capable_probe_key_env")
    @classmethod
    def _probe_key_env(cls, v: str) -> str:
        """Same rule as the playground's credential names: only credential-looking env names can be referenced."""
        if not credential_env_allowed(v):
            raise ValueError("capable_probe_key_env takes an ENV VAR NAME: LITELLM_*, NEMO_CONSOLE_PLAYGROUND_* or *_API_KEY")
        return v

    @field_validator("openshell_containers")
    @classmethod
    def _container_names(cls, v: list[str]) -> list[str]:
        for name in v:
            if not CONTAINER_NAME_RE.fullmatch(name):
                raise ValueError(f"not a container name: {name!r}")
        return v

    @field_validator("dcgm_diag_image")
    @classmethod
    def _image_ref(cls, v: str) -> str:
        if not IMAGE_REF_RE.fullmatch(v):
            raise ValueError(f"not an image reference: {v!r}")
        return v

    @field_validator("urls")
    @classmethod
    def _merge_urls(cls, v: dict[str, str]) -> dict[str, str]:
        return {**DEFAULT_URLS, **v}          # NEMO_CONSOLE_URLS__X must not drop the other defaults

    @field_validator("engine_probes")
    @classmethod
    def _merge_probes(cls, v: dict[str, str]) -> dict[str, str]:
        return {**DEFAULT_ENGINE_PROBES, **v}

    @field_validator("engine_start")
    @classmethod
    def _merge_start(cls, v: dict[str, str]) -> dict[str, str]:
        return {**DEFAULT_ENGINE_START, **v}

    @field_validator("playground_key_env")
    @classmethod
    def _merge_key_env(cls, v: dict[str, str]) -> dict[str, str]:
        return {**DEFAULT_KEY_ENV, **v}

    def effective_loopback_trust(self) -> tuple[bool, str | None]:
        """(trust, notice). Trust is forced OFF whenever a proxy/remote exposure is plausible."""
        if not self.trust_loopback:
            return False, None
        if self.host not in ("127.0.0.1", "::1", "localhost"):
            return False, f"loopback trust disabled: host {self.host!r} is not a loopback address"
        if self.allowed_hosts:
            return False, "loopback trust disabled: allowed_hosts is set (a same-host reverse proxy would make callers admin)"
        return True, None

    def repo_marker(self) -> Path:
        """The file that proves repo_root is the deployment checkout: the Dynamo compose file under the layout."""
        return Path(self.layout["dynamo"]) / "docker-compose.yml"

    def repo_root_problem(self) -> str | None:
        marker = self.repo_marker()
        if (self.repo_root / marker).is_file():
            return None
        return (f"repo root {self.repo_root} does not contain {marker}; "
                "set NEMO_CONSOLE_REPO_ROOT to the lab repository checkout (see docs/OPERATIONS.md)")

    def resolved_paths(self) -> dict[str, Path]:
        """Named ABSOLUTE config-only paths for the adapters (see docs: proposal, additive). Directories under the repo
        root come from `layout` (addendum 9.11); the defaults are the author's deployment."""
        r = self.repo_root
        home = Path.home()
        lay = {k: r / v for k, v in self.layout.items()}
        runner = lay["openshell_runner"]
        return {
            "repo": r,
            "state": self.state_dir,
            "switchyard_dir": lay["switchyard"],
            "switchyard_routes": lay["switchyard"] / "routes.toml",
            "switchyard_compose": lay["switchyard"] / "docker-compose.yml",
            "relay_routes": lay["relay"] / "routes.toml",
            "relay_dir": lay["relay"],
            "relay_config": home / ".config" / "nemo-relay" / "config.toml",
            "relay_plugins": home / ".config" / "nemo-relay" / "plugins.toml",
            "relay_atof_dir": home / ".local" / "share" / "nemo-relay" / "atof",
            "relay_atif_dir": home / ".local" / "share" / "nemo-relay" / "atif",
            "dynamo_dir": lay["dynamo"],
            "dynamo_compose": lay["dynamo"] / "docker-compose.yml",
            "skills_dir": lay["skills"],                     # SkillEvaluator: one sub-directory per skill (SKILL.md)
            "skillevaluator_dir": lay["skillevaluator"],     # the lab installer (install.sh), display only
            "ollama_dir": lay["ollama"],                     # the embeddings model's compose dir, display only
            "data_designer_dir": lay["data_designer"],       # the lab installer + key minter, display only
            "scripts_dir": self.dd_scripts_dir,              # Data Designer dataset config scripts (console-owned)
            "datasets_root": self.dd_datasets_root,          # where `create` writes the user's dataset copies
            "dd_config_dir": self.dd_config_dir,             # the tool's own dir (model_providers/model_configs.yaml)
            "dd_key_file": home / ".config" / "data-designer" / "litellm.key",   # LITELLM_DD_KEY source; EXISTENCE only
            # addendum 9.8 — key files are checked for EXISTENCE only, never read
            "openshell_runner_dir": runner,
            "openshell_gateway_config": runner / "config" / "gateway.toml",
            "openshell_policy": runner / "policy.yaml",
            "openshell_compose": runner / "docker-compose.yml",
            "observability_dir": lay["observability"],
            "observability_compose": lay["observability"] / "docker-compose.yml",
            "dcgm_counters": lay["observability"] / "dcgm" / "counters.csv",
            "aiperf_dir": lay["aiperf"],
            "aiperf_profiles": lay["aiperf"] / "profiles.yaml",
            "aiperf_key_file": home / ".config" / "aiperf" / "litellm.key",
            "guardrails_dir": lay["guardrails"],
            "guardrails_compose": lay["guardrails"] / "docker-compose.yml",
            "guardrails_config_dir": lay["guardrails"] / "config",
            "evaluator_dir": lay["evaluator"],
            "evaluator_suites": lay["evaluator"] / "suites.yaml",
            "evaluator_key_file": home / ".config" / "nemo-evaluator" / "litellm.key",
        }
