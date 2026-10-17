"""Test helpers: scriptable fake runner, fixture loaders, and a tiny REAL read-only runner for the live smoke."""
from __future__ import annotations

import asyncio
import json
from collections.abc import Callable
from pathlib import Path
from typing import Any

from nemo_console.adapters.base import CommandResult

FIX = Path(__file__).parent / "fixtures"
WORKER = "ai-lab-dynamo-dynamo-worker-1"
FRONTEND = "ai-lab-dynamo-dynamo-frontend-1"
# The captured containers' compose labels name this directory (the capture host's checkout, anonymised). The adapter
# trusts a label only inside its configured compose directory, so the fake runner maps it to the test's copy.
CAPTURED_DIR = "/home/user/lab/inference/dynamo"


def fx(name: str) -> str:
    return (FIX / name).read_text()


def fxj(name: str) -> Any:
    return json.loads(fx(name))


class FakeRunner:
    """Matches argv by predicate (first match wins); records every call as (argv, cwd). `compose_dir`: where the
    captured labels' CAPTURED_DIR points in this test (rewritten in `docker inspect` output only)."""

    def __init__(self, compose_dir: Path | None = None) -> None:
        self.calls: list[tuple[list[str], Path | None]] = []
        self.rules: list[tuple[Callable[[list[str]], bool], Callable[[list[str]], CommandResult]]] = []
        self.compose_dir = compose_dir

    def on(self, pred: Callable[[list[str]], bool], result: CommandResult | Callable[[list[str]], CommandResult]) -> None:
        fn = result if callable(result) else (lambda _a, r=result: r)
        self.rules.append((pred, fn))

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append((list(argv), cwd))
        for pred, fn in self.rules:
            if pred(argv):
                res = fn(argv)
                if self.compose_dir is not None and argv[:2] == ["docker", "inspect"]:
                    res = CommandResult(res.returncode, res.stdout.replace(CAPTURED_DIR, str(self.compose_dir)),
                                        res.stderr, res.timed_out, res.duration_ms)
                return res
        return CommandResult(returncode=127, stdout="", stderr="no rule")

    def argvs(self) -> list[list[str]]:
        return [c[0] for c in self.calls]


def ok(stdout: str = "", stderr: str = "") -> CommandResult:
    return CommandResult(returncode=0, stdout=stdout, stderr=stderr)


def fail(stderr: str = "", stdout: str = "", rc: int = 1) -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr=stderr)


def inspect_json(*docs: dict[str, Any]) -> CommandResult:
    return ok(json.dumps(list(docs)))


def worker_doc() -> dict[str, Any]:
    d: dict[str, Any] = fxj("worker_inspect.json")[0]
    return d


def frontend_doc() -> dict[str, Any]:
    d: dict[str, Any] = fxj("frontend_inspect.json")[0]
    return d


def is_docker(*prefix: str) -> Callable[[list[str]], bool]:
    return lambda a: a[: len(prefix) + 1] == ["docker", *prefix]


class RealRunner:
    """Read-only live runner: only `docker inspect|exec|logs|ps` and `docker compose ... config` are permitted."""

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        allowed = (argv[:2] in (["docker", "inspect"], ["docker", "exec"], ["docker", "logs"], ["docker", "ps"])
                   or (argv[:2] == ["docker", "compose"] and "config" in argv and "up" not in argv
                       and "down" not in argv))
        if not allowed:
            raise AssertionError(f"live smoke is read-only; refused {argv[:4]}")
        proc = await asyncio.create_subprocess_exec(*argv, stdout=asyncio.subprocess.PIPE,
                                                    stderr=asyncio.subprocess.PIPE, cwd=cwd)
        try:
            out, err = await asyncio.wait_for(proc.communicate(), timeout)
        except TimeoutError:
            proc.kill()
            return CommandResult(returncode=124, stdout="", stderr="timeout", timed_out=True)
        return CommandResult(returncode=proc.returncode or 0, stdout=out[:max_bytes].decode("utf-8", "replace"),
                             stderr=err[:max_bytes].decode("utf-8", "replace"))
