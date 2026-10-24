"""Shared fakes: adapters, runner, http. Everything offline; no real docker, network, or user files."""
from __future__ import annotations

import time
from collections.abc import AsyncIterator, Iterator
from pathlib import Path
from typing import Any

import httpx
import pytest
import respx

from nemo_console.adapters.base import AdapterContext, CommandResult, SystemAdapter
from nemo_console.config import NO_USER_ENV, Settings
from nemo_console.container import Container, build_container
from nemo_console.domain.models import (
    ActionResult,
    ActionSpec,
    ApplyResult,
    ConfigFile,
    ConfigFileSpec,
    Diagnostic,
    MetricPoint,
    MetricsSnapshot,
    ParamSpec,
    PlaygroundSpec,
    Problem,
    SystemEvent,
    SystemId,
    SystemStatus,
    ValidationResult,
)
from nemo_console.main import create_app
from nemo_console.services.common import KNOWN, sha256_hex

FAKE_SECRET = "sk-FAKEFAKEFAKE1234567890abcdef"
BASE = "http://127.0.0.1:8793"


@pytest.fixture(autouse=True)
def _isolate_env(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Iterator[None]:
    """Never read the developer's real env file / home; clear NEMO_CONSOLE_* from the environment."""
    import os
    for k in list(os.environ):
        if k.startswith("NEMO_CONSOLE_"):
            monkeypatch.delenv(k)
    monkeypatch.setenv(NO_USER_ENV, "1")
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.delenv("LITELLM_API_KEY", raising=False)
    yield
    KNOWN._values.clear()


class FakeRunner:
    def __init__(self) -> None:
        self.calls: list[list[str]] = []
        self.result = CommandResult(0, "running\n", "")

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append(argv)
        return self.result


class FakeAdapter(SystemAdapter):
    label = "Fake"

    def __init__(self, ctx: AdapterContext, system: SystemId, cfg_file: Path | None = None) -> None:
        super().__init__(ctx)
        self.id = system
        self.cfg_file = cfg_file
        self.health = "up"
        self.events: list[SystemEvent] = []
        self.follow: list[SystemEvent] = []
        self.follow_block = False
        self.post_calls: list[bool] = []
        self.status_error: Exception | None = None
        self.editable = True
        self.actions_run: list[tuple[str, dict[str, object]]] = []
        self.action_output = "done"
        self.post_health_override: str | None = None
        self.params_applied: list[dict[str, object]] = []

    async def status(self) -> SystemStatus:
        if self.status_error:
            raise self.status_error
        return SystemStatus(system=self.id, health=self.health, summary="fake",  # type: ignore[arg-type]
                            checked_at=time.time(),
                            problems=[Problem(severity="warn", code="fake", message="Bearer abcdefgh12345678 leaked")])

    async def metrics(self) -> MetricsSnapshot:
        return MetricsSnapshot(system=self.id, ts=time.time(), available=True, points=[
            MetricPoint(key="fake.requests_total", value=5),
            MetricPoint(key="fake.tokens", value=2, labels={"kind": "prompt"}),
            MetricPoint(key="fake.tokens", value=3, labels={"kind": "completion"})])

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        return self.events[:limit]

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        for e in self.follow:
            yield e
        if self.follow_block:
            import asyncio
            await asyncio.sleep(3600)

    def config_files(self) -> list[ConfigFileSpec]:
        if self.cfg_file is None:
            return []
        return [ConfigFileSpec(id="routes", label="Routes", description="d", display_path="routes.toml", format="toml",
                               editable=self.editable, why_readonly=None if self.editable else "policy file",
                               validators=["fake"])]

    def config_path(self, file_id: str) -> Path:
        if file_id != "routes" or self.cfg_file is None:
            raise KeyError(file_id)
        return self.cfg_file

    async def read_config(self, file_id: str) -> ConfigFile:
        assert self.cfg_file is not None
        data = self.cfg_file.read_bytes()
        return ConfigFile(spec=self.config_files()[0], content=data.decode(), sha256="adapter-sha-ignored", mtime=0)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        bad = "INVALID" in content
        return ValidationResult(ok=not bad, validator="fake", authoritative=True,
                                diagnostics=[Diagnostic(severity="error", message="nope")] if bad else [])

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        self.post_calls.append(restart)
        assert self.cfg_file is not None
        unhealthy = "BAD" in self.cfg_file.read_text()
        if self.post_health_override == "raise":
            raise RuntimeError("boom")
        return ApplyResult(ok=not unhealthy, restarted=restart, post_check="unhealthy" if unhealthy else "healthy",
                           detail="fake post_apply")

    def param_catalog(self) -> list[ParamSpec]:
        return [ParamSpec(key="gpu_util", label="GPU util", help="h", type="float", min=0.1, max=0.95),
                ParamSpec(key="mode", label="Mode", help="h", type="enum", choices=["a", "b"]),
                ParamSpec(key="n", label="N", help="h", type="int", min=1, max=10)]

    async def preview_params(self, values: dict[str, object]) -> str:
        return f"rendered {values}"

    async def apply_params(self, values: dict[str, object]) -> ApplyResult:
        self.params_applied.append(values)
        return ApplyResult(ok=True, detail="applied")

    def actions(self) -> list[ActionSpec]:
        return [ActionSpec(id="status", label="Status", description="d", danger="safe", role="viewer"),
                ActionSpec(id="safe_admin", label="Safe admin", description="d", danger="safe"),
                ActionSpec(id="restart", label="Restart", description="d", danger="disruptive",
                           params=[ParamSpec(key="timeout", label="t", help="h", type="int", min=1, max=60)]),
                ActionSpec(id="reset", label="Reset", description="d", danger="destructive")]

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        if action_id not in {a.id for a in self.actions()}:
            raise KeyError(action_id)
        self.actions_run.append((action_id, params))
        return ActionResult(ok=True, output=self.action_output)

    def playground(self) -> PlaygroundSpec | None:
        return PlaygroundSpec(wire="openai_chat", models=["m1"], default_model="m1")

    def playground_url(self) -> str | None:
        return "http://127.0.0.1:4010/v1/chat/completions"


class Env:
    def __init__(self, tmp: Path, mock: respx.MockRouter) -> None:
        self.tmp = tmp
        self.mock = mock
        self.cfg_file = tmp / "cfg" / "routes.toml"
        self.cfg_file.parent.mkdir()
        self.cfg_file.write_text('a = 1\n')
        self.cfg_file.chmod(0o640)
        self.settings = Settings(state_dir=tmp / "state", frontend_dir=tmp / "front", background_tasks=False, trust_loopback=True,
                                 sse_max_connections=2, sse_idle_timeout_s=0.3, sse_heartbeat_s=0.1)
        self.runner = FakeRunner()
        self.http = httpx.AsyncClient(trust_env=False)
        ctx = AdapterContext(runner=self.runner, http=self.http, urls={}, paths={}, state_dir=tmp / "state")
        self.adapters: dict[SystemId, FakeAdapter] = {
            "switchyard": FakeAdapter(ctx, "switchyard", self.cfg_file),
            "relay": FakeAdapter(ctx, "relay"),
            "dynamo": FakeAdapter(ctx, "dynamo"),
            "skillevaluator": FakeAdapter(ctx, "skillevaluator"),
            # addendum 9.8: the core tests fake the five new systems too (their own suites test the real adapters)
            "openshell": FakeAdapter(ctx, "openshell"),
            "dcgm": FakeAdapter(ctx, "dcgm"),
            "aiperf": FakeAdapter(ctx, "aiperf"),
            "guardrails": FakeAdapter(ctx, "guardrails"),
            "evaluator": FakeAdapter(ctx, "evaluator")}
        self.container: Container = build_container(self.settings, adapters=dict(self.adapters), runner=self.runner,  # type: ignore[arg-type]
                                                    http=self.http)
        self.app = create_app(self.settings, self.container)
        self.mock.get("http://127.0.0.1:4000/health/liveliness").respond(200)

    def client(self, *, client_host: str = "127.0.0.1", token: str | None = None, csrf: bool = True,
               headers: dict[str, str] | None = None) -> httpx.AsyncClient:
        h = {"X-Console": "1"} if csrf else {}
        if token:
            h["Authorization"] = f"Bearer {token}"
        h.update(headers or {})
        return httpx.AsyncClient(transport=httpx.ASGITransport(app=self.app, client=(client_host, 5000)),
                                 base_url=BASE, headers=h)

    def sha(self) -> str:
        return sha256_hex(self.cfg_file.read_bytes())


@pytest.fixture
def env(tmp_path: Path) -> Iterator[Env]:
    with respx.mock(assert_all_called=False, assert_all_mocked=False) as mock:
        yield Env(tmp_path, mock)


def apply_body(content: str, base: str, **kw: Any) -> dict[str, Any]:
    return {"content": content, "base_sha256": base, "restart": True, "confirm": True, **kw}
