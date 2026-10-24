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

    @field_validator("dcgm_exporter_container", "guardrails_container")
    @classmethod
    def _container_name(cls, v: str) -> str:
        """Addendum 9.8: these reach `docker inspect/restart` argv; a leading '-' would read as an option."""
        if not CONTAINER_NAME_RE.fullmatch(v):
            raise ValueError(f"not a container name: {v!r}")
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

    def repo_root_problem(self) -> str | None:
        if (self.repo_root / REPO_MARKER).is_file():
            return None
        return (f"repo root {self.repo_root} does not contain {REPO_MARKER}; "
                "set NEMO_CONSOLE_REPO_ROOT to the lab repository checkout (see docs/OPERATIONS.md)")

    def resolved_paths(self) -> dict[str, Path]:
        """Named ABSOLUTE config-only paths for the adapters (see docs: proposal, additive)."""
        r = self.repo_root
        home = Path.home()
        return {
            "repo": r,
            "state": self.state_dir,
            "switchyard_dir": r / "inference" / "switchyard",
            "switchyard_routes": r / "inference" / "switchyard" / "routes.toml",
            "switchyard_compose": r / "inference" / "switchyard" / "docker-compose.yml",
            "relay_routes": r / "clients" / "nemo-relay" / "routes.toml",
            "relay_dir": r / "clients" / "nemo-relay",
            "relay_config": home / ".config" / "nemo-relay" / "config.toml",
            "relay_plugins": home / ".config" / "nemo-relay" / "plugins.toml",
            "relay_atof_dir": home / ".local" / "share" / "nemo-relay" / "atof",
            "relay_atif_dir": home / ".local" / "share" / "nemo-relay" / "atif",
            "dynamo_dir": r / "inference" / "dynamo",
            "dynamo_compose": r / "inference" / "dynamo" / "docker-compose.yml",
            "skills_dir": r / ".claude" / "skills",          # SkillEvaluator: one sub-directory per skill (SKILL.md)
            "scripts_dir": self.dd_scripts_dir,              # Data Designer dataset config scripts (console-owned)
            "datasets_root": self.dd_datasets_root,          # where `create` writes the user's dataset copies
            "dd_config_dir": self.dd_config_dir,             # the tool's own dir (model_providers/model_configs.yaml)
            "dd_key_file": home / ".config" / "data-designer" / "litellm.key",   # LITELLM_DD_KEY source; EXISTENCE only
            # addendum 9.8 — key files are checked for EXISTENCE only, never read
            "openshell_runner_dir": r / "clients" / "data-designer" / "runner",
            "openshell_gateway_config": r / "clients" / "data-designer" / "runner" / "config" / "gateway.toml",
            "openshell_policy": r / "clients" / "data-designer" / "runner" / "policy.yaml",
            "openshell_compose": r / "clients" / "data-designer" / "runner" / "docker-compose.yml",
            "observability_dir": r / "observability",
            "observability_compose": r / "observability" / "docker-compose.yml",
            "dcgm_counters": r / "observability" / "dcgm" / "counters.csv",
            "aiperf_dir": r / "clients" / "aiperf",
            "aiperf_profiles": r / "clients" / "aiperf" / "profiles.yaml",
            "aiperf_key_file": home / ".config" / "aiperf" / "litellm.key",
            "guardrails_dir": r / "security" / "nemo-guardrails",
            "guardrails_compose": r / "security" / "nemo-guardrails" / "docker-compose.yml",
            "guardrails_config_dir": r / "security" / "nemo-guardrails" / "config",
            "evaluator_dir": r / "clients" / "nemo-evaluator",
            "evaluator_suites": r / "clients" / "nemo-evaluator" / "suites.yaml",
            "evaluator_key_file": home / ".config" / "nemo-evaluator" / "litellm.key",
        }
