"""Console settings: env (NEMO_CONSOLE_*) plus an optional user env file. Secrets are referenced by env-var NAME only."""
from __future__ import annotations

import os
from importlib import metadata
from pathlib import Path

from pydantic import Field, field_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

try:
    __version__ = metadata.version("nemo-console")
except metadata.PackageNotFoundError:  # pragma: no cover
    __version__ = "0.1.2"

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
}
# router alias -> engine container (the author's deployment). 2026-09-26: the lab's default engine and capable tier
# moved to qwen3.8-flash-next (SGLang TP2 across both cards). The 27B entry stays: direct-27b still targets it and
# it can still be selected, it is just dark by default (only a stage_router capable target raises capable_dark).
DEFAULT_ENGINE_PROBES = {
    "qwen3.8-flash-next": "ai-lab-sglang-flashnext-flashnext-1",
    "qwen3.8-27b-1m": "ai-lab-vllm-qwen38-vllm-1",
}
# engine container -> the ONE command, run from the repo root, that starts it correctly. A bare `docker compose up -d`
# is wrong for both of these: flashnext must start BEFORE lightning (flashnext.sh orders it; next to a running
# lightning the compose up fails "memory capacity is unbalanced"), and the 27B needs Workstation-card VRAM that
# flashnext holds (use-model.sh stops flashnext first). A container not listed gets the generic compose-dir hint.
DEFAULT_ENGINE_START = {
    "ai-lab-sglang-flashnext-flashnext-1": "inference/sglang-flashnext/flashnext.sh up",
    "ai-lab-vllm-qwen38-vllm-1": "scripts/use-model.sh qwen3.8-27b-1m   # exclusive: this also stops flashnext",
}
# Dynamo card model (2026-10-06, addendum 9.4). In the author's deployment Dynamo is OPT-IN and off by default: the
# default model (qwen3.8-flash-next, SGLang TP2) holds ~60 GB of BOTH cards, so the worker (0.5 x the Workstation card)
# cannot start beside it. Commands run from the repo root and are DISPLAY ONLY (problem fixes); the console never runs
# them. Lists REPLACE the defaults when set (NEMO_CONSOLE_DYNAMO_EXCLUSIVE_ENGINES='["a","b"]').
DEFAULT_DYNAMO_START = "scripts/use-model.sh qwen3.8-27b-dynamo"      # frees the card in the right order, starts Dynamo
DEFAULT_MODEL_START = "scripts/use-model.sh qwen3.8-flash-next"       # stops Dynamo, restores the default model
# running on the worker's card, these cannot coexist with it (VRAM): `up` is refused while one runs
DEFAULT_DYNAMO_EXCLUSIVE = ["ai-lab-sglang-flashnext-flashnext-1", "ai-lab-vllm-qwen38-vllm-1"]
# allowed beside the worker on its card (ollama holds no VRAM until a model loads)
DEFAULT_DYNAMO_COEXISTING = ["ai-lab-vllm-nemotron-lightning-nemotron-lightning-1", "ai-lab-ollama-ollama-1"]
# GPU-visible but holding no meaningful VRAM (monitoring)
DEFAULT_DYNAMO_IGNORED = ["ai-lab-observability-dcgm-exporter-1"]
# what stops answering while Dynamo holds the card (the exclusive engines are stopped then); named in dynamo.holds_card
DEFAULT_DYNAMO_DARK = ["qwen3.8-flash-next", "qwen3.8-flash-next-quick", "auto-local capable tier (Switchyard and Relay)",
                       "auto-flashnext capable tier", "direct-flashnext (Switchyard and Relay)"]
DEFAULT_KEY_ENV = {"switchyard": "LITELLM_API_KEY"}
REPO_MARKER = Path("inference") / "dynamo" / "docker-compose.yml"


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
    playground_max_tokens: int = 4096
    revisions_keep: int = 50

    otlp_endpoint: str | None = None          # e.g. http://localhost:4317 ; needs the optional opentelemetry packages
    extra_bin_dirs: list[str] = ["~/.local/bin"]   # searched for allowlisted binaries (nemo-relay)

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
        }
