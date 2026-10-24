"""NeMo Evaluator adapter: NVIDIA NeMo Evaluator (`nel`, a CLI, not a service) as a managed system.

Facts this adapter is built on (contract 9.8, 2026-10-24; evidence in tests/evaluator/fixtures, captured live):
  * `evaluator-local` is the lab wrapper (clients/nemo-evaluator/install.sh links it onto PATH). It pins
    nemo-evaluator 0.3.0 and holds ALL policy: every command except `prefetch` runs with HF_HUB_OFFLINE=1 /
    HF_DATASETS_OFFLINE=1, the router key is read by the wrapper from its own 0600 file into NEMO_API_KEY (the
    console passes NO env and never sees a key). `--version` prints "0.3.0" [fixture version.txt].
  * `prefetch` is the ONLY step with internet and is deliberately NOT an action here: a human runs it once.
  * `cached --json` lists the lab-cache state per benchmark [fixture cached.json]; `suites --file F` validates a
    suites file with the wrapper's own code.
  * `run --suite NAME --output-dir DIR` runs `nel eval run` once per benchmark and writes DIR/summary.json
    (schema 1: suite, model, state, benchmarks[] with status/score/ci/samples/tokens/...) [fixtures summary_*.json].
    summary.json holds numbers and names only. nel's own result files (results.jsonl, trajectories.jsonl,
    inference_log.jsonl) contain dataset questions and model completions: this adapter NEVER reads them.
  * suites.yaml is the lab's named runs; the schema (own, enforced here AND by the wrapper) rejects benchmarks
    that need a Docker sandbox or an LLM judge, unknown keys, and unbounded runs.
"""
from __future__ import annotations

import asyncio
import contextlib
import json
import os
import re
import shutil
import time
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import yaml

from nemo_console.adapters.base import AdapterContext, CommandResult, SystemAdapter
from nemo_console.domain.models import (
    ActionResult,
    ActionSpec,
    ApplyResult,
    Component,
    ConfigFile,
    ConfigFileSpec,
    Diagnostic,
    Endpoint,
    MetricPoint,
    MetricsSnapshot,
    ParamSpec,
    Problem,
    SystemEvent,
    SystemStatus,
    ValidationResult,
)
from nemo_console.services.common import redact, sha256_hex

LAUNCHER = "evaluator-local"
RUN_TIMEOUT = 4800.0                      # = runner.EVALUATOR_RUN; the runner caps argv anyway
QUICK_TIMEOUT = 60.0                      # version / cached / suites --file
RUN_MAX_BYTES = 4_194_304                 # 4 MiB stdout cap for `run` (nel's progress lines)
KEEP_RUNS = 100
EVENT_KIND = "evaluator.run"
VERSION_TTL = 300.0
CACHED_TTL = 20.0
VERDICTS = ("succeeded", "failed", "error", "interrupted")
RUN_ID_RE = re.compile(r"\d{8}T\d{6}-\d{3}-[a-z0-9][a-z0-9-]{0,39}")
MAX_SCORE_PAIRS = 50                      # bounded label cardinality for ev.score{bench,model}
MAX_CONFIG_BYTES = 256 * 1024
FILE_ID = "suites.yaml"
LAB_INSTALLER = Path("clients") / "nemo-evaluator" / "install.sh"
LAB_MINT_KEY = Path("clients") / "nemo-evaluator" / "mint-key.sh"
PINNED_VERSION = "0.3.0"

# ── the suite schema (own; the wrapper enforces the same rules in clients/nemo-evaluator/evaluator_cli.py) ──
SUPPORTED_BENCHMARKS = ("drop", "gsm8k", "math500", "mmlu", "mmlu_pro", "triviaqa")
REFUSED_BENCHMARKS = {
    "humaneval": "needs a Docker sandbox to execute generated code",
    "pinchbench": "needs a Docker sandbox",
    "terminal-bench-hard": "needs a Docker sandbox",
    "terminal-bench-hard-aa-split": "needs a Docker sandbox",
    "terminal-bench-v1": "needs a Docker sandbox",
    "nmp_harbor": "needs a Docker sandbox",
    "simpleqa": "needs an LLM judge",
    "healthbench": "needs an LLM judge",
    "xstest": "needs an LLM judge and downloads its CSV from github.com at run time",
    "gpqa": "the dataset is gated on Hugging Face (needs an access token)",
    "mgsm": "its dataset loader runs remote code (trust_remote_code)",
}
NAME_RE = re.compile(r"[a-z0-9][a-z0-9-]{0,39}")
MODEL_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._/:@-]{0,127}")
RUN_KEYS = ("bench", "max_problems", "max_tokens", "temperature", "repeats")
SUITE_KEYS = ("description", "model", "benchmarks")
MAX_SUITES = 20
MAX_BENCHES = 8
MAX_PROBLEMS = 5000
MAX_WORK = 20000

Done = Callable[..., ActionResult]          # run_action's `done(ok, output, problems=None)`


