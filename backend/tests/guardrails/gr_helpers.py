"""Helpers for the guardrails adapter tests: a prefix-matching fake runner and a config tree copied from the REAL lab config."""
from __future__ import annotations

import inspect
import shutil
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path

import httpx

from nemo_console.adapters.base import AdapterContext, CommandResult

FIXTURES = Path(__file__).parent / "fixtures"
BASE = "http://127.0.0.1:8806"
CONTAINER = "ai-lab-nemo-guardrails"


def fx(name: str) -> str:
    return (FIXTURES / name).read_text()


Responder = CommandResult | Callable[[list[str]], CommandResult | Awaitable[CommandResult]]


def ok(stdout: str = "", rc: int = 0, stderr: str = "") -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr=stderr)


@dataclass
class FakeRunner:
    """Matches argv by prefix (longest first). Unmatched argv -> rc 127 so a missing stub is loud, not silent."""

    rules: list[tuple[list[str], Responder]] = field(default_factory=list)
    calls: list[list[str]] = field(default_factory=list)
    cwds: list[Path | None] = field(default_factory=list)

    def on(self, prefix: list[str], result: Responder) -> FakeRunner:
        self.rules = [r for r in self.rules if r[0] != prefix] + [(prefix, result)]
        return self

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append(list(argv))
        self.cwds.append(cwd)
        for prefix, res in sorted(self.rules, key=lambda r: -len(r[0])):
            if argv[: len(prefix)] == prefix:
                out = res(argv) if callable(res) else res
                return await out if inspect.isawaitable(out) else out
        return CommandResult(returncode=127, stdout="", stderr=f"no fake for {argv}")

    def called(self, prefix: list[str]) -> list[list[str]]:
        return [c for c in self.calls if c[: len(prefix)] == prefix]


def standard_runner(isolation: str | None = None, internal: str | None = None) -> FakeRunner:
    """The live deployment as measured: all three containers running, the server healthy on an internal network."""
    iso = isolation if isolation is not None else fx("inspect_isolation.txt")

    def inspect_(argv: list[str]) -> CommandResult:
        if argv[3].startswith("{{range"):                      # the network/port isolation probe
            return ok(iso)
        return ok(fx("inspect_server.txt") if argv[-1] == CONTAINER else fx("inspect_gw.txt"))

    r = FakeRunner()
    r.on(["docker", "inspect", "--format"], inspect_)
    r.on(["docker", "network", "inspect"], ok(internal if internal is not None else fx("network_internal.txt")))
    r.on(["guardrails-local", "--version"], ok(fx("version.txt")))
    r.on(["guardrails-local", "validate"], ok(fx("validate_ok.stdout")))
    return r


def build_tree(tmp: Path) -> Path:
    """security/nemo-guardrails/config with the real lab-input-output config plus the awkward files."""
    root = tmp / "repo" / "security" / "nemo-guardrails"
    cfg = root / "config"
    if cfg.is_dir():
        return root
    shutil.copytree(FIXTURES / "config-tree", cfg)
    (root / "docker-compose.yml").write_text(
        'services:\n  guardrails:\n    command: ["server", "--config", "/config", "--default-config-id", '
        '"lab-input-output", "--port", "8000"]\n')
    c = cfg / "lab-input-output"
    (c / "actions.py").write_text("def hello():\n    return 1\n")
    (c / "Notes.md").write_text("upper-case names are not valid ids\n")
    (c / "kb").mkdir()
    (c / "kb" / "faq.md").write_text("# faq\n")
    outside = tmp / "outside"
    outside.mkdir()
    (outside / "secret.yml").write_text("x: 1\n")
    (c / "linked.yml").symlink_to(outside / "secret.yml")
    (cfg / "no-config").mkdir()
    (cfg / "no-config" / "README.md").write_text("no config.yml here\n")
    (cfg / "_hidden").mkdir()
    (cfg / "_hidden" / "config.yml").write_text("models: []\n")
    return root


def make_ctx(tmp: Path, runner: object, http: httpx.AsyncClient) -> AdapterContext:
    root = build_tree(tmp)
    return AdapterContext(
        runner=runner, http=http,  # type: ignore[arg-type]
        urls={"guardrails": BASE},
        paths={"guardrails_dir": root, "guardrails_compose": root / "docker-compose.yml",
               "guardrails_config_dir": root / "config"},
        state_dir=tmp / "state")
