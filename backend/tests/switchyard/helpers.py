"""Test helpers: a scriptable fake runner, and a tiny REAL subprocess runner for the live smoke."""
from __future__ import annotations

import asyncio
import os
from collections.abc import Callable
from pathlib import Path

from nemo_console.adapters.base import CommandResult

FIX = Path(__file__).parent / "fixtures"
INSPECT_OK = "running|healthy|ai-lab-switchyard:0.3.0|ai-lab-switchyard|switchyard\n"


def fx(name: str) -> str:
    return (FIX / name).read_text()


class FakeRunner:
    """Matches argv by predicate; records every call (argv, stdin, cwd)."""

    def __init__(self) -> None:
        self.calls: list[tuple[list[str], bytes | None, Path | None]] = []
        self.rules: list[tuple[Callable[[list[str]], bool], Callable[[list[str], bytes | None], CommandResult]]] = []

    def on(self, pred: Callable[[list[str]], bool],
           result: CommandResult | Callable[[list[str], bytes | None], CommandResult]) -> None:
        fn = result if callable(result) else (lambda _a, _s, r=result: r)  # type: ignore[misc]
        self.rules.append((pred, fn))

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append((argv, stdin, cwd))
        for pred, fn in self.rules:
            if pred(argv):
                return fn(argv, stdin)
        return CommandResult(returncode=127, stdout="", stderr="no rule")


def ok(stdout: str = "", stderr: str = "") -> CommandResult:
    return CommandResult(returncode=0, stdout=stdout, stderr=stderr)


def fail(stderr: str = "", stdout: str = "", rc: int = 1) -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr=stderr)


def is_inspect(a: list[str]) -> bool:
    return a[:2] == ["docker", "inspect"]


def is_tail(a: list[str]) -> bool:
    return a[:2] == ["docker", "exec"] and "tail" in a


def is_dry(a: list[str]) -> bool:
    return a[:2] == ["docker", "run"]


def is_compose(a: list[str]) -> bool:
    return a[:2] == ["docker", "compose"]


class RealRunner:
    """Live smoke only: argv list, no shell, DOCKER_CONTEXT=rootless, timeout + output cap."""

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        assert argv[0] == "docker"
        env = {**os.environ, "DOCKER_CONTEXT": "rootless"}
        proc = await asyncio.create_subprocess_exec(
            *argv, stdin=asyncio.subprocess.PIPE if stdin is not None else asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, cwd=cwd, env=env)
        try:
            out, err = await asyncio.wait_for(proc.communicate(stdin), timeout)
        except TimeoutError:
            proc.kill()
            await proc.wait()
            return CommandResult(returncode=-9, stdout="", stderr="timeout", timed_out=True)
        return CommandResult(returncode=proc.returncode or 0, stdout=out[:max_bytes].decode("utf-8", "replace"),
                             stderr=err[:max_bytes].decode("utf-8", "replace"))
