"""Test helpers for the NeMo Evaluator adapter: a fake runner that answers like `evaluator-local` (outputs captured
live on 2026-10-24, paths redacted, in fixtures/) and a context with a suites file. Everything offline."""
from __future__ import annotations

import inspect
import json
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path

import httpx

from nemo_console.adapters.base import AdapterContext, CommandResult

FIXTURES = Path(__file__).parent / "fixtures"
LAUNCHER = "evaluator-local"


def fx(name: str) -> str:
    return (FIXTURES / name).read_text()


Responder = CommandResult | Callable[[list[str]], CommandResult | Awaitable[CommandResult]]


@dataclass
class FakeRunner:
    """Matches argv by prefix (longest first). Unmatched argv -> rc 127 so a missing stub is loud, not silent."""

    rules: list[tuple[list[str], Responder]] = field(default_factory=list)
    calls: list[list[str]] = field(default_factory=list)
    cwds: list[Path | None] = field(default_factory=list)
    timeouts: list[float] = field(default_factory=list)

    def on(self, prefix: list[str], result: Responder) -> FakeRunner:
        self.rules = [r for r in self.rules if r[0] != prefix] + [(prefix, result)]
        return self

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append(list(argv))
        self.cwds.append(cwd)
        self.timeouts.append(timeout)
        for prefix, res in sorted(self.rules, key=lambda r: -len(r[0])):
            if argv[: len(prefix)] == prefix:
                out = res(argv) if callable(res) else res
                return await out if inspect.isawaitable(out) else out
        return CommandResult(returncode=127, stdout="", stderr=f"no fake for {argv}")


def ok(stdout: str = "", rc: int = 0, stderr: str = "") -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr=stderr)


def writes_summary(name: str = "summary_smoke.json", rc: int = 0, stdout: str | None = None
                   ) -> Callable[[list[str]], CommandResult]:
    """`evaluator-local run --suite S --output-dir D`: writes the captured summary.json under D (like the wrapper)."""
    def respond(argv: list[str]) -> CommandResult:
        out = Path(argv[argv.index("--output-dir") + 1])
        out.mkdir(parents=True, exist_ok=True)
        doc = json.loads(fx(name))
        doc["suite"] = argv[argv.index("--suite") + 1]
        (out / "summary.json").write_text(json.dumps(doc))
        # nel's own heavy files exist in a real out dir; the adapter must never read them
        (out / "gsm8k").mkdir(exist_ok=True)
        (out / "gsm8k" / "results.jsonl").write_text('{"prompt": "SECRET-QUESTION", "model_response": "SECRET-ANSWER"}\n')
        return ok(stdout if stdout is not None else fx("nel_tail_smoke.txt"), rc=rc)
    return respond


def standard_runner() -> FakeRunner:
    r = FakeRunner()
    r.on([LAUNCHER, "--version"], ok(fx("version.txt")))
    r.on([LAUNCHER, "cached"], ok(fx("cached_all.json")))
    r.on([LAUNCHER, "suites"], ok("smoke-lightning  aitop/nemotron-lightning  gsm8kx10\n"))
    r.on([LAUNCHER, "run"], writes_summary())
    return r


def make_ctx(tmp: Path, runner: object, http: httpx.AsyncClient, *, suites: str | None = None,
             key_file: bool = True, repo: Path | None = None) -> AdapterContext:
    edir = tmp / "repo" / "clients" / "nemo-evaluator"
    edir.mkdir(parents=True, exist_ok=True)
    spath = edir / "suites.yaml"
    if suites is not None or not spath.exists():
        spath.write_text(suites if suites is not None else fx("suites_good.yaml"))
    key = tmp / "home" / ".config" / "nemo-evaluator" / "litellm.key"
    if key_file:
        key.parent.mkdir(parents=True, exist_ok=True)
        key.write_text("sk-FAKE-not-a-real-key-0123456789\n")     # EXISTENCE only; never read
        key.chmod(0o600)
    else:
        key.unlink(missing_ok=True)
    paths: dict[str, Path] = {"evaluator_dir": edir, "evaluator_suites": spath, "evaluator_key_file": key}
    if repo is not None:
        paths["repo"] = repo
    return AdapterContext(runner=runner, http=http, urls={"litellm": "http://127.0.0.1:4000"},  # type: ignore[arg-type]
                          paths=paths, state_dir=tmp / "state")