def _num(v: object) -> float | None:
    return float(v) if isinstance(v, int | float) and not isinstance(v, bool) else None


def _int(v: object) -> bool:
    return isinstance(v, int) and not isinstance(v, bool)


def _dict(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


def _list(v: object) -> list[Any]:
    return v if isinstance(v, list) else []


def validate_suites(doc: object) -> list[tuple[str, str | None]]:
    """(message, suite name for line lookup) for every schema error of a parsed suites.yaml; empty = valid."""
    errs: list[tuple[str, str | None]] = []
    if not isinstance(doc, dict):
        return [("top level must be a mapping with `version` and `suites`", None)]
    for k in sorted(str(k) for k in doc if k not in ("version", "suites")):
        errs.append((f"unknown top-level key {k!r}", k))
    if doc.get("version") != 1:
        errs.append(("`version` must be 1", "version"))
    suites = doc.get("suites")
    if not isinstance(suites, dict) or not suites:
        errs.append(("`suites` must be a non-empty mapping of suite name -> suite", "suites"))
        return errs
    if len(suites) > MAX_SUITES:
        errs.append((f"at most {MAX_SUITES} suites", "suites"))
    for name, suite in suites.items():
        where = f"suite {name!r}"
        key = str(name)
        if not isinstance(name, str) or not NAME_RE.fullmatch(name):
            errs.append((f"{where}: name must match [a-z0-9][a-z0-9-]{{0,39}}", key))
        if not isinstance(suite, dict):
            errs.append((f"{where}: must be a mapping", key))
            continue
        for k in sorted(str(k) for k in suite if k not in SUITE_KEYS):
            errs.append((f"{where}: unknown key {k!r} (allowed: {', '.join(SUITE_KEYS)})", k))
        desc = suite.get("description", "")
        if not isinstance(desc, str) or len(desc) > 200:
            errs.append((f"{where}: description must be a string of at most 200 characters", "description"))
        model = suite.get("model")
        if not isinstance(model, str) or not MODEL_RE.fullmatch(model):
            errs.append((f"{where}: `model` must be a router model alias (letters, digits and . _ / : @ -)",
                         "model"))
        benches = suite.get("benchmarks")
        if not isinstance(benches, list) or not benches:
            errs.append((f"{where}: `benchmarks` must be a non-empty list", "benchmarks"))
            continue
        if len(benches) > MAX_BENCHES:
            errs.append((f"{where}: at most {MAX_BENCHES} benchmarks", "benchmarks"))
        seen: set[str] = set()
        work = 0
        for i, b in enumerate(benches[:MAX_BENCHES]):
            bw = f"{where} benchmark #{i + 1}"
            if not isinstance(b, dict):
                errs.append((f"{bw}: must be a mapping", key))
                continue
            for k in sorted(str(k) for k in b if k not in RUN_KEYS):
                errs.append((f"{bw}: unknown key {k!r} (allowed: {', '.join(RUN_KEYS)})", k))
            bench = b.get("bench")
            if not isinstance(bench, str):
                errs.append((f"{bw}: `bench` is required", key))
            elif bench in REFUSED_BENCHMARKS:
                errs.append((f"{bw}: {bench!r} is not supported — {REFUSED_BENCHMARKS[bench]}", bench))
            elif bench not in SUPPORTED_BENCHMARKS:
                errs.append((f"{bw}: unknown benchmark {bench!r} (supported: {', '.join(SUPPORTED_BENCHMARKS)})",
                             bench))
            elif bench in seen:
                errs.append((f"{bw}: {bench!r} is listed twice in this suite", bench))
            else:
                seen.add(bench)
            mp_raw, rp_raw = b.get("max_problems"), b.get("repeats", 1)
            mp = mp_raw if isinstance(mp_raw, int) and not isinstance(mp_raw, bool) else 0
            rp = rp_raw if isinstance(rp_raw, int) and not isinstance(rp_raw, bool) else 1
            if not 1 <= mp <= MAX_PROBLEMS:
                errs.append((f"{bw}: `max_problems` is required, an integer from 1 to {MAX_PROBLEMS}", key))
                mp = 0
            if not 1 <= rp <= 8:
                errs.append((f"{bw}: `repeats` must be an integer from 1 to 8", "repeats"))
                rp = 1
            mt = b.get("max_tokens")
            if mt is not None and (not _int(mt) or not 16 <= mt <= 32768):
                errs.append((f"{bw}: `max_tokens` must be an integer from 16 to 32768", "max_tokens"))
            t = b.get("temperature")
            if t is not None and (_num(t) is None or not 0 <= float(t) <= 2):
                errs.append((f"{bw}: `temperature` must be a number from 0 to 2", "temperature"))
            work += mp * rp
        if work > MAX_WORK:
            errs.append((f"{where}: max_problems x repeats sums to {work}; the limit is {MAX_WORK}", key))
    return errs


@dataclass
class _LiveRun:
    """The ONE background evaluation this console owns. The task holds the only reference keeping it alive."""
    run_id: str
    suite: str
    started: float
    task: asyncio.Task[None]


class EvaluatorAdapter(SystemAdapter):
    id = "evaluator"
    label = "NeMo Evaluator"

    # ctx.paths: evaluator_dir (the lab's clients/nemo-evaluator), evaluator_suites (suites.yaml, EDITABLE),
    # evaluator_key_file (EXISTENCE only), repo (install-fix pointers only).
    # ctx.urls: "litellm" (the router the suites call; only its liveness is probed).
    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self._version_cache: tuple[float, str | None] = (0.0, None)        # (ts, version)
        self._cached_cache: tuple[float, dict[str, bool] | None] = (0.0, None)
        self.follow_interval = 2.0
        self._live: _LiveRun | None = None

    # ── helpers ─────────────────────────────────────────────────────────────────────────────
    @property
    def _litellm(self) -> str:
        return self.ctx.urls.get("litellm", "http://127.0.0.1:4000").rstrip("/")

    @property
    def _suites_path(self) -> Path | None:
        return self.ctx.paths.get("evaluator_suites")

    @property
    def _key_file(self) -> Path | None:
        return self.ctx.paths.get("evaluator_key_file")

    @property
    def _runs_dir(self) -> Path:
        return self.ctx.state_dir / "evaluator" / "runs"

    def _lab_file(self, rel: Path) -> Path | None:
        repo = self.ctx.paths.get("repo")
        return repo / rel if repo is not None and (repo / rel).exists() else None

    def _install_fix(self) -> str:
        lab = self._lab_file(LAB_INSTALLER)
        return (f"{lab}   # pinned nemo-evaluator {PINNED_VERSION} + the evaluator-local wrapper" if lab else
                f"run clients/nemo-evaluator/install.sh in the lab repository (pinned nemo-evaluator "
                f"{PINNED_VERSION}; it links the evaluator-local wrapper onto PATH)")

    def _key_fix(self) -> str:
        lab = self._lab_file(LAB_MINT_KEY)
        return (f"{lab}   # mints the router key file evaluator-local reads" if lab else
                "run clients/nemo-evaluator/mint-key.sh in the lab repository (it writes "
                "~/.config/nemo-evaluator/litellm.key, 0600)")

    async def _run(self, argv: list[str], *, timeout: float, max_bytes: int = 256_000,
                   cwd: Path | None = None) -> CommandResult:
        try:
            return await self.ctx.runner.run(argv, timeout=timeout, max_bytes=max_bytes, cwd=cwd)
        except Exception as e:  # noqa: BLE001 - not allowlisted / not on PATH: data, not an exception
            return CommandResult(returncode=127, stdout="", stderr=f"{type(e).__name__}: {e}"[:300])

    async def _version(self) -> str | None:
        ts, version = self._version_cache
        if time.monotonic() - ts < VERSION_TTL and version is not None:
            return version
        r = await self._run([LAUNCHER, "--version"], timeout=20.0, max_bytes=4096)
        version = None
        if r.returncode == 0:
            first = r.stdout.strip().splitlines()[:1]
            version = first[0][:40] if first else None
        self._version_cache = (time.monotonic(), version)
        return version

    async def _cached(self, *, fresh: bool = False) -> dict[str, bool] | None:
        """bench -> prefetched?, from `evaluator-local cached --json`; None when it could not be read."""
        ts, data = self._cached_cache
        if not fresh and time.monotonic() - ts < CACHED_TTL and data is not None:
            return data
        r = await self._run([LAUNCHER, "cached", "--json"], timeout=QUICK_TIMEOUT, max_bytes=32_768)
        data = None
        if r.returncode == 0:
            try:
                doc = json.loads(r.stdout)
                rows = _list(_dict(doc).get("benchmarks"))
                data = {str(_dict(x).get("bench")): bool(_dict(x).get("cached")) for x in rows[:50]
                        if isinstance(_dict(x).get("bench"), str)}
            except ValueError:
                data = None
        self._cached_cache = (time.monotonic(), data)
        return data

    async def _router_up(self) -> tuple[bool, str]:
        try:
            resp = await self.ctx.http.get(f"{self._litellm}/health/liveliness", timeout=3.0, follow_redirects=False)
        except Exception as e:  # noqa: BLE001
            return False, type(e).__name__
        return resp.status_code < 500, f"HTTP {resp.status_code}"

    def _live_run_id(self) -> str | None:
        live = self._live
        return live.run_id if live is not None and not live.task.done() else None

    # ── suites.yaml (read-only parse; never raises) ─────────────────────────────────────────
    def _read_suites_text(self) -> str | None:
        p = self._suites_path
        if p is None or p.is_symlink() or not p.is_file():
            return None
        try:
            if p.stat().st_size > MAX_CONFIG_BYTES:
                return None
            return p.read_text()
        except OSError:
            return None

    def suites(self) -> tuple[dict[str, dict[str, Any]], list[str]]:
        """(valid suites by name, error messages). A missing / broken / invalid file is zero suites plus a message."""
        text = self._read_suites_text()
        if text is None:
            return {}, [f"{self._suites_path or 'suites.yaml'} is missing, a symlink or too large"]
        try:
            doc = yaml.safe_load(text)
        except yaml.YAMLError as e:
            return {}, [f"YAML error: {str(e).splitlines()[0][:200] if str(e) else type(e).__name__}"]
        errs = [m for m, _ in validate_suites(doc)]
        if errs:
            return {}, errs[:10]
        out = {str(k): dict(v) for k, v in _dict(_dict(doc).get("suites")).items()}
        return out, []

    @staticmethod
    def _suite_benches(suite: dict[str, Any]) -> list[str]:
        return [str(_dict(b).get("bench")) for b in _list(suite.get("benchmarks"))]

    # ── stored runs ─────────────────────────────────────────────────────────────────────────
    def _load_meta(self, run_dir: Path) -> dict[str, Any] | None:
        """run.json; a run left at `running` by a process that no longer runs it reads as interrupted."""
        try:
            meta = json.loads((run_dir / "run.json").read_text())
        except (OSError, ValueError):
            return None
        if not isinstance(meta, dict):
            return None
        if meta.get("verdict") == "running" and meta.get("run_id") != self._live_run_id():
            meta = {**meta, "verdict": "interrupted", "interrupted_by": "restart"}
        return meta

    @staticmethod
    def _scores_text(meta: dict[str, Any]) -> str:
        bits = []
        for r in _list(meta.get("results"))[:8]:
            rr = _dict(r)
            sc = _num(rr.get("score"))
            bits.append(f"{rr.get('bench', '?')} {sc:.3f}" if sc is not None else f"{rr.get('bench', '?')} {rr.get('status', '?')}")
        return ", ".join(bits)

    def _event(self, meta: dict[str, Any]) -> SystemEvent:
        parts = [str(meta.get("suite", "?")), str(meta.get("model", "?"))]
        scores = self._scores_text(meta)
        if scores:
            parts.append(scores)
        parts.append(str(meta.get("verdict", "?")))
        if meta.get("verdict") == "running":
            mins = max(0.0, (time.time() - (_num(meta.get("ts")) or time.time())) / 60)
            parts.append(f"running for {mins:.0f} min")
        data: dict[str, Any] = {k: meta.get(k) for k in ("run_id", "suite", "model", "verdict", "duration_ms")}
        data["benchmarks"] = [
            {k: _dict(r).get(k) for k in ("bench", "status", "metric", "score", "samples")}
            for r in _list(meta.get("results"))[:8]]
        return SystemEvent(ts=_num(meta.get("ts")) or 0.0, system="evaluator", kind=EVENT_KIND,
                           summary=redact(" · ".join(parts))[:300], data=data)

    def _run_dirs(self) -> list[Path]:
        try:
            return sorted((p for p in self._runs_dir.iterdir() if RUN_ID_RE.fullmatch(p.name) and p.is_dir()
                           and not p.is_symlink()), key=lambda p: p.name, reverse=True)
        except OSError:
            return []

    def _metas(self, limit: int) -> list[dict[str, Any]]:
        out: list[dict[str, Any]] = []
        for d in self._run_dirs()[: max(limit, 0)]:
            meta = self._load_meta(d)
            if meta is not None:
                out.append(meta)
        return out

    def _list_runs(self, limit: int) -> list[SystemEvent]:
        return [self._event(m) for m in self._metas(limit)]

    def _prune(self) -> None:
        live = self._live_run_id()
        for d in self._run_dirs()[KEEP_RUNS:]:
            if d.name != live:                      # never delete the directory a live run is writing into
                shutil.rmtree(d, ignore_errors=True)

    def _new_run_dir(self, suite: str) -> tuple[str, Path]:
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{int(time.time() * 1000) % 1000:03d}"
        run_id = f"{stamp}-{suite}"
        run_dir = self._runs_dir / run_id
        run_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        return run_id, run_dir

    @staticmethod
    def _write_meta(run_dir: Path, meta: dict[str, Any]) -> None:
        out = dict(meta)
        if isinstance(out.get("error"), str):
            out["error"] = redact(out["error"])
        tmp = run_dir / ".run.json.tmp"
        tmp.write_text(json.dumps(out, indent=1))
        tmp.chmod(0o600)
        tmp.replace(run_dir / "run.json")

    @staticmethod
    def _read_summary(run_dir: Path) -> dict[str, Any] | None:
        """The wrapper's summary.json (names and numbers only). Anything else under out/ is never read."""
        p = run_dir / "out" / "summary.json"
        try:
            if p.is_symlink() or p.stat().st_size > 1_000_000:
                return None
            d = json.loads(p.read_text())
        except (OSError, ValueError):
            return None
        return d if isinstance(d, dict) else None

    @staticmethod
    def _results(summary: dict[str, Any]) -> list[dict[str, Any]]:
        keep = ("bench", "status", "metric", "score", "ci_low", "ci_high", "samples", "max_problems", "tokens",
                "elapsed_s", "failure_rate")
        out: list[dict[str, Any]] = []
        for b in _list(summary.get("benchmarks"))[:MAX_BENCHES]:
            bb = _dict(b)
            row = {k: bb.get(k) for k in keep if bb.get(k) is not None}
            if isinstance(bb.get("error"), str):
                row["error"] = redact(bb["error"])[:300]
            if isinstance(row.get("bench"), str):
                out.append(row)
        return out

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    async def status(self) -> SystemStatus:
        version, (router_ok, router_detail), cached = await asyncio.gather(
            self._version(), self._router_up(), self._cached())
        suites, suite_errs = await asyncio.to_thread(self.suites)
        last = await asyncio.to_thread(self._list_runs, 1)
        problems: list[Problem] = []
        components = [
            Component(kind="process", name=LAUNCHER, state="running" if version else "absent",
                      detail=(f"{LAUNCHER} {version} (nel; runs are OFFLINE: Hugging Face access is switched off "
                              "and the router key is read by the wrapper from its own key file)") if version else
                      f"{LAUNCHER} is not on PATH or not installed (clients/nemo-evaluator/install.sh)"),
            Component(kind="http", name="router " + self._litellm, state="running" if router_ok else "stopped",
                      detail=f"{router_detail} on GET /health/liveliness (every evaluation request goes through it)"),
        ]
        live = self._live if self._live_run_id() else None
        if live is not None:
            mins = max(0.0, (time.time() - live.started) / 60)
            components.append(Component(kind="process", name=f"run {live.suite}", state="running",
                                        detail=f"evaluation {live.run_id}, running for {mins:.0f} min "
                                               "(one at a time; it shows in the runs list)"))
        if version is None:
            problems.append(Problem(severity="error", code="ev.not_installed",
                                    message="NeMo Evaluator is not installed (or not on the console's PATH).",
                                    fix=self._install_fix()))
        if not router_ok:
            problems.append(Problem(
                severity="warn", code="ev.router_down",
                message=f"The router ({self._litellm}) is not answering; evaluation runs will fail.",
                fix=f"curl -s -o /dev/null -w '%{{http_code}}\\n' {self._litellm}/health/liveliness"))
        if version is not None and self._key_file is not None and not self._key_file.exists():
            problems.append(Problem(severity="warn", code="ev.no_key",
                                    message=f"The router key file {self._key_file} is absent; evaluator-local run "
                                            "will refuse to start.",
                                    fix=self._key_fix()))
        if suite_errs:
            problems.append(Problem(severity="error", code="ev.suites_invalid",
                                    message="suites.yaml is not valid: " + "; ".join(suite_errs[:3]),
                                    fix="edit it in the Configure tab (the validator names each problem), or restore "
                                        "clients/nemo-evaluator/suites.yaml from git"))
        if version is not None and cached is not None and suites:
            missing = {name: [b for b in dict.fromkeys(self._suite_benches(s)) if not cached.get(b, False)]
                       for name, s in suites.items()}
            missing = {n: bs for n, bs in missing.items() if bs}
            if missing:
                need = sorted({b for bs in missing.values() for b in bs})
                problems.append(Problem(
                    severity="warn", code="ev.dataset_missing",
                    message="Datasets not prefetched (runs are offline and would fail): "
                            + "; ".join(f"{n}: {', '.join(bs)}" for n, bs in sorted(missing.items())),
                    fix=f"evaluator-local prefetch {' '.join(need)}   # the only step with internet; run it yourself"))
        if last and last[0].data.get("verdict") in ("failed", "error"):
            problems.append(Problem(severity="info", code="ev.last_run_failed",
                                    message=f"The newest run failed: {last[0].summary}",
                                    fix="run the 'Show a stored run' action for its error"))
        health = "down" if version is None else ("degraded" if not router_ok or suite_errs else "up")
        cached_n = sum(1 for v in (cached or {}).values() if v)
        summary = f"{len(suites)} suites · {cached_n}/{len(cached or {})} datasets cached"
        if last:
            summary += f" · last run: {last[0].summary}"
        return SystemStatus(
            system="evaluator", health=health, version=version, summary=summary[:300],
            endpoints=[Endpoint(name="router (evaluation)", url=self._litellm, role="api", auth="bearer")],
            components=components, problems=problems, checked_at=time.time())

    async def metrics(self) -> MetricsSnapshot:
        now = time.time()
        suites, _ = await asyncio.to_thread(self.suites)
        metas = await asyncio.to_thread(self._metas, KEEP_RUNS)
        points = [MetricPoint(key="ev.suites", value=float(len(suites)), unit="1"),
                  MetricPoint(key="ev.runs", value=float(len(metas)), unit="1")]
        if metas:                                    # a live run always has a run directory, so this covers it
            points.append(MetricPoint(key="ev.live_running", value=1.0 if self._live_run_id() else 0.0, unit="1"))
            verdicts = [m.get("verdict") for m in metas]
            for verdict in VERDICTS:
                points.append(MetricPoint(key="ev.runs_by_verdict", value=float(verdicts.count(verdict)), unit="1",
                                          labels={"verdict": verdict}))
        # newest score per (bench, model): metas are newest-first, so the first sighting wins
        seen: set[tuple[str, str]] = set()
        for m in metas:
            model = str(m.get("model") or "")
            for r in _list(m.get("results")):
                rr = _dict(r)
                bench, score = rr.get("bench"), _num(rr.get("score"))
                if not isinstance(bench, str) or score is None or not model or (bench, model) in seen:
                    continue
                if len(seen) >= MAX_SCORE_PAIRS:
                    break
                seen.add((bench, model))
                points.append(MetricPoint(key="ev.score", value=score, unit="ratio",
                                          labels={"bench": bench, "model": model}))
        return MetricsSnapshot(system="evaluator", ts=now, available=True, points=points)

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        return await asyncio.to_thread(self._list_runs, min(limit, KEEP_RUNS))

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        # keyed by (run, verdict): a background run is announced when it starts AND when it finishes
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

    # ── configure: suites.yaml (own schema; authoritative) ──────────────────────────────────
    def config_files(self) -> list[ConfigFileSpec]:
        p = self._suites_path
        if p is None:
            return []
        return [ConfigFileSpec(
            id=FILE_ID, label="Evaluation suites",
            description="Named NeMo Evaluator runs: a router model alias plus 1-8 benchmarks, each with a REQUIRED "
                        "max_problems bound. Benchmarks that need a Docker sandbox or an LLM judge are rejected. "
                        "Datasets must be prefetched by hand (evaluator-local prefetch): runs are offline.",
            display_path="clients/nemo-evaluator/suites.yaml", format="yaml", editable=True,
            validators=["suites.yaml schema (console)", "evaluator-local suites --file"], authoritative=True,
            apply_effect="takes effect on the next run; nothing to restart")]

    def config_path(self, file_id: str) -> Path:
        p = self._suites_path
        if file_id != FILE_ID or p is None:
            raise KeyError(file_id)
        if p.is_symlink() or not p.is_file():
            raise KeyError(file_id)
        return p.resolve()

    async def read_config(self, file_id: str) -> ConfigFile:
        p = self.config_path(file_id)               # KeyError for an unknown id
        try:
            if p.stat().st_size > MAX_CONFIG_BYTES:
                raise FileNotFoundError(file_id)
            content = p.read_text()
            mtime = p.stat().st_mtime
        except OSError:
            raise FileNotFoundError(file_id) from None
        return ConfigFile(spec=self.config_files()[0], content=content, sha256=sha256_hex(content.encode()),
                          mtime=mtime)

    @staticmethod
    def _line_of(text: str, needle: str | None) -> int | None:
        if not needle:
            return None
        pat = re.compile(rf"^\s*(?:-\s+)?{re.escape(needle)}\s*:|^\s*-\s+bench:\s*{re.escape(needle)}\b")
        for i, line in enumerate(text.splitlines(), start=1):
            if pat.search(line):
                return i
        return None

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        self.config_path(file_id)
        name = "suites.yaml schema (console)"
        if len(content.encode()) > MAX_CONFIG_BYTES:
            return ValidationResult(ok=False, validator=name, authoritative=True,
                                    diagnostics=[Diagnostic(severity="error", message="file is larger than 256 KiB")])
        try:
            doc = yaml.safe_load(content)
        except yaml.YAMLError as e:
            mark = getattr(e, "problem_mark", None)
            problem = getattr(e, "problem", None) or "invalid YAML"
            return ValidationResult(ok=False, validator=name, authoritative=True, diagnostics=[Diagnostic(
                severity="error", message=redact(f"YAML error: {problem}")[:300],
                line=(mark.line + 1) if mark is not None else None)])
        diags = [Diagnostic(severity="error", message=m[:300], line=self._line_of(content, key))
                 for m, key in validate_suites(doc)]
        notes = ["Checked against the suite schema; model aliases are not checked against the router."]
        if not diags and self._cached_cache[1] is not None:
            cached = self._cached_cache[1]
            for sname, suite in _dict(_dict(doc).get("suites")).items():
                miss = [b for b in dict.fromkeys(self._suite_benches(_dict(suite))) if not cached.get(b, False)]
                if miss:
                    diags.append(Diagnostic(severity="warn", line=self._line_of(content, str(sname)), message=(
                        f"suite {sname!r}: dataset(s) not prefetched: {', '.join(miss)} "
                        f"(evaluator-local prefetch {' '.join(miss)})")))
        if not any(d.severity == "error" for d in diags) and await self._version() is not None:
            wrapper_diags = await self._wrapper_validate(file_id, content)
            diags += wrapper_diags
            notes.append("The wrapper's own validator (evaluator-local suites --file) agrees." if not wrapper_diags
                         else "The wrapper's own validator disagreed.")
        ok = not any(d.severity == "error" for d in diags)
        return ValidationResult(ok=ok, validator=name, authoritative=True, diagnostics=diags, note=" ".join(notes))

    async def _wrapper_validate(self, file_id: str, content: str) -> list[Diagnostic]:
        tmp_dir = self.ctx.state_dir / "evaluator" / "tmp"
        await asyncio.to_thread(tmp_dir.mkdir, 0o700, True, True)
        tmp = tmp_dir / f"suites.{os.urandom(6).hex()}.yaml"
        try:
            await asyncio.to_thread(tmp.write_text, content)
            tmp.chmod(0o600)
            r = await self._run([LAUNCHER, "suites", "--file", str(tmp)], timeout=QUICK_TIMEOUT, max_bytes=16_384)
        finally:
            with contextlib.suppress(OSError):
                tmp.unlink(missing_ok=True)
        if r.returncode == 0:
            return []
        tail = [redact(line.strip())[:300] for line in (r.stderr + "\n" + r.stdout).splitlines() if line.strip()]
        return [Diagnostic(severity="error", message="evaluator-local: " + m) for m in tail[-6:]]

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        self.config_path(file_id)
        self._cached_cache = (0.0, None)
        return ApplyResult(ok=True, post_check="healthy",
                           detail="nothing to restart: suites.yaml is read by the next run")

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    def actions(self) -> list[ActionSpec]:
        names = list(self.suites()[0])
        return [
            ActionSpec(
                id="run", label="Run a suite", danger="disruptive",
                description="Runs `evaluator-local run --suite NAME` in the background (ONE at a time): each "
                            "benchmark's sampled problems go through the router to the suite's model (spends router "
                            "tokens; minutes for large suites). The run is OFFLINE: datasets must already be "
                            "prefetched. Scores appear under metrics (ev.score) and in the runs list.",
                params=[ParamSpec(key="suite", label="Suite", type="enum", choices=names or None,
                                  default=names[0] if names else None, requires_restart=False,
                                  help="A suite from suites.yaml.")]),
            ActionSpec(
                id="report", label="Show a stored run", danger="safe",
                description="The report of a stored run (newest first): verdict and a table of per-benchmark "
                            "scores. Numbers only: no questions or model answers are shown.",
                params=[ParamSpec(key="run_id", label="Run", type="string", requires_restart=False,
                                  help="A run id from the runs list; empty = the newest run.")]),
        ]

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        t0 = time.monotonic()

        def done(ok: bool, out: str, problems: list[Problem] | None = None) -> ActionResult:
            return ActionResult(ok=ok, output=redact(out)[:20_000], duration_ms=int((time.monotonic() - t0) * 1000),
                                problems=problems or [])

        if action_id == "run":
            return await self._start_run(params, done)
        if action_id == "report":
            return done(*await asyncio.to_thread(self._report_text, str(params.get("run_id") or "")))
        raise KeyError(action_id)

    def _busy_result(self, done: Done) -> ActionResult | None:
        busy = self._live_run_id()
        if busy is None:
            return None
        return done(False, f"An evaluation is already running ({busy}); one runs at a time. Wait for it to finish: "
                           "it shows in the runs list.", [Problem(
                               severity="warn", code="ev.run.busy", message=f"evaluation {busy} is still running",
                               fix=f"'Show a stored run' with run id {busy} shows its progress")])

    async def _start_run(self, params: dict[str, object], done: Done) -> ActionResult:
        busy = self._busy_result(done)
        if busy is not None:
            return busy
        suites, errs = await asyncio.to_thread(self.suites)
        if errs:
            return done(False, "suites.yaml is not valid: " + "; ".join(errs[:3]), [Problem(
                severity="error", code="ev.suites_invalid", message="suites.yaml is not valid",
                fix="fix it in the Configure tab")])
        suite_name = str(params.get("suite") or "")
        suite = suites.get(suite_name)
        if suite is None:
            return done(False, f"unknown suite {suite_name!r} (have: {', '.join(suites) or 'none'})")
        if await self._version() is None:
            return done(False, "NeMo Evaluator is not installed.", [Problem(
                severity="error", code="ev.not_installed", message="evaluator-local is not on PATH.",
                fix=self._install_fix())])
        if self._key_file is not None and not self._key_file.exists():
            return done(False, f"The router key file {self._key_file} is absent.", [Problem(
                severity="warn", code="ev.no_key", message="no router key for evaluator-local", fix=self._key_fix())])
        cached = await self._cached(fresh=True)
        if cached is not None:
            miss = [b for b in dict.fromkeys(self._suite_benches(suite)) if not cached.get(b, False)]
            if miss:
                fix = f"evaluator-local prefetch {' '.join(miss)}"
                return done(False, f"Datasets not prefetched: {', '.join(miss)}. Runs are offline; fetch them once "
                                   f"yourself:\n  {fix}", [Problem(
                                       severity="warn", code="ev.dataset_missing",
                                       message=f"datasets not prefetched: {', '.join(miss)}", fix=fix)])
        busy = self._busy_result(done)              # re-check: the probes awaited
        if busy is not None:
            return busy
        # From here to create_task nothing awaits, so no second request can slip in between the check and the start.
        run_id, run_dir = self._new_run_dir(suite_name)
        out_dir = run_dir / "out"
        argv = [LAUNCHER, "run", "--suite", suite_name, "--output-dir", str(out_dir)]
        started = time.time()
        meta: dict[str, Any] = {"run_id": run_id, "suite": suite_name, "model": str(suite.get("model")),
                                "ts": started, "verdict": "running", "mode": "run",
                                "benchmarks": self._suite_benches(suite)}
        self._write_meta(run_dir, meta)
        task = asyncio.create_task(self._run_task(run_dir, argv, meta), name=f"ev-run-{run_id}")
        self._live = _LiveRun(run_id=run_id, suite=suite_name, started=started, task=task)
        return done(True, f"Started evaluation {run_id} (suite {suite_name}, model {meta['model']}; "
                          f"{', '.join(meta['benchmarks'])}). It runs in the background: watch the runs list, then "
                          "'Show a stored run' with this run id.")

    async def _run_task(self, run_dir: Path, argv: list[str], meta: dict[str, Any]) -> None:
        t0 = time.monotonic()
        finishing = False
        try:
            r = await self._run(argv, timeout=RUN_TIMEOUT, max_bytes=RUN_MAX_BYTES, cwd=run_dir)
            finishing = True                # from here this task writes the outcome; never overwrite it
            await asyncio.to_thread(self._finish_run, run_dir, meta, r, int((time.monotonic() - t0) * 1000))
        except asyncio.CancelledError:
            # The console is stopping: the runner has already interrupted the wrapper (SIGINT, then SIGKILL).
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

    def _finish_run(self, run_dir: Path, meta: dict[str, Any], r: CommandResult, ms: int) -> None:
        meta["duration_ms"] = ms
        summary = self._read_summary(run_dir)
        if summary is not None:
            meta["results"] = self._results(summary)
        state = summary.get("state") if summary is not None else None
        if r.timed_out:
            meta.update(verdict="failed", error="timed out")
        elif r.returncode == 130 or state == "interrupted":
            meta.update(verdict="interrupted", interrupted_by="signal")
        elif r.returncode == 0 and state == "succeeded":
            meta["verdict"] = "succeeded"
        elif r.returncode == 0:
            meta.update(verdict="error", error="the wrapper exited 0 but wrote no succeeded summary.json")
        else:
            errors = [str(_dict(x).get("error")) for x in _list(meta.get("results")) if _dict(x).get("error")]
            meta.update(verdict="failed",
                        error=(errors[-1] if errors else (r.stderr or r.stdout).strip()[-1500:]) or f"exit {r.returncode}")
        self._cached_cache = (0.0, None)
        self._write_meta(run_dir, meta)
        self._prune()

    # ── report ──────────────────────────────────────────────────────────────────────────────
    def _report_text(self, run_id: str) -> tuple[bool, str]:
        if run_id:
            if RUN_ID_RE.fullmatch(run_id) is None:
                return False, "run_id must be a run id from the runs list"
            d = self._runs_dir / run_id
            meta = self._load_meta(d) if d.is_dir() and not d.is_symlink() else None
        else:
            metas = self._metas(1)
            meta = metas[0] if metas else None
        if meta is None:
            return False, "no such run" if run_id else "no runs yet: start one with the 'Run a suite' action"
        lines = [f"Run {meta.get('run_id', '?')}", f"suite: {meta.get('suite', '?')} · model: {meta.get('model', '?')}",
                 f"verdict: {meta.get('verdict', '?')}"
                 + (f" · {(_num(meta.get('duration_ms')) or 0) / 1000:.1f} s" if meta.get("duration_ms") else "")]
        if meta.get("error"):
            lines.append(f"error: {str(meta['error'])[:600]}")
        rows = _list(meta.get("results"))[:MAX_BENCHES]
        if rows:
            lines += ["", "| Benchmark | Status | Metric | Score | CI (95%) | Samples | Tokens |",
                      "|---|---|---|---|---|---|---|"]
            for r in rows:
                rr = _dict(r)
                sc, lo, hi = _num(rr.get("score")), _num(rr.get("ci_low")), _num(rr.get("ci_high"))
                ci = f"{lo:.3f}..{hi:.3f}" if lo is not None and hi is not None else "-"
                lines.append(f"| {str(rr.get('bench', '?'))[:40]} | {str(rr.get('status', '?'))[:20]} | "
                             f"{str(rr.get('metric') or '-')[:20]} | {f'{sc:.4f}' if sc is not None else '-'} | {ci} | "
                             f"{rr.get('samples', '-')} | {rr.get('tokens', '-')} |")
        return meta.get("verdict") != "error", "\n".join(lines)
