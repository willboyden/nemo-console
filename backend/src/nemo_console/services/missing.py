"""Stand-in for an adapter whose module is absent or failed to construct: health=unknown + a Problem, never a crash."""
from __future__ import annotations

import time
from collections.abc import AsyncIterator
from pathlib import Path

from nemo_console.adapters.base import AdapterContext, SystemAdapter
from nemo_console.domain.models import (
    ActionResult,
    ActionSpec,
    ApplyResult,
    ConfigFile,
    ConfigFileSpec,
    MetricsSnapshot,
    Problem,
    SystemEvent,
    SystemId,
    SystemStatus,
    ValidationResult,
)


class MissingAdapter(SystemAdapter):
    def __init__(self, system: SystemId, ctx: AdapterContext, reason: str) -> None:
        super().__init__(ctx)
        self.id = system
        self.label = system.capitalize()
        self.reason = reason

    async def status(self) -> SystemStatus:
        return SystemStatus(
            system=self.id, health="unknown", summary="adapter not available", checked_at=time.time(),
            problems=[Problem(severity="error", code="console.adapter.missing",
                              message=f"adapter for {self.id} could not be loaded: {self.reason[:200]}",
                              fix="reinstall nemo-console (uv sync --frozen) and restart it")])

    async def metrics(self) -> MetricsSnapshot:
        return MetricsSnapshot(system=self.id, ts=time.time(), available=False, error="adapter not available")

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        return []

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        return
        yield  # pragma: no cover  (makes this an async generator)

    def config_files(self) -> list[ConfigFileSpec]:
        return []

    def config_path(self, file_id: str) -> Path:
        raise KeyError(file_id)

    async def read_config(self, file_id: str) -> ConfigFile:
        raise KeyError(file_id)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        raise KeyError(file_id)

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        raise KeyError(file_id)

    def actions(self) -> list[ActionSpec]:
        return []

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        raise KeyError(action_id)
