"""SkillEvaluator adapter: evaluate agent skills with NVIDIA SkillEvaluator and keep the reports.

Facts this adapter is built on (source in brackets):
  * `skillevaluator` 0.5.0 is a CLI, not a service [`skillevaluator --help`]. It is configured by environment only
    (SKILL_EVAL_*); it reads no dotenv or config file [source: provider_config.py].
  * `tier1 PATH [--no-llm] -r json,markdown -o DIR` writes skillevaluator-tier1.{json,md}; `tier2 PATH -r json -o DIR`
    writes skillevaluator-tier2.json [live 2026-10-06]. The exit code is 1 when checks fail OR are incomplete and the
    reports are still written, so the verdict is the JSON `overall_status` (passed | failed | incomplete), not the
    exit code [live].
  * tier1 JSON: overall_status, incomplete_scans, total_errors, total_warnings, severity_counts, quality_summary[]
    (overall_score, grade) and, when an LLM is configured, rubric_eval.overall_score [live]. Tier 2 needs an embeddings
    endpoint; Tier 1 without --no-llm needs a chat model [source]. A Tier 2 run whose embeddings call fails still writes
    its report, with `embedding-provider` in incomplete_scans [source: intra_skill_validator.py].
  * Tier 3 (live agent runs, "Skill Lift") runs ONLY through the lab wrapper `skillevaluator-local tier3 SKILL ...`,
    which hands it to clients/skillevaluator/runner/launch.py: one sandboxed runner container per job that reaches the
    router (through a key-injecting broker) and the public internet only [lab source, 2026-10-17]. SkillEvaluator's own
    stock sandboxes run with network_mode "public" [source: tier3/harbor/adapter.py], so plain `skillevaluator` is
    refused for Tier 3. `--results-dir DIR` receives SkillEvaluator's tree as <skill>/<run>/result.json; `--quiet`
    leaves stdout to the final {"job", "container_exit", "summary"} object [launch.py --help, 2026-10-17]. A run takes
    ~30 s for a 2-case skill and up to about an hour, so it runs in the background (one at a time), never inside the
    HTTP request.
  * result.json: execution_status (succeeded | failed | unknown | skipped | not_applicable), scored_attempts,
    expected_attempts, duration_seconds, run_id, agents.<agent>.{model, lift.overall.{with_skill, without_skill, delta},
    lift.<metric>.{...}, execution_status, execution_errors}, tier3_feedback.{conclusions, recommendations}[] of
    {severity, title, message} [captured 2026-10-17, tests/skillevaluator/fixtures/tier3_result.json].
  * Launcher: `skillevaluator-local` when it is on PATH (a wrapper that injects the provider settings and reads its
    own 0600 key file), else `skillevaluator`, which then needs SKILL_EVAL_* in the console's environment (the runner
    passes those to SkillEvaluator binaries only). The console never reads a key file and never returns a key.
"""
from __future__ import annotations

import asyncio
import contextlib
import json
import os
import re
import shutil
import stat
import time
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from nemo_console.adapters.base import AdapterContext, CommandResult, SystemAdapter
from nemo_console.domain.models import (
    ActionResult,
    ActionSpec,
    ApplyResult,
    Component,
    ConfigFile,
    ConfigFileSpec,
    Endpoint,
    MetricPoint,
    MetricsSnapshot,
    ParamSpec,
    Problem,
    SystemEvent,
    SystemStatus,
    ValidationResult,
)
from nemo_console.services.common import redact

LAUNCHERS = ("skillevaluator-local", "skillevaluator")     # first one found on PATH wins
DEFAULT_LITELLM_URL = "http://127.0.0.1:4000"
RUN_TIMEOUT = 540.0                 # one tier; the runner caps every command at 600 s
VERSION_TTL = 300.0                 # `--version` is a subprocess: do not spawn it on every status poll
KEEP_RUNS = 200                     # oldest run directories beyond this are pruned after each run
EVENT_KIND = "skilleval.run"
NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
RUN_ID_RE = re.compile(r"\d{8}T\d{6}-\d{3}-[A-Za-z0-9._-]{1,128}")
TIERS = {
    "static": "Tier 1 static checks only (offline, no model calls)",
    "tier1": "Tier 1 with the LLM judge (rubric + LLM security review)",
    "tier1+2": "Tier 1 with the LLM judge, then Tier 2 deduplication (needs an embeddings model)",
}
# The pinned install (the lab's installer pins the same commit and links the wrapper); shown, never run.
PINNED_SHA = "7304d76cde371287b67ea99653409d014b6b9c85"          # tag v0.5.0
PINNED_INSTALL = (f'uv tool install --python 3.13 "skillevaluator[all] @ '
                  f'git+https://github.com/NVIDIA/SkillEvaluator.git@{PINNED_SHA}"   # v0.5.0')
LAB_INSTALLER = Path("clients") / "skillevaluator" / "install.sh"
LAB_OLLAMA = Path("inference") / "ollama"

# ── Tier 3 (live agent evaluation) ───────────────────────────────────────────────────────────
LIVE_LAUNCHER = "skillevaluator-local"     # Tier 3 needs the lab wrapper: it contains the run (see the module doc)
LIVE_AGENT = "opencode"                    # the only agent CLI in the lab's runner image
LIVE_TIMEOUT = 4800.0                      # = runner.MAX_LIVE_EVAL_TIMEOUT; launch.py itself stops a job by 4620 s
LIVE_MAX_BYTES = 4_000_000                 # stdout is the final summary JSON (launch.py caps summary.json at 2 MiB)
LIVE_MAX_ATTEMPTS = 5
LIVE_DATASETS = ("auto", "existing", "generate")
DEFAULT_AGENT_MODEL = "nemotron-lightning"           # launch.py's verified defaults (2026-10-17)
DEFAULT_JUDGE_MODEL = "nemotron-lightning-judge"
MODEL_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:/@\-]{0,127}")      # the playground's model-id shape (api/routers/act.py)
WORD_RE = re.compile(r"[a-z][a-z0-9_]{0,31}")                      # status words / metric names copied from result.json
RESULT_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
MAX_RESULT_BYTES = 8 * 1024 * 1024
MAX_FEEDBACK = 8                           # conclusions / recommendations kept per run
TIER3_STATUSES = ("succeeded", "failed", "unknown", "skipped", "not_applicable")
VERDICTS = ("passed", "failed", "incomplete", "error", "succeeded", "interrupted")   # se.runs_by_verdict labels

Done = Callable[..., ActionResult]          # run_action's `done(ok, output, problems=None)`


def _num(v: object) -> float | None:
    return float(v) if isinstance(v, int | float) and not isinstance(v, bool) else None


