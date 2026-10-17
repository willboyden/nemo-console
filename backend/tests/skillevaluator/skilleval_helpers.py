"""Test helpers for the SkillEvaluator adapter: a fake runner that writes real-shaped reports, and a skills tree."""
from __future__ import annotations

import inspect
import json
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path

import httpx

from nemo_console.adapters.base import AdapterContext, CommandResult

FIXTURES = Path(__file__).parent / "fixtures"


def fx(name: str) -> str:
    return (FIXTURES / name).read_text()


Responder = CommandResult | Callable[[list[str]], CommandResult | Awaitable[CommandResult]]


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
                return await out if inspect.isawaitable(out) else out     # an async responder can block (live runs)
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


T3_SKILL, T3_RUN, T3_JOB = "calculator", "20261017_091635_7_46d5ccd742f5", "20261017-051633-16fc9065"


def tier3_stdout(execution_status: str | None = "succeeded", error: str | None = None, rc: int = 0) -> str:
    """The launcher's final stdout with --quiet: one indent=2 JSON object (shape from launch.py main(); the summary is the
    captured summary.json of the same 2026-10-17 run, trimmed; job_dir is a placeholder path)."""
    t3: dict[str, object] = {"execution_status": execution_status, "scored_attempts": 4, "expected_attempts": 4}
    if error:
        t3 = {"execution_status": "error", "error": error}
    return json.dumps({"job": T3_JOB, "job_dir": f"/home/user/.local/share/skilleval-mcp/jobs/cli/{T3_JOB}",
                       "container_exit": rc, "summary": {"skill": T3_SKILL, "tiers": {"3": t3}}}, indent=2)


def writes_tier3(fixture: str | None = "tier3_result.json", rc: int = 0, stderr: str = "") -> Callable[[list[str]], CommandResult]:
    """Like `skillevaluator-local tier3 SKILL ... --results-dir D --quiet`: SkillEvaluator's tree lands in
    D/<skill>/<run>/ (result.json + report.html) and stdout is the final summary JSON."""
    def respond(argv: list[str]) -> CommandResult:
        out = Path(argv[argv.index("--results-dir") + 1])
        if fixture is not None:
            run = out / T3_SKILL / T3_RUN
            run.mkdir(parents=True)
            (run / "result.json").write_text(fx(fixture))
            (run / "report.html").write_text("<!doctype html><title>report</title>\n")
        return ok(tier3_stdout(None if fixture is None else "succeeded"), rc=rc,
                  stderr=stderr or f"launch: job {T3_JOB} ({T3_SKILL}, tier3)")
    return respond


def standard_runner(launcher: str = "skillevaluator-local") -> FakeRunner:
    r = FakeRunner()
    r.on([launcher, "--version"], ok("skillevaluator, version 0.5.0\n"))
    r.on([launcher, "tier1"], writes_report("tier1", "tier1_incomplete.json"))
    r.on([launcher, "tier2"], writes_report("tier2", "tier2_passed.json", rc=0))
    r.on([launcher, "tier3"], writes_tier3())
    r.on([launcher, "doctor"], ok("CLI package pass skillevaluator 0.5.0\nPublic LLM provider pass openai-compatible\n"))
    return r


def build_skills(tmp: Path) -> Path:
    """Idempotent: a second adapter in the same test (a "restarted console") reuses the same tree and state dir."""
    root = tmp / "repo" / ".claude" / "skills"
    if root.is_dir():
        return root
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


def make_ctx(tmp: Path, runner: object, http: httpx.AsyncClient, *, skills: bool = True,
             repo: Path | None = None) -> AdapterContext:
    paths = {"skills_dir": build_skills(tmp)} if skills else {}
    if repo is not None:
        paths["repo"] = repo
    return AdapterContext(runner=runner, http=http, urls={"litellm": "http://127.0.0.1:4000"},  # type: ignore[arg-type]
                          paths=paths, state_dir=tmp / "state")
