"""Test helpers: a scriptable fake runner (argv predicates), fixture loaders."""
from __future__ import annotations

from collections.abc import Callable
from pathlib import Path

from nemo_console.adapters.base import CommandResult

FIX = Path(__file__).parent / "fixtures"
EXPORTER = "http://127.0.0.1:9400"
PROM = "http://127.0.0.1:9090"
CONTAINER = "ai-lab-observability-dcgm-exporter-1"


def fx(name: str) -> str:
    return (FIX / name).read_text()


class FakeRunner:
    """Matches argv by predicate (first match wins); records every call."""

    def __init__(self) -> None:
        self.calls: list[list[str]] = []
        self.rules: list[tuple[Callable[[list[str]], bool], Callable[[list[str]], CommandResult]]] = []

    def on(self, pred: Callable[[list[str]], bool],
           result: CommandResult | Callable[[list[str]], CommandResult]) -> None:
        fn = result if callable(result) else (lambda _a, r=result: r)  # type: ignore[misc]
        self.rules.append((pred, fn))

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append(argv)
        for pred, fn in self.rules:
            if pred(argv):
                return fn(argv)
        return CommandResult(returncode=127, stdout="", stderr="no rule")

    def argvs(self, *prefix: str) -> list[list[str]]:
        return [a for a in self.calls if a[: len(prefix)] == list(prefix)]


def ok(stdout: str = "", stderr: str = "") -> CommandResult:
    return CommandResult(returncode=0, stdout=stdout, stderr=stderr)


def bad(stderr: str = "", stdout: str = "", rc: int = 1, timed_out: bool = False) -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr=stderr, timed_out=timed_out)


def is_inspect(a: list[str]) -> bool:
    return a[:2] == ["docker", "inspect"]


def is_image_inspect(a: list[str]) -> bool:
    return a[:3] == ["docker", "image", "inspect"]


def is_restart(a: list[str]) -> bool:
    return a[:2] == ["docker", "restart"]


def is_logs(a: list[str]) -> bool:
    return a[:2] == ["docker", "logs"]


def is_run(a: list[str]) -> bool:
    return a[:2] == ["docker", "run"]


def is_rm(a: list[str]) -> bool:
    return a[:3] == ["docker", "rm", "-f"]