def _dict(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


def _list(v: object) -> list[Any]:
    return v if isinstance(v, list) else []


def _word(v: object) -> str | None:
    return v if isinstance(v, str) and WORD_RE.fullmatch(v) else None


def _model_id(v: object) -> str | None:
    return v if isinstance(v, str) and MODEL_RE.fullmatch(v) else None


def summarize_tier1(d: dict[str, Any]) -> dict[str, Any]:
    """The few fields the UI shows, from a tier1 JSON report. Missing fields stay None (never invented)."""
    q = _list(d.get("quality_summary"))
    q0 = _dict(q[0]) if q else {}
    rub = _dict(d.get("rubric_eval"))
    sev = _dict(d.get("severity_counts"))
    inc = d.get("incomplete_scans")
    return {
        "status": d.get("overall_status") if isinstance(d.get("overall_status"), str) else None,
        "quality_score": _num(q0.get("overall_score")),
        "grade": q0.get("grade") if isinstance(q0.get("grade"), str) else None,
        "rubric_score": _num(rub.get("overall_score")) if rub.get("execution_status") == "succeeded" else None,
        "errors": _num(d.get("total_errors")),
        "warnings": _num(d.get("total_warnings")),
        "high": _num(sev.get("high")),
        "critical": _num(sev.get("critical")),
        "incomplete": [str(x) for x in inc][:10] if isinstance(inc, list) else [],
    }


def failed_checks(d: dict[str, Any], limit: int = 12) -> list[str]:
    """`[SEVERITY] validator: message` for failed findings, most severe first."""
    order = {"critical": 0, "high": 1, "medium": 2, "low": 3, "info": 4}
    rows: list[tuple[int, str]] = []
    for r in _list(d.get("results")):
        if not isinstance(r, dict):
            continue
        for f in _list(r.get("findings")):
            if not isinstance(f, dict):
                continue
            sev = str(f.get("severity", "info")).lower()
            if sev in ("info", "low"):
                continue
            msg = str(f.get("message") or f.get("title") or f.get("check") or "")[:200]
            rows.append((order.get(sev, 5), f"[{sev.upper()}] {r.get('validator', '?')}: {msg}"))
    rows.sort(key=lambda x: x[0])
    return [line for _, line in rows[:limit]]


def _feedback(items: object) -> list[dict[str, str]]:
    """tier3_feedback conclusions / recommendations -> at most MAX_FEEDBACK {severity, title, message}, redacted, capped."""
    out: list[dict[str, str]] = []
    for it in _list(items)[:MAX_FEEDBACK]:
        if isinstance(it, str):
            it = {"message": it}
        if not isinstance(it, dict):
            continue
        msg = redact(str(it.get("message") or ""))[:400]
        title = redact(str(it.get("title") or ""))[:160]
        if msg or title:
            out.append({"severity": _word(it.get("severity")) or "info", "title": title, "message": msg})
    return out


def summarize_tier3(d: dict[str, Any]) -> dict[str, Any]:
    """The fields the console keeps from a Tier 3 result.json. Missing fields stay None (never invented); strings are
    copied only when identifier-shaped, free text (feedback, errors) is redacted and capped."""
    agents: dict[str, Any] = {}
    for name, raw in list(_dict(d.get("agents")).items())[:8]:
        if not isinstance(name, str) or not NAME_RE.fullmatch(name):
            continue
        a = _dict(raw)
        lift = _dict(a.get("lift"))
        overall = _dict(lift.get("overall"))
        metrics: dict[str, dict[str, float | None]] = {}
        for key, v in list(lift.items())[:16]:
            if key == "overall" or not _word(key):
                continue
            m = _dict(v)
            metrics[key] = {"with_skill": _num(m.get("with_skill")), "without_skill": _num(m.get("without_skill")),
                            "delta": _num(m.get("delta"))}
        agents[name] = {
            "model": _model_id(a.get("model")),
            "execution_status": _word(a.get("execution_status")),
            "skill_lift": _num(overall.get("delta")),
            "with_skill": _num(overall.get("with_skill")),
            "without_skill": _num(overall.get("without_skill")),
            "scored_attempts": _num(a.get("scored_attempts")),
            "expected_attempts": _num(a.get("expected_attempts")),
            "metrics": metrics,
            "errors": [redact(str(e))[:300] for e in _list(a.get("execution_errors"))[:5]],
        }
    fb = _dict(d.get("tier3_feedback"))
    return {
        "execution_status": _word(d.get("execution_status")),
        "report_status": _word(d.get("report_status")),
        "scored_attempts": _num(d.get("scored_attempts")),
        "expected_attempts": _num(d.get("expected_attempts")),
        "duration_seconds": _num(d.get("duration_seconds")),
        "run_id": d.get("run_id") if isinstance(d.get("run_id"), str) and RESULT_NAME_RE.fullmatch(d["run_id"]) else None,
        "agents": agents,
        "conclusions": _feedback(fb.get("conclusions")),
        "recommendations": _feedback(fb.get("recommendations")),
        "errors": [redact(str(e))[:300] for e in _list(d.get("execution_errors"))[:5]],
    }


def tier3_verdict(t3: dict[str, Any]) -> str:
    status = t3.get("execution_status")
    return status if status in TIER3_STATUSES else "error"


def last_json_object(text: str) -> dict[str, Any] | None:
    """The launcher's final JSON object. It is printed with indent=2, so it starts on the last line that begins with
    `{` (nested objects are indented)."""
    idx = text.rfind("\n{")
    start = idx + 1 if idx >= 0 else (0 if text.startswith("{") else -1)
    if start < 0:
        return None
    try:
        d = json.loads(text[start:])
    except ValueError:
        return None
    return d if isinstance(d, dict) else None


def _fmt(v: object, signed: bool = False) -> str:
    n = _num(v)
    if n is None:
        return "-"
    return f"{n:+.3f}" if signed else f"{n:.3f}"


@dataclass
class _LiveRun:
    """The ONE live (Tier 3) run this console owns. The task holds the only reference that keeps it alive."""
    run_id: str
    skill: str
    started: float
    task: asyncio.Task[None]


class SkillEvaluatorAdapter(SystemAdapter):
    id = "skillevaluator"
    label = "SkillEvaluator"

    # ctx.paths: skills_dir (the skill roots' parent: one sub-directory per skill, each with SKILL.md), state, repo
    # (only to word install / embeddings fixes). ctx.urls: "litellm" (the router the launcher points SkillEvaluator
    # at; only its liveness is probed).
    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self._version_cache: tuple[float, str | None, str | None] = (0.0, None, None)   # (ts, launcher, version)
        self.follow_interval = 2.0
        self._live: _LiveRun | None = None

    # ── helpers ─────────────────────────────────────────────────────────────────────────────
    @property
    def _litellm(self) -> str:
        return self.ctx.urls.get("litellm", DEFAULT_LITELLM_URL).rstrip("/")

    @property
    def _skills_root(self) -> Path | None:
        return self.ctx.paths.get("skills_dir")

    @property
    def _runs_dir(self) -> Path:
        return self.ctx.state_dir / "skillevaluator" / "runs"

    def _lab_file(self, rel: Path) -> Path | None:
        """A lab helper: `rel`'s file under the `skillevaluator_dir` layout dir (addendum 9.11), else `rel` under the repo root."""
        base, repo = self.ctx.paths.get("skillevaluator_dir"), self.ctx.paths.get("repo")
        for p in (base / rel.name if base is not None else None, repo / rel if repo is not None else None):
            if p is not None and p.exists():
                return p
        return None

    def _install_fix(self) -> str:
        """The lab's pinned installer when the repo has it (it also links the wrapper), else the pinned uv command."""
        lab = self._lab_file(LAB_INSTALLER)
        return f"{lab}   # pinned SkillEvaluator {PINNED_SHA[:7]} (v0.5.0) + the skillevaluator-local wrapper" if lab \
            else PINNED_INSTALL

    def _wrapper_fix(self) -> str:
        lab = self._lab_file(LAB_INSTALLER)
        return (f"install the lab wrapper: {lab}   # links skillevaluator-local into ~/.local/bin" if lab else
                "install the lab wrapper: clients/skillevaluator/install.sh in the lab repository (it links "
                "skillevaluator-local onto PATH and builds the runner)")

    def _embeddings_fix(self) -> str:
        repo = self.ctx.paths.get("repo")
        ollama = next((p for p in (self.ctx.paths.get("ollama_dir"), repo / LAB_OLLAMA if repo is not None else None)
                       if p is not None and p.exists()), None)
        return (f"cd {ollama} && docker compose up -d" if ollama
                else "start the embeddings model the launcher names in SKILL_EVAL_EMBEDDING_MODEL")

    def skills(self) -> list[str]:
        """Skill names under skills_dir: real directories (no symlinks) that contain a SKILL.md."""
        root = self._skills_root
        if root is None:
            return []
        try:
            entries = sorted(root.iterdir())
        except OSError:
            return []
        out: list[str] = []
        for p in entries:
            if NAME_RE.fullmatch(p.name) and not p.is_symlink() and p.is_dir() and (p / "SKILL.md").is_file():
                out.append(p.name)
        return out[:500]

    def _skill_path(self, name: str) -> Path:
        """Resolve a skill name to its directory, refusing anything outside skills_dir."""
        root = self._skills_root
        if root is None or not NAME_RE.fullmatch(name):
            raise KeyError(name)
        base = root.resolve()
        p = (root / name).resolve()
        if p.parent != base or not (p / "SKILL.md").is_file():
            raise KeyError(name)
        return p

    async def _launcher(self) -> tuple[str | None, str | None]:
        ts, launcher, version = self._version_cache
        if time.monotonic() - ts < VERSION_TTL and launcher is not None:
            return launcher, version
        launcher, version = None, None
        for name in LAUNCHERS:
            r = await self._run([name, "--version"], timeout=20.0, max_bytes=4096)
            if r.returncode == 0:
                m = re.search(r"version\s+(\S+)", r.stdout)
                launcher, version = name, (m.group(1) if m else r.stdout.strip()[:40] or None)
                break
        self._version_cache = (time.monotonic(), launcher, version)
        return launcher, version

    async def _run(self, argv: list[str], *, timeout: float, max_bytes: int = 256_000,
                   cwd: Path | None = None) -> CommandResult:
        try:
            return await self.ctx.runner.run(argv, timeout=timeout, max_bytes=max_bytes, cwd=cwd)
        except Exception as e:  # noqa: BLE001 - not allowlisted / not on PATH: data, not an exception
            return CommandResult(returncode=127, stdout="", stderr=f"{type(e).__name__}: {e}"[:300])

    async def _router_up(self) -> tuple[bool, str]:
        try:
            resp = await self.ctx.http.get(f"{self._litellm}/health/liveliness", timeout=3.0, follow_redirects=False)
        except Exception as e:  # noqa: BLE001
            return False, type(e).__name__
        return resp.status_code < 500, f"HTTP {resp.status_code}"

    def _live_run_id(self) -> str | None:
        """The run id of the live run this process is still running (None when idle or after a restart)."""
        live = self._live
        return live.run_id if live is not None and not live.task.done() else None

    # ── stored runs ─────────────────────────────────────────────────────────────────────────
    def _load_meta(self, run_dir: Path) -> dict[str, Any] | None:
        """run.json, with a Tier 3 run left at `running` by a process that no longer runs it shown as interrupted."""
        try:
            meta = json.loads((run_dir / "run.json").read_text())
        except (OSError, ValueError):
            return None
        if not isinstance(meta, dict):
            return None
        if meta.get("verdict") == "running" and meta.get("run_id") != self._live_run_id():
            meta = {**meta, "verdict": "interrupted", "interrupted_by": "restart"}
        return meta

    def _event(self, meta: dict[str, Any]) -> SystemEvent:
        parts = [str(meta.get("skill", "?")), str(meta.get("tiers", "?"))]
        if meta.get("tiers") == "tier3":
            live = _dict(meta.get("live"))
            t3 = _dict(meta.get("tier3"))
            for name, a in _dict(t3.get("agents")).items():
                if _num(_dict(a).get("skill_lift")) is not None:
                    parts.append(f"lift {_fmt(_dict(a).get('skill_lift'), signed=True)} ({name})")
                    break
            if meta.get("verdict") == "running":
                parts.append(f"{LIVE_AGENT}={live.get('agent_model', '?')}, {live.get('attempts', '?')} attempt(s)")
            parts.append(str(meta.get("verdict", "?")))
            sa, ea = _num(t3.get("scored_attempts")), _num(t3.get("expected_attempts"))
            if sa is not None and ea is not None:
                parts[-1] += f" ({sa:.0f}/{ea:.0f} attempts scored)"
        else:
            s = _dict(meta.get("tier1"))
            if s.get("quality_score") is not None:
                parts.append(f"quality {s['quality_score']:.1f}{' ' + s['grade'] if s.get('grade') else ''}")
            if s.get("rubric_score") is not None:
                parts.append(f"judge {s['rubric_score']:.1f}")
            parts.append(str(meta.get("verdict", "?")))
            if s.get("incomplete"):
                parts[-1] += f" ({', '.join(s['incomplete'])} incomplete)"
        ts = _num(meta.get("ts")) or 0.0
        data: dict[str, Any] = {k: meta.get(k) for k in ("run_id", "skill", "tiers", "verdict", "model_calls", "tier1",
                                                         "tier2_status", "duration_ms")}
        if meta.get("tiers") == "tier3":
            # metadata only: the agents' lift numbers and the run settings, never the feedback text
            t3 = _dict(meta.get("tier3"))
            summary: dict[str, Any] = {k: t3.get(k) for k in ("execution_status", "scored_attempts", "expected_attempts",
                                                               "duration_seconds")}
            summary["agents"] = {n: {k: _dict(a).get(k) for k in ("model", "skill_lift", "with_skill", "without_skill",
                                                                  "execution_status")}
                                 for n, a in _dict(t3.get("agents")).items()}
            data["tier3"] = summary
            data["live"] = _dict(meta.get("live"))
        return SystemEvent(ts=ts, system="skillevaluator", kind=EVENT_KIND, summary=" · ".join(parts)[:300], data=data)

    def _read_run(self, run_dir: Path) -> SystemEvent | None:
        meta = self._load_meta(run_dir)
        return None if meta is None else self._event(meta)

    def _run_dirs(self) -> list[Path]:
        try:
            return sorted((p for p in self._runs_dir.iterdir() if RUN_ID_RE.fullmatch(p.name) and p.is_dir()),
                          key=lambda p: p.name, reverse=True)
        except OSError:
            return []

    def _list_runs(self, limit: int) -> list[SystemEvent]:
        out: list[SystemEvent] = []
        for d in self._run_dirs()[: max(limit, 0)]:
            ev = self._read_run(d)
            if ev is not None:
                out.append(ev)
        return out

    def _prune(self) -> None:
        live = self._live_run_id()
        for d in self._run_dirs()[KEEP_RUNS:]:
            if d.name != live:                      # never delete the directory a live run is writing into
                shutil.rmtree(d, ignore_errors=True)

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    async def status(self) -> SystemStatus:
        (launcher, version), (router_ok, router_detail) = await asyncio.gather(self._launcher(), self._router_up())
        skills = await asyncio.to_thread(self.skills)
        last = await asyncio.to_thread(self._list_runs, 1)
        problems: list[Problem] = []
        components = [
            Component(kind="process", name=launcher or "skillevaluator", state="running" if launcher else "absent",
                      detail=(f"{launcher} {version or ''}".strip() + (
                          " (wrapper: provider settings and its own key file)" if launcher == "skillevaluator-local"
                          else " (provider from SKILL_EVAL_* in the console's environment)"))
                      if launcher else "neither skillevaluator-local nor skillevaluator is on PATH"),
            Component(kind="http", name="router " + self._litellm, state="running" if router_ok else "stopped",
                      detail=f"{router_detail} on GET /health/liveliness (LLM judge and embeddings go through it)"),
        ]
        live = self._live if self._live_run_id() else None
        if live is not None:
            mins = max(0.0, (time.time() - live.started) / 60)
            components.append(Component(kind="process", name=f"tier3 {live.skill}", state="running",
                                        detail=f"live evaluation {live.run_id}, running for {mins:.0f} min "
                                               "(one at a time; it shows in the runs list)"))
        if launcher is None:
            problems.append(Problem(
                severity="error", code="skillevaluator.not_installed",
                message="SkillEvaluator is not installed (or not on the console's PATH).", fix=self._install_fix()))
        elif launcher == "skillevaluator":
            problems.append(Problem(
                severity="info", code="skillevaluator.no_wrapper",
                message="Running plain `skillevaluator`: LLM checks need SKILL_EVAL_* (provider, base URL, key) in the "
                        "console's environment; without them Tier 1 runs static checks only and Tier 2 fails. Tier 3 "
                        "(live agent runs) is refused: it needs the skillevaluator-local wrapper's sandboxed runner.",
                fix="Put a `skillevaluator-local` wrapper on PATH, or set SKILL_EVAL_* in the console's EnvironmentFile"))
        if not router_ok:
            problems.append(Problem(
                severity="warn", code="skillevaluator.router_down",
                message=f"The router ({self._litellm}) is not answering; only the 'static' tier will work.",
                fix=f"curl -s -o /dev/null -w '%{{http_code}}\\n' {self._litellm}/health/liveliness"))
        if self._skills_root is None or not skills:
            problems.append(Problem(
                severity="warn", code="skillevaluator.no_skills",
                message=f"No skills found under {self._skills_root} (one directory per skill, each with a SKILL.md).",
                fix="export NEMO_CONSOLE_REPO_ROOT=/path/to/repo-with-.claude/skills and restart nemo-console"))
        health = "down" if launcher is None else ("degraded" if not router_ok else "up")
        summary = f"{len(skills)} skills"
        if live is not None:
            summary += f"; live: {live.skill} running"
        if last:
            summary += f"; last run: {last[0].summary}"
        return SystemStatus(
            system="skillevaluator", health=health, version=version, summary=summary[:300],
            endpoints=[Endpoint(name="router (judge + embeddings)", url=self._litellm, role="api", auth="bearer")],
            components=components, problems=problems, checked_at=time.time())

    async def metrics(self) -> MetricsSnapshot:
        now = time.time()
        skills = await asyncio.to_thread(self.skills)
        runs = await asyncio.to_thread(self._list_runs, KEEP_RUNS)
        points = [MetricPoint(key="se.skills", value=float(len(skills)), unit="1"),
                  MetricPoint(key="se.runs", value=float(len(runs)), unit="1")]
        if runs:                                     # a live run always has a run directory, so this covers it
            points.append(MetricPoint(key="se.live_running", value=1.0 if self._live_run_id() else 0.0, unit="1"))
            static = next((r for r in runs if r.data.get("tiers") != "tier3"), None)
            t1 = (static.data.get("tier1") if static else None) or {}
            if isinstance(t1, dict):
                for key, field in (("se.last_quality_score", "quality_score"), ("se.last_judge_score", "rubric_score")):
                    v = _num(t1.get(field))
                    if v is not None:
                        points.append(MetricPoint(key=key, value=v, unit="1"))
            for r in runs:                           # the newest finished Tier 3 run that reported a lift
                lift = next((_num(_dict(a).get("skill_lift")) for a in _dict(_dict(r.data.get("tier3")).get("agents"))
                             .values() if _num(_dict(a).get("skill_lift")) is not None), None)
                if lift is not None:
                    points.append(MetricPoint(key="se.last_skill_lift", value=lift, unit="1"))
                    break
            verdicts = [r.data.get("verdict") for r in runs]
            for verdict in VERDICTS:
                points.append(MetricPoint(key=f"se.runs_by_verdict{{verdict={verdict}}}",
                                          value=float(verdicts.count(verdict)), unit="1"))
        return MetricsSnapshot(system="skillevaluator", ts=now, available=True, points=points)

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        return await asyncio.to_thread(self._list_runs, min(limit, KEEP_RUNS))

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        # keyed by (run, verdict): a live run is announced when it starts AND again when it finishes or is interrupted
        seen: set[tuple[str, str]] = set()
        first = True
        while True:
            events = await asyncio.to_thread(self._list_runs, 50)
            for ev in sorted(events, key=lambda e: e.ts):
                key = (str(ev.data.get("run_id")), str(ev.data.get("verdict")))
                if key in seen:
                    continue
                seen.add(key)
                if first and (since_ts is None or ev.ts <= since_ts):
                    continue
                yield ev
            first = False
            await asyncio.sleep(self.follow_interval)

    # ── configure: nothing to edit (configuration is the launcher's environment) ────────────
    def config_files(self) -> list[ConfigFileSpec]:
        return []

    def config_path(self, file_id: str) -> Path:
        raise KeyError(file_id)

    async def read_config(self, file_id: str) -> ConfigFile:
        raise KeyError(file_id)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        raise KeyError(file_id)

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        raise KeyError(file_id)

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    def actions(self) -> list[ActionSpec]:
        names = self.skills()

        def skill_param() -> ParamSpec:
            return ParamSpec(key="skill", label="Skill", type="enum", choices=names or None,
                             default=names[0] if names else None, requires_restart=False,
                             help="A directory under the skills dir that contains SKILL.md.")

        return [
            ActionSpec(
                id="evaluate", label="Evaluate a skill", danger="safe",
                description="Runs SkillEvaluator on one skill and keeps the JSON + Markdown reports in the console's "
                            "state dir (newest 200 runs). 'static' makes no model calls; 'tier1' adds the LLM judge "
                            "through the router; 'tier1+2' also runs embedding deduplication. Tier 3 (live agent "
                            "runs) is the separate 'Tier 3: live agent evaluation' action.",
                params=[
                    skill_param(),
                    ParamSpec(key="tiers", label="Tiers", type="enum", choices=list(TIERS), default="tier1",
                              requires_restart=False, help="; ".join(f"{k}: {v}" for k, v in TIERS.items())),
                ]),
            ActionSpec(
                id="evaluate_live", label="Tier 3: live agent evaluation", danger="disruptive",
                description="Runs an agent (opencode) on the skill's eval cases with and without the skill, in a "
                            "sandboxed runner container started by the skillevaluator-local wrapper (it reaches the "
                            "router and the public internet only); a judge model scores both arms and the difference "
                            "is the Skill Lift. Spends router tokens and takes minutes (up to about an hour with "
                            "dataset generation and several attempts). It starts in the background: the run shows as "
                            "running in the runs list and fills in when it ends; open it with 'Show a stored report'. "
                            "One live run at a time.",
                params=[
                    skill_param(),
                    ParamSpec(key="attempts", flag="--n-attempts", label="Attempts per case", type="int", default=1,
                              min=1, max=LIVE_MAX_ATTEMPTS, requires_restart=False,
                              help="Runs per eval case and arm. The lift varies between runs; use 3 or more before "
                                   "trusting it."),
                    ParamSpec(key="agent_model", flag="--agent-model", label="Agent model (router alias)",
                              type="string", default=DEFAULT_AGENT_MODEL, requires_restart=False,
                              help=f"The model opencode runs on (passed as {LIVE_AGENT}=ALIAS). It must handle tool "
                                   "results on the Responses API."),
                    ParamSpec(key="judge_model", flag="--judge-model", label="Judge model (router alias)",
                              type="string", default=DEFAULT_JUDGE_MODEL, requires_restart=False,
                              help="Scores both arms; a model with thinking off so its JSON parses."),
                    ParamSpec(key="dataset", flag="--dataset", label="Eval dataset", type="enum",
                              choices=list(LIVE_DATASETS), default="auto", requires_restart=False,
                              help="auto: the skill's evals/evals.json, else one starter case; existing: fail without "
                                   "evals.json; generate: write up to four cases with the chat model first (slower)."),
                    ParamSpec(key="skip_baseline", flag="--skip-baseline", label="Skip the without-skill arm",
                              type="bool", default=False, requires_restart=False,
                              help="Half the agent runs, but no Skill Lift (nothing to compare against)."),
                ]),
            ActionSpec(
                id="evaluate_all_static", label="Static check of every skill", danger="safe",
                description="Tier 1 static checks (no model calls) on every skill, one run each; prints a score table. "
                            "Takes about 2 s per skill."),
            ActionSpec(
                id="report", label="Show a stored report", danger="safe",
                description="The report of a stored run (newest first): Tier 1/2 findings, or a Tier 3 run's status, "
                            "Skill Lift per agent and its conclusions.",
                params=[ParamSpec(key="run_id", label="Run", type="string", requires_restart=False,
                                  help="A run id from the runs list; empty = the newest run.")]),
            ActionSpec(id="doctor", label="Doctor", danger="safe",
                       description="skillevaluator doctor: which provider and Harbor agents the launcher resolves."),
        ]

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        t0 = time.monotonic()

        def done(ok: bool, out: str, problems: list[Problem] | None = None) -> ActionResult:
            return ActionResult(ok=ok, output=redact(out)[:20_000], duration_ms=int((time.monotonic() - t0) * 1000),
                                problems=problems or [])

        if action_id not in {"evaluate", "evaluate_live", "evaluate_all_static", "report", "doctor"}:
            raise KeyError(action_id)
        if action_id == "report":
            return done(*await asyncio.to_thread(self._report_text, str(params.get("run_id") or "")))
        if action_id == "evaluate_live":
            return await self._start_live(params, done)
        launcher, _ = await self._launcher()
        if launcher is None:
            return done(False, "SkillEvaluator is not installed.", [Problem(
                severity="error", code="skillevaluator.not_installed", message="SkillEvaluator is not on PATH.",
                fix=self._install_fix())])
        if action_id == "doctor":
            r = await self._run([launcher, "doctor"], timeout=60.0)
            return done(r.returncode == 0, (r.stdout + ("\n" + r.stderr if r.stderr else "")).strip())
        if action_id == "evaluate":
            skill, tiers = str(params.get("skill") or ""), str(params.get("tiers") or "tier1")
            if tiers not in TIERS:
                return done(False, f"unknown tiers {tiers!r}")
            try:
                path = self._skill_path(skill)
            except KeyError:
                return done(False, f"unknown skill {skill!r}")
            meta = await self._evaluate(launcher, skill, path, tiers)
            text = await asyncio.to_thread(self._report_text, str(meta.get("run_id")))
            problems = [self._no_embeddings_problem()] if meta.get("tier2_no_embeddings") else []
            return done(meta.get("verdict") in ("passed", "incomplete", "failed"), text[1], problems)
        # evaluate_all_static
        rows = ["skill                                     verdict     quality  grade  errors  high"]
        ok = True
        for name in self.skills():
            try:
                meta = await self._evaluate(launcher, name, self._skill_path(name), "static")
            except KeyError:
                continue
            s = _dict(meta.get("tier1"))
            q = s.get("quality_score")
            ok = ok and meta.get("verdict") != "error"
            rows.append(f"{name[:41]:<41} {meta.get('verdict')!s:<11} {q if q is not None else '-':>7}  "
                        f"{s.get('grade') or '-':<5}  {s.get('errors') if s.get('errors') is not None else '-':>6}  "
                        f"{s.get('high') if s.get('high') is not None else '-':>4}")
        return done(ok, "\n".join(rows))

    def _no_embeddings_problem(self) -> Problem:
        return Problem(severity="warn", code="skillevaluator.tier2.no_embeddings",
                       message="Tier 2 could not get embeddings: the launcher's embeddings model (nomic-embed through "
                               "the router, served by Ollama in the lab) did not answer, so deduplication did not run.",
                       fix=self._embeddings_fix())

    def _new_run_dir(self, skill: str) -> tuple[str, Path]:
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{int(time.time() * 1000) % 1000:03d}"
        run_id = f"{stamp}-{skill}"
        run_dir = self._runs_dir / run_id
        run_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        return run_id, run_dir

    async def _evaluate(self, launcher: str, skill: str, path: Path, tiers: str) -> dict[str, Any]:
        run_id, run_dir = await asyncio.to_thread(self._new_run_dir, skill)
        t0 = time.monotonic()
        argv = [launcher, "tier1", str(path)]
        if tiers == "static":
            argv.append("--no-llm")
        argv += ["-r", "json,markdown", "-o", str(run_dir)]
        r1 = await self._run(argv, timeout=RUN_TIMEOUT, cwd=run_dir)
        meta: dict[str, Any] = {"run_id": run_id, "skill": skill, "tiers": tiers, "ts": time.time(),
                                "model_calls": tiers != "static", "launcher": launcher}
        t1 = self._load(run_dir / "skillevaluator-tier1.json")
        if t1 is None:
            meta["verdict"] = "error"
            meta["error"] = ("timed out" if r1.timed_out else (r1.stderr or r1.stdout).strip()[-1500:]) or f"exit {r1.returncode}"
        else:
            meta["tier1"] = summarize_tier1(t1)
            meta["failed_checks"] = failed_checks(t1)
            meta["verdict"] = meta["tier1"]["status"] or "error"
        if tiers == "tier1+2":
            r2 = await self._run([launcher, "tier2", str(path), "-r", "json", "-o", str(run_dir)],
                                 timeout=RUN_TIMEOUT, cwd=run_dir)
            t2 = self._load(run_dir / "skillevaluator-tier2.json")
            meta["tier2_status"] = (t2.get("overall_status") if isinstance(t2, dict) else None) or "error"
            if meta["tier2_status"] == "error":
                meta["tier2_error"] = ("timed out" if r2.timed_out else (r2.stderr or r2.stdout).strip()[-800:]) or f"exit {r2.returncode}"
            # embeddings unavailable: the report says so in incomplete_scans, a run without one in its error text
            inc = [str(x) for x in _list(_dict(t2).get("incomplete_scans"))]
            if "embedding-provider" in inc or "embedding" in str(meta.get("tier2_error", "")).lower():
                meta["tier2_no_embeddings"] = True
            if meta["tier2_status"] != "passed" and meta["verdict"] == "passed":
                meta["verdict"] = "failed" if meta["tier2_status"] == "failed" else meta["tier2_status"]
        meta["duration_ms"] = int((time.monotonic() - t0) * 1000)
        await asyncio.to_thread(self._write_meta, run_dir, meta)
        await asyncio.to_thread(self._prune)
        return meta

    # ── Tier 3: one live run in the background ──────────────────────────────────────────────
    async def _start_live(self, params: dict[str, object], done: Done) -> ActionResult:
        busy = self._busy_result(done)
        if busy is not None:
            return busy
        # The core validated types and ranges against the ActionSpec; these re-checks also cover direct callers.
        skill = str(params.get("skill") or "")
        try:
            path = self._skill_path(skill)
        except KeyError:
            return done(False, f"unknown skill {skill!r}")
        attempts = params.get("attempts", 1)
        if isinstance(attempts, bool) or not isinstance(attempts, int) or not 1 <= attempts <= LIVE_MAX_ATTEMPTS:
            return done(False, f"attempts must be an integer from 1 to {LIVE_MAX_ATTEMPTS}")
        models: dict[str, str] = {}
        for key, default in (("agent_model", DEFAULT_AGENT_MODEL), ("judge_model", DEFAULT_JUDGE_MODEL)):
            value = params.get(key)
            value = default if value in (None, "") else value
            if not isinstance(value, str) or MODEL_RE.fullmatch(value) is None:
                return done(False, f"{key} must be a router model alias (letters, digits and ._:/@-)")
            models[key] = value
        dataset = params.get("dataset") or "auto"
        if dataset not in LIVE_DATASETS:
            return done(False, f"dataset must be one of {', '.join(LIVE_DATASETS)}")
        skip_baseline = params.get("skip_baseline", False)
        if not isinstance(skip_baseline, bool):
            return done(False, "skip_baseline must be true or false")
        launcher, _ = await self._launcher()
        if launcher is None:
            return done(False, "SkillEvaluator is not installed.", [Problem(
                severity="error", code="skillevaluator.not_installed", message="SkillEvaluator is not on PATH.",
                fix=self._install_fix())])
        if launcher != LIVE_LAUNCHER:
            return done(False, "Refused: Tier 3 runs an agent that executes the skill's own scripts, so it only runs "
                               "through the skillevaluator-local wrapper, which contains each job in a sandboxed runner "
                               "container. Plain `skillevaluator` would run it with SkillEvaluator's stock sandboxes "
                               "(open network).", [Problem(
                                   severity="error", code="skillevaluator.live.needs_wrapper",
                                   message="Tier 3 needs the skillevaluator-local wrapper on the console's PATH.",
                                   fix=self._wrapper_fix())])
        busy = self._busy_result(done)              # re-check: the launcher probe awaited
        if busy is not None:
            return busy
        # From here to create_task nothing awaits, so no second request can slip in between the check and the start.
        run_id, run_dir = self._new_run_dir(skill)
        argv = [launcher, "tier3", str(path), "--n-attempts", str(attempts),
                "--agent-model", f"{LIVE_AGENT}={models['agent_model']}", "--judge-model", models["judge_model"],
                "--dataset", str(dataset)]
        if skip_baseline:
            argv.append("--skip-baseline")
        # --quiet: stdout carries only the final summary JSON, so an hour of progress lines cannot hit the output cap
        # (which kills the child); the launcher keeps them in its own job log.
        argv += ["--results-dir", str(run_dir), "--quiet"]
        started = time.time()
        meta: dict[str, Any] = {"run_id": run_id, "skill": skill, "tiers": "tier3", "ts": started, "verdict": "running",
                                "model_calls": True, "launcher": launcher,
                                "live": {"agent": LIVE_AGENT, "agent_model": models["agent_model"],
                                         "judge_model": models["judge_model"], "attempts": attempts,
                                         "dataset": dataset, "skip_baseline": skip_baseline}}
        self._write_meta(run_dir, meta)
        task = asyncio.create_task(self._live_task(run_dir, argv, meta), name=f"skilleval-live-{run_id}")
        self._live = _LiveRun(run_id=run_id, skill=skill, started=started, task=task)
        return done(True, f"Started live evaluation {run_id} ({skill}; {LIVE_AGENT}={models['agent_model']}, judge "
                          f"{models['judge_model']}, {attempts} attempt(s), dataset {dataset}"
                          f"{', no baseline' if skip_baseline else ''}). It runs in the background: watch the runs "
                          "list, then 'Show a stored report' with this run id.")

    def _busy_result(self, done: Done) -> ActionResult | None:
        busy = self._live_run_id()
        if busy is None:
            return None
        return done(False, f"A live evaluation is already running ({busy}); one runs at a time. Wait for "
                                           "it to finish: it shows in the runs list.", [Problem(
                                               severity="warn", code="skillevaluator.live.busy",
                                               message=f"live evaluation {busy} is still running",
                                               fix=f"'Show a stored report' with run id {busy} shows its progress")])

    async def _live_task(self, run_dir: Path, argv: list[str], meta: dict[str, Any]) -> None:
        t0 = time.monotonic()
        finishing = False
        try:
            r = await self._run(argv, timeout=LIVE_TIMEOUT, max_bytes=LIVE_MAX_BYTES, cwd=run_dir)
            finishing = True                # from here the worker thread writes the outcome; never overwrite it
            await asyncio.to_thread(self._finish_live, run_dir, meta, r, int((time.monotonic() - t0) * 1000))
        except asyncio.CancelledError:
            # The console is stopping: the runner has already interrupted the launcher (it stops its job container).
            if not finishing:
                meta.update(verdict="interrupted", interrupted_by="console stop",
                            duration_ms=int((time.monotonic() - t0) * 1000))
                with contextlib.suppress(OSError):
                    self._write_meta(run_dir, meta)
            raise
        except Exception as e:  # noqa: BLE001 - bookkeeping failed: still close the run, never leave it "running"
            meta.update(verdict="error", error=f"{type(e).__name__}: {e}"[:300])
            with contextlib.suppress(OSError):
                self._write_meta(run_dir, meta)

    def _finish_live(self, run_dir: Path, meta: dict[str, Any], r: CommandResult, ms: int) -> None:
        meta["duration_ms"] = ms
        out = last_json_object(r.stdout.strip()) or {}
        if isinstance(out.get("container_exit"), int) and not isinstance(out.get("container_exit"), bool):
            meta["container_exit"] = out["container_exit"]
        if isinstance(out.get("job"), str) and RESULT_NAME_RE.fullmatch(out["job"]):
            meta["job"] = out["job"]
        result_path = self._find_result(run_dir)
        result = self._load_result(result_path) if result_path is not None else None
        if result is None:
            t3_summary = _dict(_dict(_dict(out.get("summary")).get("tiers")).get("3"))
            why = t3_summary.get("error") if isinstance(t3_summary.get("error"), str) else None
            meta["verdict"] = "error"
            meta["error"] = (why or ("timed out" if r.timed_out else (r.stderr or r.stdout).strip()[-1500:])
                             or f"exit {r.returncode}")
        else:
            t3 = summarize_tier3(result)
            meta["conclusions"] = t3.pop("conclusions")
            meta["recommendations"] = t3.pop("recommendations")
            meta["tier3"] = t3
            meta["verdict"] = tier3_verdict(t3)
            assert result_path is not None
            meta["result"] = str(result_path.relative_to(run_dir))
        self._write_meta(run_dir, meta)
        self._prune()

    @staticmethod
    def _find_result(run_dir: Path) -> Path | None:
        """<skill>/<run>/result.json as launch.py copies it (regular files only, never through a symlink)."""
        found: list[Path] = []
        with contextlib.suppress(OSError):
            for skill_dir in sorted(run_dir.iterdir())[:50]:
                if skill_dir.is_symlink() or not skill_dir.is_dir() or not RESULT_NAME_RE.fullmatch(skill_dir.name):
                    continue
                for rd in sorted(skill_dir.iterdir())[:50]:
                    p = rd / "result.json"
                    if (rd.name != "latest" and RESULT_NAME_RE.fullmatch(rd.name) and not rd.is_symlink()
                            and rd.is_dir() and not p.is_symlink() and p.is_file()):
                        found.append(p)
        return max(found, key=lambda p: p.parent.name) if found else None

    @staticmethod
    def _load_result(p: Path) -> dict[str, Any] | None:
        """result.json was written under the influence of the skill being evaluated: no-follow open, regular file,
        size-capped, a JSON object or nothing."""
        try:
            fd = os.open(p, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
        except OSError:
            return None
        try:
            st = os.fstat(fd)
            if not stat.S_ISREG(st.st_mode) or st.st_size > MAX_RESULT_BYTES:
                return None
            with os.fdopen(fd, "rb", closefd=False) as fh:
                d = json.loads(fh.read(MAX_RESULT_BYTES + 1))
        except (OSError, ValueError):
            return None
        finally:
            os.close(fd)
        return d if isinstance(d, dict) else None

    @staticmethod
    def _load(p: Path) -> dict[str, Any] | None:
        try:
            if p.stat().st_size > 8 * 1024 * 1024:
                return None
            d = json.loads(p.read_text())
        except (OSError, ValueError):
            return None
        return d if isinstance(d, dict) else None

    @staticmethod
    def _write_meta(run_dir: Path, meta: dict[str, Any]) -> None:
        tmp = run_dir / ".run.json.tmp"
        tmp.write_text(json.dumps(redact_meta(meta), indent=1))
        tmp.chmod(0o600)
        tmp.replace(run_dir / "run.json")

    # ── reports ─────────────────────────────────────────────────────────────────────────────
    def _report_text(self, run_id: str) -> tuple[bool, str]:
        if run_id:
            if not RUN_ID_RE.fullmatch(run_id):
                return False, "unknown run id"
            run_dir = self._runs_dir / run_id
        else:
            runs = self._list_runs(1)
            if not runs:
                return False, "no stored runs yet"
            run_dir = self._runs_dir / str(runs[0].data.get("run_id"))
        meta = self._load_meta(run_dir)
        if meta is None:
            return False, "unknown run id"
        if meta.get("tiers") == "tier3":
            return True, "\n".join(self._tier3_report(run_dir, meta))
        head = [self._event(meta).summary, ""]
        if meta.get("verdict") == "error":
            head += ["SkillEvaluator did not produce a report:", str(meta.get("error", "")), ""]
        fc = _list(meta.get("failed_checks"))
        if fc:
            head += ["Top findings (medium and above):", *[f"  {x}" for x in fc], ""]
        if meta.get("tier2_status"):
            head += [f"Tier 2 (deduplication): {meta['tier2_status']}", ""]
        if meta.get("tier2_no_embeddings"):
            p = self._no_embeddings_problem()
            head += [p.message, f"Fix: {p.fix}", ""]
        md = run_dir / "skillevaluator-tier1.md"
        try:
            body = md.read_text()[:16_000] if md.stat().st_size <= 4 * 1024 * 1024 else "(report too large to show)"
        except OSError:
            body = ""
        return True, "\n".join(head) + ("\n" + body if body else "")

    def _tier3_report(self, run_dir: Path, meta: dict[str, Any]) -> list[str]:
        live = _dict(meta.get("live"))
        verdict = str(meta.get("verdict", "?"))
        lines = [self._event(meta).summary, "",
                 f"Status: {verdict}",
                 (f"Agent: {live.get('agent', LIVE_AGENT)} on {live.get('agent_model', '?')}; judge "
                  f"{live.get('judge_model', '?')}; {live.get('attempts', '?')} attempt(s) per case; dataset "
                  f"{live.get('dataset', '?')}; baseline {'skipped' if live.get('skip_baseline') else 'on'}")]
        t3 = _dict(meta.get("tier3"))
        if verdict == "running":
            mins = max(0.0, (time.time() - (_num(meta.get("ts")) or time.time())) / 60)
            lines.append(f"Running for {mins:.0f} min. This report fills in when the run ends (a 2-case skill takes "
                         "about 30 s; dataset generation and several attempts take much longer).")
            return lines
        if verdict == "interrupted":
            why = ("the console stopped while it ran (the launcher was interrupted and stops its job container)"
                   if meta.get("interrupted_by") == "console stop" else
                   "the console restarted while it ran, so nothing recorded its end")
            lines += ["", f"Interrupted: {why}."]
            late = self._find_result(run_dir)
            res = self._load_result(late) if late is not None else None
            if res is None:
                return lines
            t3 = summarize_tier3(res)
            meta = {**meta, "conclusions": t3.pop("conclusions"), "recommendations": t3.pop("recommendations")}
            lines.append(f"A result.json landed after that (the run finished on its own): execution_status "
                         f"{t3.get('execution_status') or '?'}.")
        if verdict == "error":
            lines += ["", "No Tier 3 result was written:", str(meta.get("error", "")).strip()]
            if meta.get("container_exit") is not None:
                lines.append(f"(runner container exit code {meta['container_exit']})")
            return lines
        sa, ea, dur = _num(t3.get("scored_attempts")), _num(t3.get("expected_attempts")), _num(t3.get("duration_seconds"))
        if sa is not None and ea is not None:
            lines.append(f"Attempts scored: {sa:.0f} of {ea:.0f}" + (f"; evaluation took {dur:.1f} s" if dur else ""))
        agents = _dict(t3.get("agents"))
        if agents:
            lines += ["", "Skill Lift = overall score with the skill minus without it (per agent):",
                      f"  {'agent':<12} {'model':<34} {'lift':>7} {'with':>7} {'without':>8}  status"]
            for name, a in agents.items():
                a = _dict(a)
                lines.append(f"  {name[:12]:<12} {str(a.get('model') or '-')[:34]:<34} "
                             f"{_fmt(a.get('skill_lift'), signed=True):>7} {_fmt(a.get('with_skill')):>7} "
                             f"{_fmt(a.get('without_skill')):>8}  {a.get('execution_status') or '-'}")
                metrics = _dict(a.get("metrics"))
                if metrics:
                    lines.append(f"    {'metric':<20} {'with':>7} {'without':>8} {'delta':>7}")
                    for mk, mv in metrics.items():
                        mv = _dict(mv)
                        lines.append(f"    {mk[:20]:<20} {_fmt(mv.get('with_skill')):>7} {_fmt(mv.get('without_skill')):>8} "
                                     f"{_fmt(mv.get('delta'), signed=True):>7}")
                for err in _list(a.get("errors")):
                    lines.append(f"    error: {err}")
        for title, key in (("Conclusions", "conclusions"), ("Recommendations", "recommendations")):
            items = _list(meta.get(key))
            if items:
                lines += ["", f"{title}:"]
                for it in items:
                    it = _dict(it)
                    head = f"{it.get('title')}: " if it.get("title") and it.get("title") != it.get("message") else ""
                    lines.append(f"  [{it.get('severity', 'info')}] {head}{it.get('message', '')}")
        rel = meta.get("result")
        if isinstance(rel, str) and not rel.startswith("/") and ".." not in rel:
            html = run_dir / Path(rel).parent / "report.html"
            if html.is_file() and not html.is_symlink():
                lines += ["", f"Full report (HTML, open it locally): {html}"]
        return lines


def redact_meta(meta: dict[str, Any]) -> dict[str, Any]:
    """run.json holds metadata only; error text is redacted like every other output."""
    out = dict(meta)
    for k in ("error", "tier2_error"):
        if isinstance(out.get(k), str):
            out[k] = redact(out[k])
    return out
