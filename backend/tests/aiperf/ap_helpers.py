"""Test helpers for the AIPerf adapter: a fake runner that writes the REAL aiperf summary JSON (captured live on
2026-10-24 against aitop/nemotron-lightning through the lab router) and a context with the lab's real profiles.yaml.
Everything offline: no network, no real CLI."""
from __future__ import annotations

import inspect
import shutil
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path

import httpx

from nemo_console.adapters.base import AdapterContext, CommandResult

FIXTURES = Path(__file__).parent / "fixtures"
LAUNCHER = "aiperf-local"


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


def artifact_dir(argv: list[str]) -> Path:
    return Path(argv[argv.index("--artifact-dir") + 1])


def writes_records(rc: int = 1) -> Callable[[list[str]], CommandResult]:
    """A run whose every request failed: only the per-request records (captured live: model name unknown to the
    router, HTTP 400) and the error panel on stderr; no summary, exit 1."""
    def respond(argv: list[str]) -> CommandResult:
        out = artifact_dir(argv)
        out.mkdir(parents=True, exist_ok=True)
        shutil.copy(FIXTURES / "failed-records.jsonl", out / "profile_export.jsonl")
        return ok("", rc=rc, stderr=fx("all-failed.txt"))
    return respond


def writes_profile(rc: int = 0, summary: str | None = None) -> Callable[[list[str]], CommandResult]:
    """`aiperf-local profile ... --artifact-dir DIR`: the captured summary JSON (or an override) plus the captured
    console tail on stdout."""
    def respond(argv: list[str]) -> CommandResult:
        out = artifact_dir(argv)
        out.mkdir(parents=True, exist_ok=True)
        if summary is None:
            shutil.copy(FIXTURES / "profile_export_aiperf.json", out / "profile_export_aiperf.json")
        else:
            (out / "profile_export_aiperf.json").write_text(summary)
        return ok(fx("smoke-console.txt"), rc=rc)
    return respond


def standard_runner() -> FakeRunner:
    r = FakeRunner()
    r.on([LAUNCHER, "--version"], ok(fx("version.txt")))
    r.on([LAUNCHER, "profile"], writes_profile())
    return r


def make_ctx(tmp: Path, runner: object, http: httpx.AsyncClient, *, profiles: str | None = None, key_file: bool = True,
             tokenizer: bool = True, repo: Path | None = None) -> AdapterContext:
    aiperf_dir = tmp / "lab" / "clients" / "aiperf"
    aiperf_dir.mkdir(parents=True, exist_ok=True)
    prof = aiperf_dir / "profiles.yaml"
    prof.write_text(profiles if profiles is not None else fx("profiles.yaml"))
    key = tmp / "home" / ".config" / "aiperf" / "litellm.key"
    if key_file:
        key.parent.mkdir(parents=True, exist_ok=True)
        key.write_text("sk-FAKE-not-a-real-key-0123456789\n")        # EXISTENCE only; never read
        key.chmod(0o600)
    if tokenizer:
        cache = aiperf_dir / ".tiktoken-cache"
        cache.mkdir(exist_ok=True)
        (cache / "fb374d419588a4632f3f557e76b4b70aebbca790").write_bytes(b"x")
    paths: dict[str, Path] = {"aiperf_dir": aiperf_dir, "aiperf_profiles": prof, "aiperf_key_file": key}
    if repo is not None:
        paths["repo"] = repo
    return AdapterContext(runner=runner, http=http, urls={"litellm": "http://127.0.0.1:4000"},  # type: ignore[arg-type]
                          paths=paths, state_dir=tmp / "state")
