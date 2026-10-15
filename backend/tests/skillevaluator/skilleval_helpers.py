"""Test helpers for the SkillEvaluator adapter: a fake runner that writes real-shaped reports, and a skills tree."""
from __future__ import annotations

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
                return res(argv) if callable(res) else res
        return CommandResult(returncode=127, stdout="", stderr=f"no fake for {argv}")


def ok(stdout: str = "", rc: int = 0, stderr: str = "") -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr=stderr)


def writes_report(name: str, fixture: str, rc: int = 1) -> Callable[[list[str]], CommandResult]:
    """Like the real CLI: write the JSON (+ markdown) report into the -o dir, exit 1 when not all checks passed."""
    def respond(argv: list[str]) -> CommandResult:
        out = Path(argv[argv.index("-o") + 1])
        (out / f"skillevaluator-{name}.json").write_text(fx(fixture))
        if name == "tier1":
            (out / "skillevaluator-tier1.md").write_text("# SkillEvaluator Validation Report\n\n**Status:** INCOMPLETE\n")
        return ok("progress lines", rc=rc, stderr="Error: Tier 1 checks failed or were incomplete" if rc else "")
    return respond


def standard_runner(launcher: str = "skillevaluator-local") -> FakeRunner:
    r = FakeRunner()
    r.on([launcher, "--version"], ok("skillevaluator, version 0.5.0\n"))
    r.on([launcher, "tier1"], writes_report("tier1", "tier1_incomplete.json"))
    r.on([launcher, "tier2"], writes_report("tier2", "tier2_passed.json", rc=0))
    r.on([launcher, "doctor"], ok("CLI package pass skillevaluator 0.5.0\nPublic LLM provider pass openai-compatible\n"))
    return r


def build_skills(tmp: Path) -> Path:
    root = tmp / "repo" / ".claude" / "skills"
    for name in ("robust-scripts", "serve-model"):
        (root / name).mkdir(parents=True)
        (root / name / "SKILL.md").write_text(f"---\nname: {name}\ndescription: test\n---\n# {name}\n")
    (root / "no-skill-md").mkdir()                       # not a skill: no SKILL.md
    (root / "README.md").write_text("not a directory")
    outside = tmp / "outside"
    outside.mkdir()
    (outside / "SKILL.md").write_text("---\nname: outside\n---\n")
    (root / "linked").symlink_to(outside)                # symlinks are never offered
    return root


def make_ctx(tmp: Path, runner: object, http: httpx.AsyncClient, *, skills: bool = True) -> AdapterContext:
    paths = {"skills_dir": build_skills(tmp)} if skills else {}
    return AdapterContext(runner=runner, http=http, urls={"litellm": "http://127.0.0.1:4000"},  # type: ignore[arg-type]
                          paths=paths, state_dir=tmp / "state")
