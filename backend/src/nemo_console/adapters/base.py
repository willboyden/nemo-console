"""SystemAdapter port: the single seam between the system-agnostic core and one NeMo system.

LOCKED (contract v1). The core (API, storage, SSE, audit, config store) depends ONLY on this
module and `domain.models`. Adding a system = one adapter file + a registry line; no core or
frontend change, because the UI renders from status/metrics/config/action/param descriptors.

Rules every adapter MUST follow (each is enforced by the core or by tests/contract):
  * Never shell out or touch docker/systemd directly: use `ctx.runner` (an argv-list runner with a
    binary allowlist, timeouts and output caps). Never build a shell string.
  * Never read secret files. Credentials are referenced by env-var NAME only (`ctx.secret_env`),
    resolved inside the runner/http layer; a value must never appear in a return value.
  * Read paths: only the fixed set handed in via `ctx.paths`; resolve + containment-check every path.
  * Failure is data: a down system yields `health="down"` + a Problem with a `fix`, never an exception
    out of `status()` / `metrics()`.
  * Never fabricate a metric: if a canonical key cannot be read, omit it.
"""
from __future__ import annotations

from abc import ABC, abstractmethod
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Protocol

import httpx

from nemo_console.domain.models import (
    ActionResult,
    ActionSpec,
    ApplyResult,
    ConfigFile,
    ConfigFileSpec,
    MetricsSnapshot,
    ParamSpec,
    PlaygroundSpec,
    SystemEvent,
    SystemId,
    SystemStatus,
    ValidationResult,
)


@dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: str          # already redacted and capped by the runner
    stderr: str
    timed_out: bool = False
    duration_ms: int = 0


class CommandRunner(Protocol):
    """Provided by the core. Only allowlisted binaries (docker, systemctl, nemo-relay) run.

    `argv[0]` is the binary; there is no shell. `stdin` (if any) is passed as bytes, never in argv.
    """
    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult: ...


@dataclass(frozen=True)
class AdapterContext:
    """Everything an adapter may touch. Built once by the core from settings; fakes in tests."""
    runner: CommandRunner
    http: httpx.AsyncClient                      # trust_env=False, no redirects followed across hosts
    urls: dict[str, str]                         # named base URLs, e.g. {"switchyard": "http://127.0.0.1:4010"}
    paths: dict[str, Path]                       # named ABSOLUTE paths, config-only (repo files, state dirs)
    secret_env: dict[str, str] = field(default_factory=dict)   # logical name -> ENV VAR NAME (never a value)
    state_dir: Path = Path(".")                  # console-owned writable dir (0700); adapter may create subdirs


class SystemAdapter(ABC):
    id: SystemId
    label: str

    def __init__(self, ctx: AdapterContext) -> None:
        self.ctx = ctx

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    @abstractmethod
    async def status(self) -> SystemStatus: ...

    @abstractmethod
    async def metrics(self) -> MetricsSnapshot: ...

    @abstractmethod
    async def recent_events(self, limit: int = 100) -> list[SystemEvent]: ...

    @abstractmethod
    def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        """Live tail. Must terminate promptly when the consumer cancels; bounded memory."""

    # ── configure: files ────────────────────────────────────────────────────────────────────
    @abstractmethod
    def config_files(self) -> list[ConfigFileSpec]: ...

    @abstractmethod
    def config_path(self, file_id: str) -> Path:
        """Absolute path of an ALLOWLISTED file; KeyError for an unknown id. The core does the
        compare-and-swap, backup and atomic write — the adapter never writes a config file."""

    @abstractmethod
    async def read_config(self, file_id: str) -> ConfigFile: ...

    @abstractmethod
    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        """Validate CANDIDATE content without touching the live file (temp copy or stdin).
        Set `authoritative=False` and explain in `note` when the validator is only a proxy."""

    @abstractmethod
    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        """Called by the core AFTER it wrote the new content. Restart if asked, then health-check.
        Return post_check="unhealthy" to make the core restore the backup and call this again."""

    # ── configure: structured parameters (optional; Dynamo uses it) ─────────────────────────
    def param_catalog(self) -> list[ParamSpec]:
        return []

    async def preview_params(self, values: dict[str, object]) -> str:
        """Return the exact rendered artifact (e.g. a compose override) for these values. Pure."""
        raise NotImplementedError

    async def apply_params(self, values: dict[str, object]) -> ApplyResult:
        raise NotImplementedError

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    @abstractmethod
    def actions(self) -> list[ActionSpec]: ...

    @abstractmethod
    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        """Unknown id -> KeyError. Params are pre-validated against the ActionSpec by the core."""

    # ── interact ────────────────────────────────────────────────────────────────────────────
    def playground(self) -> PlaygroundSpec | None:
        return None

    def playground_url(self) -> str | None:
        """Absolute URL the core POSTs a playground request to (core adds the credential)."""
        return None
