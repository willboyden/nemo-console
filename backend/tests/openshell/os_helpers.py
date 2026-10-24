"""Test helpers for the OpenShell adapter: a fake runner answering with CAPTURED CLI/docker output (fixtures/*,
captured live 2026-10-24 against CLI 0.0.71 + gateway 0.0.77; files named HAND-WRITTEN are not captures) and a
runner directory laid out like clients/data-designer/runner. Everything offline: no CLI, no docker, no network."""
from __future__ import annotations

import inspect
import shutil
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path

import httpx

from nemo_console.adapters.base import AdapterContext, CommandResult

FIXTURES = Path(__file__).parent / "fixtures"
RUNNER_SRC = FIXTURES / "runner"          # copies of clients/data-designer/runner (home dir redacted)


def fx(name: str) -> str:
    return (FIXTURES / name).read_text()


Responder = CommandResult | Callable[[list[str]], CommandResult | Awaitable[CommandResult]]


@dataclass
class FakeRunner:
    """Matches argv by prefix (longest first). Unmatched argv -> rc 127 so a missing stub is loud, not silent."""

    rules: list[tuple[list[str], Responder]] = field(default_factory=list)
    calls: list[list[str]] = field(default_factory=list)

    def on(self, prefix: list[str], result: Responder) -> FakeRunner:
        self.rules = [r for r in self.rules if r[0] != prefix] + [(prefix, result)]
        return self

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append(list(argv))
        for prefix, res in sorted(self.rules, key=lambda r: -len(r[0])):
            if argv[: len(prefix)] == prefix:
                out = res(argv) if callable(res) else res
                return await out if inspect.isawaitable(out) else out
        return CommandResult(returncode=127, stdout="", stderr=f"no fake for {argv}")

    def count(self, prefix: list[str]) -> int:
        return sum(1 for c in self.calls if c[: len(prefix)] == prefix)


def ok(stdout: str = "", rc: int = 0, stderr: str = "") -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr=stderr)


GW = ["openshell", "-g", "openshell"]
LIVE_SANDBOX_LIST = fx("sandbox-list.json")


@dataclass
class World:
    """The mutable 'machine' behind the fake runner: tests change it between polls."""
    inspect_out: str = field(default_factory=lambda: fx("docker-inspect-running.txt"))
    inspect_err: str = ""
    inspect_rc: int = 0
    internal: str = "true"
    status_out: str = field(default_factory=lambda: fx("status-connected.raw.txt"))
    sandboxes: str = "[]"
    sandbox_rc: int = 0
    cli_version: str = field(default_factory=lambda: fx("version.txt"))
    cli_rc: int = 0


def make_runner(w: World | None = None) -> tuple[FakeRunner, World]:
    w = w or World()
    r = FakeRunner()
    r.on(["openshell", "--version"], lambda a: ok(w.cli_version, rc=w.cli_rc))
    r.on(["docker", "inspect", "--format"], lambda a: ok(w.inspect_out, rc=w.inspect_rc, stderr=w.inspect_err))
    r.on(["docker", "network", "inspect"], lambda a: ok(w.internal + "\n") if w.internal in ("true", "false")
         else ok("", rc=1, stderr="Error response from daemon: network openshell-docker not found"))
    r.on([*GW, "status"], lambda a: ok(w.status_out))
    r.on([*GW, "sandbox", "list"], lambda a: ok(w.sandboxes, rc=w.sandbox_rc,
                                                stderr="" if w.sandbox_rc == 0 else "transport error"))
    r.on([*GW, "provider", "list"], ok(fx("provider-list.json")))
    r.on(["openshell", "doctor", "check"], ok(fx("doctor-check.txt")))
    return r, w


def runner_dir(tmp: Path) -> Path:
    """A copy of the lab's runner files (gateway.toml, policy.yaml, profile, compose) under tmp."""
    rd = tmp / "runner"
    if rd.is_dir():
        return rd
    (rd / "config").mkdir(parents=True)
    for rel in ("config/gateway.toml", "policy.yaml", "profile-lab-router.yaml", "docker-compose.yml"):
        shutil.copy(RUNNER_SRC / rel, rd / rel)
    return rd


def make_ctx(tmp: Path, runner: object, http: httpx.AsyncClient, *, with_paths: bool = True) -> AdapterContext:
    paths: dict[str, Path] = {}
    if with_paths:
        rd = runner_dir(tmp)
        paths = {"openshell_runner_dir": rd, "openshell_gateway_config": rd / "config" / "gateway.toml",
                 "openshell_policy": rd / "policy.yaml", "openshell_compose": rd / "docker-compose.yml"}
    return AdapterContext(runner=runner, http=http, urls={}, paths=paths, state_dir=tmp / "state")  # type: ignore[arg-type]
