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
    endpoint; Tier 1 without --no-llm needs a chat model [source].
  * Tier 3 (live agent runs through Harbor) is NOT offered here: its stock Docker sandboxes run with
    network_mode "public" and install agent CLIs from the internet [source: tier3/harbor/adapter.py].
  * Launcher: `skillevaluator-local` when it is on PATH (a wrapper that injects the provider settings and reads its
    own 0600 key file), else `skillevaluator`, which then needs SKILL_EVAL_* in the console's environment (the runner
    passes those to SkillEvaluator binaries only). The console never reads a key file and never returns a key.
"""
from __future__ import annotations

import asyncio
import json
import re
import shutil
import time
from collections.abc import AsyncIterator
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


def _num(v: object) -> float | None:
    return float(v) if isinstance(v, int | float) and not isinstance(v, bool) else None


def _dict(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


def _list(v: object) -> list[Any]:
    return v if isinstance(v, list) else []


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


class SkillEvaluatorAdapter(SystemAdapter):
    id = "skillevaluator"
    label = "SkillEvaluator"

    # ctx.paths: skills_dir (the skill roots' parent: one sub-directory per skill, each with SKILL.md), state.
    # ctx.urls: "litellm" (the router the launcher points SkillEvaluator at; only its liveness is probed).
    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self._version_cache: tuple[float, str | None, str | None] = (0.0, None, None)   # (ts, launcher, version)
        self.follow_interval = 2.0

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

    # ── stored runs ─────────────────────────────────────────────────────────────────────────
    def _read_run(self, run_dir: Path) -> SystemEvent | None:
        try:
            meta = json.loads((run_dir / "run.json").read_text())
        except (OSError, ValueError):
            return None
        if not isinstance(meta, dict):
            return None
        s = _dict(meta.get("tier1"))
        parts = [str(meta.get("skill", "?")), str(meta.get("tiers", "?"))]
        if s.get("quality_score") is not None:
            parts.append(f"quality {s['quality_score']:.1f}{' ' + s['grade'] if s.get('grade') else ''}")
        if s.get("rubric_score") is not None:
            parts.append(f"judge {s['rubric_score']:.1f}")
        parts.append(str(meta.get("verdict", "?")))
        if s.get("incomplete"):
            parts[-1] += f" ({', '.join(s['incomplete'])} incomplete)"
        ts = _num(meta.get("ts")) or 0.0
        return SystemEvent(ts=ts, system="skillevaluator", kind=EVENT_KIND, summary=" · ".join(parts)[:300],
                           data={k: meta.get(k) for k in ("run_id", "skill", "tiers", "verdict", "model_calls", "tier1",
                                                          "tier2_status", "duration_ms")})

    def _list_runs(self, limit: int) -> list[SystemEvent]:
        try:
            dirs = sorted((p for p in self._runs_dir.iterdir() if RUN_ID_RE.fullmatch(p.name) and p.is_dir()),
                          key=lambda p: p.name, reverse=True)
        except OSError:
            return []
        out: list[SystemEvent] = []
        for d in dirs[: max(limit, 0)]:
            ev = self._read_run(d)
            if ev is not None:
                out.append(ev)
        return out

    def _prune(self) -> None:
        try:
            dirs = sorted((p for p in self._runs_dir.iterdir() if RUN_ID_RE.fullmatch(p.name) and p.is_dir()),
                          key=lambda p: p.name, reverse=True)
        except OSError:
            return
        for d in dirs[KEEP_RUNS:]:
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
        if launcher is None:
            problems.append(Problem(
                severity="error", code="skillevaluator.not_installed",
                message="SkillEvaluator is not installed (or not on the console's PATH).",
                fix='uv tool install --python 3.13 "skillevaluator[all] @ git+https://github.com/NVIDIA/SkillEvaluator.git"'))
        elif launcher == "skillevaluator":
            problems.append(Problem(
                severity="info", code="skillevaluator.no_wrapper",
                message="Running plain `skillevaluator`: LLM checks need SKILL_EVAL_* (provider, base URL, key) in the "
                        "console's environment; without them Tier 1 runs static checks only and Tier 2 fails.",
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
        if runs:
            t1 = runs[0].data.get("tier1") or {}
            if isinstance(t1, dict):
                for key, field in (("se.last_quality_score", "quality_score"), ("se.last_judge_score", "rubric_score")):
                    v = _num(t1.get(field))
                    if v is not None:
                        points.append(MetricPoint(key=key, value=v, unit="1"))
            verdicts = [r.data.get("verdict") for r in runs]
            for verdict in ("passed", "failed", "incomplete", "error"):
                points.append(MetricPoint(key=f"se.runs_by_verdict{{verdict={verdict}}}",
                                          value=float(verdicts.count(verdict)), unit="1"))
        return MetricsSnapshot(system="skillevaluator", ts=now, available=True, points=points)

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        return await asyncio.to_thread(self._list_runs, min(limit, KEEP_RUNS))

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        seen: set[str] = set()
        first = True
        while True:
            events = await asyncio.to_thread(self._list_runs, 50)
            for ev in sorted(events, key=lambda e: e.ts):
                rid = str(ev.data.get("run_id"))
                if rid in seen:
                    continue
                seen.add(rid)
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
        return [
            ActionSpec(
                id="evaluate", label="Evaluate a skill", danger="safe",
                description="Runs SkillEvaluator on one skill and keeps the JSON + Markdown reports in the console's "
                            "state dir (newest 200 runs). 'static' makes no model calls; 'tier1' adds the LLM judge "
                            "through the router; 'tier1+2' also runs embedding deduplication. Tier 3 (live agent runs) "
                            "is not offered: its sandboxes need open internet.",
                params=[
                    ParamSpec(key="skill", label="Skill", type="enum", choices=names or None,
                              default=names[0] if names else None, requires_restart=False,
                              help="A directory under the skills dir that contains SKILL.md."),
                    ParamSpec(key="tiers", label="Tiers", type="enum", choices=list(TIERS), default="tier1",
                              requires_restart=False, help="; ".join(f"{k}: {v}" for k, v in TIERS.items())),
                ]),
            ActionSpec(
                id="evaluate_all_static", label="Static check of every skill", danger="safe",
                description="Tier 1 static checks (no model calls) on every skill, one run each; prints a score table. "
                            "Takes about 2 s per skill."),
            ActionSpec(
                id="report", label="Show a stored report", danger="safe",
                description="The Markdown report of a stored run (newest first).",
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

        if action_id not in {"evaluate", "evaluate_all_static", "report", "doctor"}:
            raise KeyError(action_id)
        if action_id == "report":
            return done(*await asyncio.to_thread(self._report_text, str(params.get("run_id") or "")))
        launcher, _ = await self._launcher()
        if launcher is None:
            return done(False, "SkillEvaluator is not installed.", [Problem(
                severity="error", code="skillevaluator.not_installed", message="SkillEvaluator is not on PATH.",
                fix='uv tool install --python 3.13 "skillevaluator[all] @ git+https://github.com/NVIDIA/SkillEvaluator.git"')])
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
            return done(meta.get("verdict") in ("passed", "incomplete", "failed"), text[1])
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

    async def _evaluate(self, launcher: str, skill: str, path: Path, tiers: str) -> dict[str, Any]:
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{int(time.time() * 1000) % 1000:03d}"
        run_id = f"{stamp}-{skill}"
        run_dir = self._runs_dir / run_id
        await asyncio.to_thread(run_dir.mkdir, mode=0o700, parents=True, exist_ok=True)
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
            if meta["tier2_status"] != "passed" and meta["verdict"] == "passed":
                meta["verdict"] = "failed" if meta["tier2_status"] == "failed" else meta["tier2_status"]
        meta["duration_ms"] = int((time.monotonic() - t0) * 1000)
        await asyncio.to_thread(self._write_meta, run_dir, meta)
        await asyncio.to_thread(self._prune)
        return meta

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
        ev = self._read_run(run_dir)
        if ev is None:
            return False, "unknown run id"
        meta = ev.data
        head = [ev.summary, ""]
        if meta.get("verdict") == "error":
            try:
                err = json.loads((run_dir / "run.json").read_text()).get("error", "")
            except (OSError, ValueError):
                err = ""
            head += ["SkillEvaluator did not produce a report:", str(err), ""]
        try:
            fc = json.loads((run_dir / "run.json").read_text()).get("failed_checks") or []
        except (OSError, ValueError):
            fc = []
        if fc:
            head += ["Top findings (medium and above):", *[f"  {x}" for x in fc], ""]
        if meta.get("tier2_status"):
            head += [f"Tier 2 (deduplication): {meta['tier2_status']}", ""]
        md = run_dir / "skillevaluator-tier1.md"
        try:
            body = md.read_text()[:16_000] if md.stat().st_size <= 4 * 1024 * 1024 else "(report too large to show)"
        except OSError:
            body = ""
        return True, "\n".join(head) + ("\n" + body if body else "")


def redact_meta(meta: dict[str, Any]) -> dict[str, Any]:
    """run.json holds metadata only; error text is redacted like every other output."""
    out = dict(meta)
    for k in ("error", "tier2_error"):
        if isinstance(out.get(k), str):
            out[k] = redact(out[k])
    return out
