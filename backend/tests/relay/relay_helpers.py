"""Test helpers for the Relay adapter: fake runner, context builder, and a tiny READ-ONLY real runner."""
from __future__ import annotations

import asyncio
import os
import shutil
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

import httpx

from nemo_console.adapters.base import AdapterContext, CommandResult

FIXTURES = Path(__file__).parent / "fixtures"


def fx(name: str) -> str:
    return (FIXTURES / name).read_text()


Responder = CommandResult | Callable[[list[str]], CommandResult]


@dataclass
class FakeRunner:
    """Matches argv by prefix (longest first). Unmatched argv -> rc 127 so a missing stub is loud, not silent."""

    rules: list[tuple[list[str], Responder]] = field(default_factory=list)
    calls: list[list[str]] = field(default_factory=list)

    def on(self, prefix: list[str], result: Responder) -> FakeRunner:
        self.rules = [r for r in self.rules if r[0] != prefix] + [(prefix, result)]   # later stub wins
        return self

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append(list(argv))
        for prefix, res in sorted(self.rules, key=lambda r: -len(r[0])):
            if argv[: len(prefix)] == prefix:
                return res(argv) if callable(res) else res
        return CommandResult(returncode=127, stdout="", stderr=f"no fake for {argv}")


def ok(stdout: str = "", rc: int = 0) -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr="")


def standard_runner(show: str | None = None) -> FakeRunner:
    r = FakeRunner()
    r.on(["systemctl", "--user", "--timestamp=unix", "show"], ok(show if show is not None else fx("systemctl_show_active.txt")))
    r.on(["nemo-relay", "--version"], ok(fx("version.txt")))
    r.on(["nemo-relay", "plugins", "list", "--json"], ok(fx("plugins_list.json")))
    r.on(["nemo-relay", "plugins", "validate"], ok(fx("plugins_validate.json")))
    r.on(["nemo-relay", "agents", "--json"], ok(fx("agents.json")))
    r.on(["nemo-relay", "doctor", "--offline", "--json"], ok(fx("doctor_offline.json")))
    r.on(["systemctl", "--user", "status"], ok("* nemo-relay-gateway.service - fake\n  Active: active (running)\n", 0))
    r.on(["systemctl", "--user", "restart"], ok())
    return r


def build_tree(tmp: Path, *, plugins_toml: str | None = None, atof_lines: str | None = None) -> dict[str, Path]:
    """Create a fake home layout under tmp and return the ctx.paths mapping."""
    cfg = tmp / "cfg"
    atof = tmp / "atof"
    atif = tmp / "atif"
    for d in (cfg, atof, atif):
        d.mkdir(parents=True, exist_ok=True)
    (cfg / "routes.toml").write_text(fx("routes.toml"))
    (cfg / "plugins.toml").write_text(plugins_toml if plugins_toml is not None else fx("plugins.toml"))
    (cfg / "config.toml").write_text(fx("config.toml"))
    os.utime(cfg / "plugins.toml", (1_790_000_000, 1_790_000_000))     # older than the fixture unit's start time
    (atof / "relay-events.jsonl").write_text("")
    (atof / "relay-events.jsonl.1").write_text(atof_lines if atof_lines is not None else fx("atof_sample.jsonl"))
    (atif / "nemo-relay-atif-aaa.json").write_text("{}")
    (atif / "nemo-relay-atif-bbb.json").write_text("{}")
    (atif / "notes.txt").write_text("not a trajectory")
    return {"relay_routes": cfg / "routes.toml", "relay_plugins": cfg / "plugins.toml", "relay_config": cfg / "config.toml",
            "relay_atof_dir": atof, "relay_atif_dir": atif}


def make_ctx(tmp: Path, runner: object, http: httpx.AsyncClient, **kw: object) -> AdapterContext:
    paths = build_tree(tmp, **kw)  # type: ignore[arg-type]
    return AdapterContext(runner=runner, http=http, urls={"relay": "http://127.0.0.1:4040", "litellm": "http://127.0.0.1:4000"},  # type: ignore[arg-type]
                          paths=paths, state_dir=tmp / "state")


class ReadOnlyRealRunner:
    """Live-smoke runner: real subprocesses, argv only, READ-ONLY commands allowlisted (no restart, ever)."""

    ALLOWED = (
        ["systemctl", "--user", "--timestamp=unix", "show"],
        ["nemo-relay", "--version"],
        ["nemo-relay", "plugins", "list", "--json"],
    )

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        if not any(argv[: len(a)] == list(a) for a in self.ALLOWED) or shutil.which(argv[0]) is None:
            return CommandResult(returncode=126, stdout="", stderr="not allowed in the live smoke")
        t0 = time.monotonic()
        proc = await asyncio.create_subprocess_exec(*argv, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        try:
            out, err = await asyncio.wait_for(proc.communicate(), timeout)
        except TimeoutError:
            proc.kill()
            return CommandResult(returncode=124, stdout="", stderr="timeout", timed_out=True)
        return CommandResult(returncode=proc.returncode or 0, stdout=out[:max_bytes].decode(), stderr=err[:4096].decode(),
                             duration_ms=int((time.monotonic() - t0) * 1000))
