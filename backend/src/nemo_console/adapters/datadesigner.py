"""Data Designer adapter: NVIDIA NeMo Data Designer (a CLI, not a service) as a managed system.

Facts this adapter is built on (contract 2026-10-23; evidence in tests/datadesigner/fixtures, captured live):
  * `data-designer-local` 0.9.3 is the lab wrapper (clients/data-designer/install.sh; also linked as bare
    `data-designer`, which the console does NOT use). `--version` prints "0.9.3" [fixture version.txt].
    The wrapper sets NEMO_TELEMETRY_ENABLED=false and injects LITELLM_DD_KEY / NVIDIA_API_KEY itself from
    0600 key files — the console passes NO env and NEVER sees a key.
  * `validate SCRIPT` prints a human-readable table; exit 0 when valid, != 0 with error text otherwise.
    Validation (and preview/create) EXECUTES the config script (it calls its load_config_builder()) —
    the tool's design; every UI-facing text says so.
  * `preview SCRIPT -n N --save-results -o DIR` writes DIR/preview_results_<YYYYMMDD_HHMMSS>/ with
    dataset.parquet, report.html and sample_records/*.html [fixture preview-artifacts/]. `--save-results`
    is ALWAYS passed (never the interactive TUI).
  * `create SCRIPT --num-records N --dataset-name NAME --artifact-path DIR --no-tui` writes DIR/<NAME>/ with
    metadata.json (actual_num_records, column_statistics[], file_paths{"parquet-files": [...]}, config_hash...),
    parquet-files/batch_*.parquet and builder_config.json [fixture create-artifacts/]. Long-running → ONE
    background run at a time (the SkillEvaluator Tier 3 pattern).
  * `check-models SOURCE` needs a config source argument (its own --help, fixture check-models-help.txt): it
    probes every model/MCP tool the config references.
  * The tool's own config: ~/.data-designer/model_providers.yaml (name, endpoint, provider_type,
    api_key = ENV NAME) and model_configs.yaml (alias, model, provider, inference_parameters). Parsed READ-ONLY,
    never raising, api_key never surfaced.
  * Parquet → csv/jsonl/json conversion is the fixed wrapper `data-designer-export IN.parquet OUT.ext`
    (the extension picks the format); the adapter reads only records.jsonl, never parquet.
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

LAUNCHERS = ("data-designer-local",)      # the bare `data-designer` link is NOT used by the console
EXPORTER = "data-designer-export"         # fixed parquet → csv/jsonl/json/parquet converter (one file in)
RUN_TIMEOUT = 540.0                       # preview / validate / check-models
CREATE_TIMEOUT = 3600.0                   # only `create` is long (= runner.DD_CREATE; the runner caps argv anyway)
EXPORT_TIMEOUT = 300.0                    # one parquet conversion
KEEP_RUNS = 100
EVENT_KIND = "dd.run"
MAX_PREVIEW_RECORDS = 2000                # records.jsonl line cap (the console copy), not the user's dataset copy
VERSION_TTL = 300.0
NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
RUN_ID_RE = re.compile(r"\d{8}T\d{6}-\d{3}-[A-Za-z0-9._-]{1,128}")
PREVIEW_DIR_RE = re.compile(r"preview_results_\d{8}_\d{6}")
FORMATS = ("parquet", "csv", "jsonl", "json")
VERDICTS = ("succeeded", "failed", "error", "interrupted")
CREATE_MAX_BYTES = 4_194_304              # 4 MiB stdout cap for `create` (progress lines only)
MAX_CONFIG_BYTES = 512 * 1024             # read_config cap
MAX_RECORDS_ROWS = 5000                   # defensive read bound; records.jsonl is capped at 2000 lines anyway
CELL_CAP = 2048                           # API: no row value exceeds 2 KiB
REPORT_CELL = 120                         # report markdown table cells
MAX_REPORT_ROWS = 5
MAX_FILES = 500
MAX_YAML_BYTES = 1024 * 1024
MAX_DEST_SEGMENTS = 8
# Script allowlist. The core's configure router accepts file ids matching [a-z0-9][a-z0-9_.\-]{0,63}
# (api/deps.ID_RE), so the id (= filename) must be the INTERSECTION of NAME_RE with that: lowercase, ≤ 64
# chars, one of .py/.yaml/.yml/.json (contract: NAME_RE + extension allowlist, narrowed by the LOCKED core).
SCRIPT_RE = re.compile(r"[a-z0-9][a-z0-9._-]{0,52}\.(?:py|yaml|yml|json)")
LAB_INSTALLER = Path("clients") / "data-designer" / "install.sh"
LAB_MINT_KEY = Path("clients") / "data-designer" / "mint-key.sh"
PINNED_VERSION = "0.9.3"

Done = Callable[..., ActionResult]          # run_action's `done(ok, output, problems=None)`


def _num(v: object) -> float | None:
    return float(v) if isinstance(v, int | float) and not isinstance(v, bool) else None


def _dict(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


def _list(v: object) -> list[Any]:
    return v if isinstance(v, list) else []


@dataclass
class _LiveRun:
    """The ONE background dataset build this console owns. The task holds the only reference keeping it alive."""
    run_id: str
    script: str
    started: float
    task: asyncio.Task[None]


class DataDesignerAdapter(SystemAdapter):
    id = "datadesigner"
    label = "Data Designer"

    # ctx.paths: scripts_dir (dataset config scripts), datasets_root (user dataset copies), dd_config_dir (the
    # tool's own ~/.data-designer), dd_key_file (EXISTENCE check only), repo (install-fix pointers only).
    # ctx.urls: "litellm" (the router the configured models point at; only its liveness is probed).
    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self._version_cache: tuple[float, str | None, str | None] = (0.0, None, None)   # (ts, launcher, version)
        self.follow_interval = 2.0
        self._live: _LiveRun | None = None
        self._export_error: str = ""            # redacted stderr tail of the last failed export (diagnostics)

    # ── helpers ─────────────────────────────────────────────────────────────────────────────
    @property
    def _litellm(self) -> str:
        return self.ctx.urls.get("litellm", "http://127.0.0.1:4000").rstrip("/")

    @property
    def _scripts_root(self) -> Path | None:
        return self.ctx.paths.get("scripts_dir")

    @property
    def _datasets_root(self) -> Path | None:
        return self.ctx.paths.get("datasets_root")

    @property
    def _config_dir(self) -> Path | None:
        return self.ctx.paths.get("dd_config_dir")

    @property
    def _key_file(self) -> Path | None:
        return self.ctx.paths.get("dd_key_file")

    @property
    def _runs_dir(self) -> Path:
        return self.ctx.state_dir / "datadesigner" / "runs"

    def _lab_file(self, rel: Path) -> Path | None:
        repo = self.ctx.paths.get("repo")
        return repo / rel if repo is not None and (repo / rel).exists() else None

    def _install_fix(self) -> str:
        lab = self._lab_file(LAB_INSTALLER)
        return (f"{lab}   # pinned data-designer {PINNED_VERSION} + the data-designer-local wrapper" if lab else
                f"run clients/data-designer/install.sh in the lab repository (pinned data-designer "
                f"{PINNED_VERSION}; it links the data-designer-local wrapper onto PATH)")

    def _key_fix(self) -> str:
        lab = self._lab_file(LAB_MINT_KEY)
        return (f"{lab}   # mints the router key file the wrapper injects as LITELLM_DD_KEY" if lab else
                "run clients/data-designer/mint-key.sh in the lab repository (it writes "
                "~/.config/data-designer/litellm.key, 0600)")

    # ── scripts ─────────────────────────────────────────────────────────────────────────────
    def scripts(self) -> list[str]:
        """Config script FILENAMES under scripts_dir: regular files (no symlinks) matching the allowlist."""
        root = self._scripts_root
        if root is None:
            return []
        try:
            entries = sorted(root.iterdir())
        except OSError:
            return []
        return [p.name for p in entries
                if SCRIPT_RE.fullmatch(p.name) and not p.is_symlink() and p.is_file()][:200]

    def _script_path(self, name: str) -> Path:
        """Resolve a script name to its file, refusing anything outside scripts_dir (symlinks included)."""
        root = self._scripts_root
        if root is None or SCRIPT_RE.fullmatch(name) is None:
            raise KeyError(name)
        raw = root / name
        base = root.resolve()
        p = raw.resolve()
        if raw.is_symlink() or p.parent != base or not p.is_file():
            raise KeyError(name)
        return p

    # ── launcher probe (TTL-cached; `--version` is a subprocess) ────────────────────────────
    async def _launcher(self) -> tuple[str | None, str | None]:
        ts, launcher, version = self._version_cache
        if time.monotonic() - ts < VERSION_TTL and launcher is not None:
            return launcher, version
        launcher, version = None, None
        for name in LAUNCHERS:
            r = await self._run([name, "--version"], timeout=20.0, max_bytes=4096)
            if r.returncode == 0:
                first = r.stdout.strip().splitlines()[:1]
                launcher, version = name, (first[0][:40] if first else None)
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
        live = self._live
        return live.run_id if live is not None and not live.task.done() else None

    # ── the tool's own yaml config (read-only, never raises, api_key never surfaced) ────────
    def _read_yaml_entries(self, filename: str) -> list[dict[str, Any]]:
        """model_providers.yaml / model_configs.yaml: a list of mappings, or a mapping name→mapping. A missing,
        oversized, symlinked or broken file is zero entries (failure is data; parsed on every status poll)."""
        path = self._config_dir / filename if self._config_dir is not None else None
        if path is None or path.is_symlink() or not path.is_file():
            return []
        try:
            if path.stat().st_size > MAX_YAML_BYTES:
                return []
            d = yaml.safe_load(path.read_text())
        except (OSError, yaml.YAMLError, ValueError):
            return []
        if isinstance(d, dict):
            key = "name" if filename == "model_providers.yaml" else "alias"
            # The tool's own shape is a single top-level key ("providers"/"model_configs") holding a
            # list; also accept a mapping name→mapping and a bare list.
            nested = next((v for v in d.values() if isinstance(v, list)), None)
            if nested is not None:
                d = nested
        if isinstance(d, list):
            return [dict(e) for e in _list(d)[:200] if isinstance(e, dict)]
        if isinstance(d, dict):
            out: list[dict[str, Any]] = []
            for k, v in list(d.items())[:200]:
                if isinstance(v, dict):
                    e = dict(v)
                    e.setdefault(key, k)
                    out.append(e)
            return out
        return []

    def providers(self) -> list[dict[str, str]]:
        """name / endpoint / provider_type of model_providers.yaml. The api_key field (an ENV NAME anyway) is
        NEVER included."""
        out: list[dict[str, str]] = []
        for e in self._read_yaml_entries("model_providers.yaml"):
            name = e.get("name")
            if not isinstance(name, str) or not NAME_RE.fullmatch(name):
                continue
            out.append({"name": name[:80], "endpoint": str(e.get("endpoint") or "")[:160],
                        "provider_type": str(e.get("provider_type") or "")[:40]})
        return out[:100]

    def aliases(self) -> list[dict[str, Any]]:
        """alias / model / provider / generation_type / temperature / top_p / max_tokens of model_configs.yaml."""
        out: list[dict[str, Any]] = []
        for e in self._read_yaml_entries("model_configs.yaml"):
            alias = e.get("alias")
            if not isinstance(alias, str) or not NAME_RE.fullmatch(alias):
                continue
            ip = _dict(e.get("inference_parameters"))
            gen = e.get("generation_type", ip.get("generation_type"))
            out.append({"alias": alias[:80], "model": str(e.get("model") or "")[:160],
                        "provider": str(e.get("provider") or "")[:80],
                        "generation_type": str(gen) if isinstance(gen, str) else None,
                        "temperature": _num(ip.get("temperature", e.get("temperature"))),
                        "top_p": _num(ip.get("top_p", e.get("top_p"))),
                        "max_tokens": _num(ip.get("max_tokens", e.get("max_tokens")))})
        return out[:100]

    def _config_dump(self) -> list[str]:
        providers, aliases = self.providers(), self.aliases()
        lines = [(f"Providers ({self._config_dir / 'model_providers.yaml' if self._config_dir else 'model_providers.yaml'})"
                  " — key values are never shown:"),
                 f"  {'name':<20} {'type':<16} endpoint"]
        lines += [f"  {p['name'][:20]:<20} {p['provider_type'][:16]:<16} {p['endpoint']}" for p in providers]
        lines += ["", f"Aliases ({self._config_dir / 'model_configs.yaml' if self._config_dir else 'model_configs.yaml'}):",
                  f"  {'alias':<16} {'model':<36} {'provider':<16} {'gen':<10} {'temp':>5} {'top_p':>6} {'max_tok':>8}"]
        for a in aliases:
            t, tp, mt = a["temperature"], a["top_p"], a["max_tokens"]
            lines.append(f"  {a['alias'][:16]:<16} {a['model'][:36]:<36} {a['provider'][:16]:<16} "
                         f"{str(a['generation_type'] or '-')[:10]:<10} "
                         f"{t if t is not None else '-':>5} {tp if tp is not None else '-':>6} "
                         f"{int(mt) if mt is not None else '-':>8}")
        return lines[:100]

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

    def _event(self, meta: dict[str, Any]) -> SystemEvent:
        parts = [str(meta.get("script", "?")), str(meta.get("mode", "?"))]
        records = _num(meta.get("records"))
        if records is not None:
            parts.append(f"{records:.0f} record(s)")
        if meta.get("mode") == "create":
            parts.append(f"{meta.get('dataset_name', '?')} → {meta.get('destination', '?')} ({meta.get('format', '?')})")
        parts.append(str(meta.get("verdict", "?")))
        if meta.get("verdict") == "running":
            mins = max(0.0, (time.time() - (_num(meta.get("ts")) or time.time())) / 60)
            parts.append(f"running for {mins:.0f} min")
        ts = _num(meta.get("ts")) or 0.0
        data: dict[str, Any] = {k: meta.get(k) for k in ("run_id", "script", "mode", "verdict",
                                                         "num_records_requested", "dataset_name", "destination",
                                                         "format", "batches", "duration_ms")}
        data["num_records"] = meta.get("records")     # metadata only: counts and names, never record contents
        return SystemEvent(ts=ts, system="datadesigner", kind=EVENT_KIND, summary=" · ".join(parts)[:300], data=data)

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

    def _new_run_dir(self, script: str) -> tuple[str, Path]:
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{int(time.time() * 1000) % 1000:03d}"
        run_id = f"{stamp}-{script}"
        run_dir = self._runs_dir / run_id
        run_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        return run_id, run_dir

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

    # ── records.jsonl (the ONLY dataset artifact the API reads; never parquet) ──────────────
    @staticmethod
    def _cap_value(v: object) -> object:
        """Pass values through as-is; no string exceeds CELL_CAP (nothing added, no marker fields)."""
        if isinstance(v, str):
            return v[:CELL_CAP]
        if isinstance(v, dict):
            return {str(k)[:120]: DataDesignerAdapter._cap_value(x) for k, x in list(v.items())[:50]}
        if isinstance(v, list):
            return [DataDesignerAdapter._cap_value(x) for x in v[:50]]
        return v if v is None or isinstance(v, bool | int | float) else str(v)[:CELL_CAP]

    def _read_records(self, path: Path) -> tuple[list[dict[str, Any]], list[str]]:
        """(rows, columns in first-seen order). Malformed lines are skipped, never raised."""
        rows: list[dict[str, Any]] = []
        columns: list[str] = []
        seen: set[str] = set()
        try:
            with path.open("rb") as fh:
                for line in fh:
                    if len(rows) >= MAX_RECORDS_ROWS:
                        break
                    try:
                        d = json.loads(line)
                    except ValueError:
                        continue
                    if not isinstance(d, dict):
                        continue
                    rows.append(d)
                    for k in d:
                        if isinstance(k, str) and k not in seen and len(columns) < 64:
                            seen.add(k)
                            columns.append(k)
        except OSError:
            return [], []
        return rows, columns

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    async def status(self) -> SystemStatus:
        (launcher, version), (router_ok, router_detail) = await asyncio.gather(self._launcher(), self._router_up())
        scripts = await asyncio.to_thread(self.scripts)
        aliases = await asyncio.to_thread(self.aliases)
        last = await asyncio.to_thread(self._list_runs, 1)
        problems: list[Problem] = []
        components = [
            Component(kind="process", name=launcher or "data-designer-local", state="running" if launcher else "absent",
                      detail=(f"{launcher} {version or ''} (wrapper: injects its own credentials; the console never "
                              "sees a key)") if launcher else
                      "data-designer-local is not on PATH (installed by clients/data-designer/install.sh)"),
            Component(kind="http", name="router " + self._litellm, state="running" if router_ok else "stopped",
                      detail=f"{router_detail} on GET /health/liveliness (generation goes through it)"),
        ]
        live = self._live if self._live_run_id() else None
        if live is not None:
            mins = max(0.0, (time.time() - live.started) / 60)
            components.append(Component(kind="process", name=f"create {live.script}", state="running",
                                        detail=f"dataset build {live.run_id}, running for {mins:.0f} min "
                                               "(one at a time; it shows in the runs list)"))
        if launcher is None:
            problems.append(Problem(severity="error", code="dd.not_installed",
                                    message="Data Designer is not installed (or not on the console's PATH).",
                                    fix=self._install_fix()))
        if not router_ok:
            problems.append(Problem(
                severity="warn", code="dd.router_down",
                message=f"The router ({self._litellm}) is not answering; preview/create/check-models will fail.",
                fix=f"curl -s -o /dev/null -w '%{{http_code}}\\n' {self._litellm}/health/liveliness"))
        root = self._scripts_root
        if root is None:
            problems.append(Problem(severity="warn", code="dd.no_scripts",
                                    message="No scripts directory is configured for Data Designer.",
                                    fix="set NEMO_CONSOLE_DD_SCRIPTS_DIR and restart nemo-console"))
        elif not root.is_dir():
            problems.append(Problem(severity="warn", code="dd.no_scripts",
                                    message=f"The scripts directory {root} does not exist.",
                                    fix=f"mkdir -p {root} && add a config script (.py defining "
                                        "load_config_builder(), or .yaml/.yml/.json)"))
        elif not scripts:
            problems.append(Problem(severity="warn", code="dd.no_scripts",
                                    message=f"No config scripts under {root}.",
                                    fix="add a config script: a .py defining load_config_builder(), or a "
                                        ".yaml/.yml/.json config"))
        if launcher is not None and self._key_file is not None and not self._key_file.exists():
            problems.append(Problem(severity="warn", code="dd.no_key",
                                    message=f"The wrapper's LITELLM_DD_KEY source file {self._key_file} is absent; "
                                            "runs through the lab router will fail.",
                                    fix=self._key_fix()))
        cfg_dir = self._config_dir
        if cfg_dir is not None and not cfg_dir.is_dir():
            problems.append(Problem(severity="warn", code="dd.no_config_dir",
                                    message=f"{cfg_dir} does not exist yet: no model providers or aliases are "
                                            "configured.",
                                    fix="run any data-designer-local command once (it creates the directory)"))
        health = "down" if launcher is None else ("degraded" if not router_ok else "up")
        summary = f"{len(scripts)} scripts · {len(aliases)} aliases"
        if last:
            summary += f" · last run: {last[0].summary}"
        return SystemStatus(
            system="datadesigner", health=health, version=version, summary=summary[:300],
            endpoints=[Endpoint(name="router (generation)", url=self._litellm, role="api", auth="bearer")],
            components=components, problems=problems, checked_at=time.time())

    async def metrics(self) -> MetricsSnapshot:
        now = time.time()
        scripts = await asyncio.to_thread(self.scripts)
        runs = await asyncio.to_thread(self._list_runs, KEEP_RUNS)
        points = [MetricPoint(key="dd.scripts", value=float(len(scripts)), unit="1"),
                  MetricPoint(key="dd.runs", value=float(len(runs)), unit="1")]
        if runs:                                     # a live run always has a run directory, so this covers it
            points.append(MetricPoint(key="dd.live_running", value=1.0 if self._live_run_id() else 0.0, unit="1"))
            last_records = next((_num(r.data.get("num_records")) for r in runs
                                 if _num(r.data.get("num_records")) is not None), None)
            if last_records is not None:
                points.append(MetricPoint(key="dd.last_records", value=last_records, unit="1"))
            verdicts = [r.data.get("verdict") for r in runs]
            for verdict in VERDICTS:
                points.append(MetricPoint(key=f"dd.runs_by_verdict{{verdict={verdict}}}",
                                          value=float(verdicts.count(verdict)), unit="1"))
        return MetricsSnapshot(system="datadesigner", ts=now, available=True, points=points)

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        return await asyncio.to_thread(self._list_runs, min(limit, KEEP_RUNS))

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        # keyed by (run, verdict): a background create is announced when it starts AND when it finishes
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

    # ── configure: dataset scripts as config files (the core's CAS/backup/revision machinery) ─
    def config_files(self) -> list[ConfigFileSpec]:
        root = self._scripts_root
        return [ConfigFileSpec(
            id=name, label=name, display_path=str(root / name) if root else name,
            description="A Data Designer dataset config script. Validating or applying runs "
                        "`data-designer-local validate`, which EXECUTES this script locally (it calls its "
                        "load_config_builder()) with your privileges.",
            format="yaml" if name.endswith((".yaml", ".yml")) else "text", editable=True,
            validators=["data-designer-local validate"], authoritative=True,
            apply_effect="takes effect on the next preview/create run; nothing to restart")
            for name in self.scripts()]

    def config_path(self, file_id: str) -> Path:
        root = self._scripts_root
        if root is None or SCRIPT_RE.fullmatch(file_id) is None:
            raise KeyError(file_id)
        raw = root / file_id
        p = raw.resolve()
        if raw.is_symlink() or p.parent != root.resolve() or not p.is_file():
            raise KeyError(file_id)
        return p

    async def read_config(self, file_id: str) -> ConfigFile:
        p = self.config_path(file_id)               # KeyError for an unknown id
        try:
            if p.stat().st_size > MAX_CONFIG_BYTES:
                raise FileNotFoundError(file_id)
            content = p.read_text()
            mtime = p.stat().st_mtime
        except OSError:
            raise FileNotFoundError(file_id) from None
        return ConfigFile(spec=next(s for s in self.config_files() if s.id == file_id), content=content,
                          sha256=sha256_hex(content.encode()), mtime=mtime)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        self.config_path(file_id)                   # only allowlisted ids validate (the id names the temp file)
        launcher, _ = await self._launcher()
        if launcher is None:
            return ValidationResult(ok=False, validator="data-designer-local validate", authoritative=True,
                                    diagnostics=[Diagnostic(severity="error",
                                                            message="data-designer-local is not installed.")])
        tmp_dir = self.ctx.state_dir / "datadesigner" / "tmp"
        await asyncio.to_thread(tmp_dir.mkdir, 0o700, True, True)
        tmp = tmp_dir / f"{Path(file_id).stem}.{os.urandom(6).hex()}{Path(file_id).suffix}"
        try:
            await asyncio.to_thread(tmp.write_text, content)
            tmp.chmod(0o600)
            r = await self._run([launcher, "validate", str(tmp)], timeout=RUN_TIMEOUT)
        finally:
            with contextlib.suppress(OSError):
                tmp.unlink(missing_ok=True)
        if r.timed_out:
            return ValidationResult(ok=False, validator="data-designer-local validate", authoritative=True,
                                    diagnostics=[Diagnostic(severity="error", message="validation timed out")])
        if r.returncode == 0:
            return ValidationResult(ok=True, validator="data-designer-local validate", authoritative=True,
                                    note="Validation ran `data-designer-local validate`, which EXECUTES this "
                                         "config script locally (it calls its load_config_builder()).")
        tail = [redact(line.strip())[:300] for line in (r.stderr + "\n" + r.stdout).splitlines() if line.strip()]
        return ValidationResult(ok=False, validator="data-designer-local validate", authoritative=True,
                                diagnostics=[Diagnostic(severity="error", message=m) for m in tail[-10:]])

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        self.config_path(file_id)
        return ApplyResult(ok=True, post_check="healthy",
                           detail="nothing to restart: scripts are read by the next preview/create run")

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    def actions(self) -> list[ActionSpec]:
        names = self.scripts()

        def script_param() -> ParamSpec:
            return ParamSpec(key="script", label="Config script", type="enum", choices=names or None,
                             default=names[0] if names else None, requires_restart=False,
                             help="A config script in the scripts dir. preview/create/validate EXECUTE it "
                                  "locally (it calls its load_config_builder()).")

        return [
            ActionSpec(
                id="preview", label="Preview records", danger="disruptive",
                description="Runs `data-designer-local preview <script> -n N --save-results`: N sample records "
                            "through the configured models (spends router tokens), keeping report.html, the "
                            "per-record HTML and records.jsonl in the console's state dir (newest 100 runs). "
                            "disruptive: it EXECUTES the config script locally and spends router tokens.",
                params=[script_param(),
                        ParamSpec(key="num_records", label="Records", type="int", default=5, min=1, max=100,
                                  requires_restart=False, help="Sample records to generate (1–100).")]),
            ActionSpec(
                id="create", label="Create a dataset", danger="disruptive",
                description="Runs `data-designer-local create` in the background (ONE at a time): builds the full "
                            "dataset through the configured models (spends router tokens, takes minutes), writes "
                            "the parquet batches into the run dir and a converted copy (your chosen format) under "
                            "the datasets root. The run shows in the runs list; 'Show a stored run' reports it.",
                params=[script_param(),
                        ParamSpec(key="num_records", flag="--num-records", label="Records", type="int", default=100,
                                  min=1, max=100_000, requires_restart=False, help="Dataset size (1–100000)."),
                        ParamSpec(key="dataset_name", flag="--dataset-name", label="Dataset name", type="string",
                                  requires_restart=False,
                                  help="Identifier for the dataset (letters, digits and . _ -). Default: "
                                       "dataset_<script stem>."),
                        ParamSpec(key="destination_subdir", label="Destination subdirectory", type="string",
                                  requires_restart=False,
                                  help="Relative path under the datasets root (no .., no leading /). Default: the "
                                       "dataset name."),
                        ParamSpec(key="format", label="Copy format", type="enum", choices=list(FORMATS),
                                  default="parquet", requires_restart=False,
                                  help="Format of the copy written under the datasets root; the run dir always "
                                       "keeps the parquet batches and records.jsonl (first 2000 records).")]),
            ActionSpec(
                id="report", label="Show a stored run", danger="safe",
                description="The report of a stored run (newest first): verdict, record count, destination and a "
                            "small table of the first records.",
                params=[ParamSpec(key="run_id", label="Run", type="string", requires_restart=False,
                                  help="A run id from the runs list; empty = the newest run.")]),
            ActionSpec(
                id="check_models", label="Check models", danger="disruptive",
                description="`data-designer-local check-models <script>`: a tiny generation against every model the "
                            "config references, plus a connectivity probe to each referenced MCP tool. "
                            "disruptive: it EXECUTES the config script locally and makes real model calls.",
                params=[script_param()]),
            ActionSpec(
                id="show_config", label="Show provider config", danger="safe",
                description="Read-only dump of the tool's own provider and alias tables "
                            "(~/.data-designer/model_providers.yaml + model_configs.yaml); key values are never "
                            "shown."),
        ]

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        t0 = time.monotonic()

        def done(ok: bool, out: str, problems: list[Problem] | None = None) -> ActionResult:
            return ActionResult(ok=ok, output=redact(out)[:20_000], duration_ms=int((time.monotonic() - t0) * 1000),
                                problems=problems or [])

        if action_id not in {"preview", "create", "report", "check_models", "show_config"}:
            raise KeyError(action_id)
        if action_id == "create":
            return await self._start_create(params, done)
        if action_id == "report":
            return done(*await asyncio.to_thread(self._report_text, str(params.get("run_id") or "")))
        launcher, _ = await self._launcher()
        if launcher is None:
            return done(False, "Data Designer is not installed.", [self._not_installed_problem()])
        if action_id == "show_config":
            return done(True, "\n".join(self._config_dump()))
        script = str(params.get("script") or "")
        try:
            path = self._script_path(script)
        except KeyError:
            return done(False, f"unknown script {script!r}")
        if action_id == "check_models":
            r = await self._run([launcher, "check-models", str(path)], timeout=RUN_TIMEOUT)
            return done(r.returncode == 0, (r.stdout + ("\n" + r.stderr if r.stderr else "")).strip())
        # preview
        n = params.get("num_records", 5)
        if isinstance(n, bool) or not isinstance(n, int) or not 1 <= n <= 100:
            return done(False, "num_records must be an integer from 1 to 100")
        return await self._preview(launcher, script, path, n, done)

    def _not_installed_problem(self) -> Problem:
        return Problem(severity="error", code="dd.not_installed", message="data-designer-local is not on PATH.",
                       fix=self._install_fix())

    async def _preview(self, launcher: str, script: str, path: Path, n: int, done: Done) -> ActionResult:
        run_id, run_dir = await asyncio.to_thread(self._new_run_dir, script)
        t0 = time.monotonic()
        argv = [launcher, "preview", str(path), "-n", str(n), "--save-results", "-o", str(run_dir)]
        r = await self._run(argv, timeout=RUN_TIMEOUT, cwd=run_dir)
        meta: dict[str, Any] = {"run_id": run_id, "script": script, "mode": "preview", "ts": time.time(),
                                "num_records_requested": n, "launcher": launcher}
        preview_dir = await asyncio.to_thread(self._newest_preview_dir, run_dir)
        records: int | None = None
        if preview_dir is not None:
            records = await self._export_records(preview_dir / "dataset.parquet", run_dir / "records.jsonl")
        if r.timed_out:
            meta.update(verdict="error", records=0, error="timed out")
        elif r.returncode != 0:
            meta.update(verdict="failed", records=0,
                        error=(r.stderr or r.stdout).strip()[-1500:] or f"exit {r.returncode}")
        elif preview_dir is None:
            meta.update(verdict="error", records=0, error="no preview_results_* directory was written")
        elif records is None:
            detail = f": {self._export_error}" if self._export_error else ""
            meta.update(verdict="error", records=0,
                        error=f"dataset.parquet could not be converted to records.jsonl{detail}")
        else:
            assert preview_dir is not None
            meta.update(verdict="succeeded", records=records, preview_dir=str(preview_dir.relative_to(run_dir)))
        meta["duration_ms"] = int((time.monotonic() - t0) * 1000)
        await asyncio.to_thread(self._write_meta, run_dir, meta)
        await asyncio.to_thread(self._prune)
        lines = [self._event(meta).summary, ""]
        if meta.get("verdict") != "succeeded":
            lines += ["Data Designer did not complete the preview:", str(meta.get("error", "")), ""]
        else:
            browser = run_dir / str(meta["preview_dir"]) / "sample_records" / "sample_records_browser.html"
            if browser.is_file() and not browser.is_symlink():
                lines.append(f"Per-record browser (HTML, open it locally): {browser}")
            lines.append(f"records.jsonl: {run_dir / 'records.jsonl'} (first {MAX_PREVIEW_RECORDS} records)")
        return done(meta.get("verdict") == "succeeded", "\n".join(lines))

    @staticmethod
    def _newest_preview_dir(run_dir: Path) -> Path | None:
        """The newest preview_results_<stamp>/ the CLI wrote (regular dirs only, never through a symlink)."""
        found: list[Path] = []
        with contextlib.suppress(OSError):
            for p in run_dir.iterdir():
                if PREVIEW_DIR_RE.fullmatch(p.name) and not p.is_symlink() and p.is_dir():
                    found.append(p)
        return max(found, key=lambda p: p.name) if found else None

    async def _export_records(self, parquet: Path, out: Path) -> int | None:
        """Convert one parquet to jsonl via data-designer-export, then copy the first MAX_PREVIEW_RECORDS lines to
        `out`. Returns the number of records kept, or None when the conversion failed (never invents a count).
        On failure, `self._export_error` carries the exporter's redacted stderr tail for the run's error text."""
        self._export_error = ""
        if parquet.is_symlink() or not parquet.is_file():
            self._export_error = f"{parquet.name} is missing"
            return None
        tmp = out.parent / f".records.{os.urandom(6).hex()}.jsonl"
        try:
            r = await self._run([EXPORTER, str(parquet), str(tmp)], timeout=EXPORT_TIMEOUT)
            if r.returncode != 0:
                self._export_error = redact((r.stderr or r.stdout).strip()[-300:]) or f"export exit {r.returncode}"
                return None
            count = 0
            with tmp.open("rb") as src, out.open("wb") as dst:
                for line in src:
                    if count >= MAX_PREVIEW_RECORDS:
                        break
                    dst.write(line)
                    if line.endswith(b"\n"):
                        count += 1
            out.chmod(0o600)
            return count
        except OSError:
            return None
        finally:
            with contextlib.suppress(OSError):
                tmp.unlink(missing_ok=True)

    # ── create: ONE background run (the SkillEvaluator Tier 3 pattern) ──────────────────────
    async def _start_create(self, params: dict[str, object], done: Done) -> ActionResult:
        busy = self._busy_result(done)
        if busy is not None:
            return busy
        script = str(params.get("script") or "")
        try:
            path = self._script_path(script)
        except KeyError:
            return done(False, f"unknown script {script!r}")
        n = params.get("num_records", 100)
        if isinstance(n, bool) or not isinstance(n, int) or not 1 <= n <= 100_000:
            return done(False, "num_records must be an integer from 1 to 100000")
        dataset_name = str(params.get("dataset_name") or "") or f"dataset_{Path(script).stem}"
        if NAME_RE.fullmatch(dataset_name) is None:
            return done(False, "dataset_name must use letters, digits and . _ - only")
        dest_sub = str(params.get("destination_subdir") or "") or dataset_name
        if not self._dest_subdir_ok(dest_sub):
            return done(False, "destination_subdir must be a relative path under the datasets root (segments of "
                               "letters, digits and . _ -; no .., no leading /)")
        fmt = params.get("format") or "parquet"
        if not isinstance(fmt, str) or fmt not in FORMATS:
            return done(False, f"format must be one of {', '.join(FORMATS)}")
        root = self._datasets_root
        if root is None:
            return done(False, "no datasets root is configured", [Problem(
                severity="error", code="dd.no_datasets_root", message="NEMO_CONSOLE_DD_DATASETS_ROOT is not set.",
                fix="set NEMO_CONSOLE_DD_DATASETS_ROOT and restart nemo-console")])
        root_r, dest = root.resolve(), (root / dest_sub).resolve()
        if dest == root_r or not dest.is_relative_to(root_r):
            return done(False, "destination_subdir resolves outside the datasets root")
        if dest.exists() and not dest.is_dir():
            return done(False, f"destination {dest} exists and is not a directory")
        target = None if fmt == "parquet" else dest / f"{dataset_name}.{fmt}"
        launcher, _ = await self._launcher()
        if launcher is None:
            return done(False, "Data Designer is not installed.", [self._not_installed_problem()])
        busy = self._busy_result(done)              # re-check: the launcher probe awaited
        if busy is not None:
            return busy
        # From here to create_task nothing awaits, so no second request can slip in between the check and the start.
        run_id, run_dir = self._new_run_dir(script)
        argv = [launcher, "create", str(path), "--num-records", str(n), "--dataset-name", dataset_name,
                "--artifact-path", str(run_dir), "--no-tui"]
        started = time.time()
        meta: dict[str, Any] = {"run_id": run_id, "script": script, "mode": "create", "ts": started,
                                "verdict": "running", "num_records_requested": n, "dataset_name": dataset_name,
                                "destination": dest_sub, "format": fmt, "launcher": launcher}
        self._write_meta(run_dir, meta)
        task = asyncio.create_task(self._create_task(run_dir, argv, meta, dest, target),
                                   name=f"dd-create-{run_id}")
        self._live = _LiveRun(run_id=run_id, script=script, started=started, task=task)
        return done(True, f"Started dataset build {run_id} ({script}; {n} records, dataset {dataset_name}, copy "
                          f"{fmt} → {dest}). It runs in the background: watch the runs list, then 'Show a stored "
                          "run' with this run id.")

    @staticmethod
    def _dest_subdir_ok(sub: str) -> bool:
        if not sub or sub.startswith("/") or "\\" in sub or len(sub) > 256:
            return False
        parts = sub.split("/")
        return (len(parts) <= MAX_DEST_SEGMENTS
                and all(p not in ("", ".", "..") and NAME_RE.fullmatch(p) for p in parts))

    def _busy_result(self, done: Done) -> ActionResult | None:
        busy = self._live_run_id()
        if busy is None:
            return None
        return done(False, f"A dataset build is already running ({busy}); one runs at a time. Wait for it to "
                                           "finish: it shows in the runs list.", [Problem(
                                               severity="warn", code="dd.create.busy",
                                               message=f"dataset build {busy} is still running",
                                               fix=f"'Show a stored run' with run id {busy} shows its progress")])

    async def _create_task(self, run_dir: Path, argv: list[str], meta: dict[str, Any], dest: Path,
                           target: Path | None) -> None:
        t0 = time.monotonic()
        finishing = False
        try:
            r = await self._run(argv, timeout=CREATE_TIMEOUT, max_bytes=CREATE_MAX_BYTES, cwd=run_dir)
            finishing = True                # from here this task writes the outcome; never overwrite it
            await self._finish_create(run_dir, meta, r, dest, target, int((time.monotonic() - t0) * 1000))
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

    async def _finish_create(self, run_dir: Path, meta: dict[str, Any], r: CommandResult, dest: Path,
                             target: Path | None, ms: int) -> None:
        meta["duration_ms"] = ms
        if r.timed_out or r.returncode != 0:
            meta.update(verdict="failed",
                        error=(r.stderr or r.stdout).strip()[-1500:] or
                              ("timed out" if r.timed_out else f"exit {r.returncode}"))
        else:
            data_dir = run_dir / str(meta["dataset_name"])
            md = self._load(data_dir / "metadata.json")
            batches = self._batch_files(data_dir, md) if md is not None else []
            if md is None:
                meta.update(verdict="error", error="the CLI exited 0 but no metadata.json was written")
            elif not batches:
                meta.update(verdict="error", error="metadata.json lists no parquet batch files")
            else:
                meta["records"] = _num(md.get("actual_num_records"))   # never invented: only when metadata says so
                meta["batches"] = len(batches)
                kept = await self._export_batches(run_dir, batches, target, dest, str(meta["format"]))
                if kept is None:
                    meta.update(verdict="failed",
                                error="converting the parquet batches failed (partial copies may remain)")
                else:
                    meta["verdict"] = "succeeded"
        await asyncio.to_thread(self._write_meta, run_dir, meta)
        await asyncio.to_thread(self._prune)

    @staticmethod
    def _batch_files(data_dir: Path, md: dict[str, Any]) -> list[Path]:
        """metadata.json → file_paths["parquet-files"], each a contained regular file under data_dir (no symlinks)."""
        out: list[Path] = []
        base = data_dir.resolve()
        for rel in _list(_dict(md.get("file_paths")).get("parquet-files"))[:64]:
            if not isinstance(rel, str) or not rel or rel.startswith("/"):
                continue
            p = (data_dir / rel).resolve()
            if p.is_relative_to(base) and not p.is_symlink() and p.is_file():
                out.append(p)
        return out

    async def _export_batches(self, run_dir: Path, batches: list[Path], target: Path | None, dest: Path,
                              fmt: str) -> int | None:
        """records.jsonl in the run dir (capped at MAX_PREVIEW_RECORDS lines) + the user's copy under dest.
        Returns the number of records kept in records.jsonl, or None when a conversion failed (partial copies
        stay on disk: failure is data, nothing is invented)."""
        try:
            dest.mkdir(mode=0o700, parents=True, exist_ok=True)
        except OSError:
            return None
        records_path = run_dir / "records.jsonl"
        kept = 0
        wrote_any = False                            # json array framing across all batches
        for i, batch in enumerate(batches):
            if target is None:                       # parquet: the user copy IS the batch files
                with contextlib.suppress(OSError):
                    shutil.copy2(batch, dest / batch.name, follow_symlinks=False)
            if kept >= MAX_PREVIEW_RECORDS:
                continue                             # records.jsonl is full; finish copying the parquet batches
            ext = "jsonl"                              # the interchange is ALWAYS jsonl: records.jsonl and the
            # json user copy are framed from jsonl lines; a csv user copy gets its own conversion below
            # (the exporter's .json output shape is never parsed here).
            tmp = run_dir / f".batch.{i}.{os.urandom(6).hex()}.{ext}"
            user_tmp: Path | None = None
            try:
                r = await self._run([EXPORTER, str(batch), str(tmp)], timeout=EXPORT_TIMEOUT)
                if r.returncode != 0:
                    return None
                kept = self._append_capped(tmp, records_path, kept)
                if target is not None:
                    if fmt == "csv":
                        user_tmp = run_dir / f".batch.{i}.{os.urandom(6).hex()}.csv"
                        rc = await self._run([EXPORTER, str(batch), str(user_tmp)], timeout=EXPORT_TIMEOUT)
                        if rc.returncode != 0:
                            return None
                        wrote_any = self._append_user_copy(user_tmp, target, fmt, first_batch=(i == 0),
                                                           wrote_any=wrote_any)
                    else:
                        wrote_any = self._append_user_copy(tmp, target, fmt, first_batch=(i == 0),
                                                           wrote_any=wrote_any)
            except OSError:
                return None
            finally:
                for t in (tmp, user_tmp):
                    if t is not None:
                        with contextlib.suppress(OSError):
                            t.unlink(missing_ok=True)
        if target is not None:
            try:
                if fmt == "json":
                    with target.open("ab") as out:
                        out.write(b"\n]\n" if wrote_any else b"]\n")
                    if not wrote_any:
                        target.write_bytes(b"[]\n")  # no records at all: a valid empty array, not a dangling "["
                target.chmod(0o600)
            except OSError:
                return None
        with contextlib.suppress(OSError):
            records_path.chmod(0o600)
        return kept

    @staticmethod
    def _append_capped(src: Path, dst: Path, kept: int) -> int:
        """Copy lines from src onto records.jsonl until MAX_PREVIEW_RECORDS lines are kept in total."""
        with src.open("rb") as f, dst.open("ab") as out:
            for line in f:
                if kept >= MAX_PREVIEW_RECORDS:
                    break
                out.write(line)
                if line.endswith(b"\n"):
                    kept += 1
        return kept

    @staticmethod
    def _append_user_copy(src: Path, target: Path, fmt: str, *, first_batch: bool, wrote_any: bool) -> bool:
        """Append one converted batch to the user's copy. csv skips repeated headers; jsonl appends lines; json
        streams object lines that the caller frames into one array. Returns the new wrote_any."""
        mode = "wb" if first_batch else "ab"
        with src.open("rb") as f, target.open(mode) as out:
            if fmt == "json" and first_batch:
                out.write(b"[\n")
            skip_header = fmt == "csv" and not first_batch
            for i, line in enumerate(f):
                if skip_header and i == 0:
                    continue
                if fmt == "json":
                    stripped = line.strip()
                    if not stripped:
                        continue
                    if wrote_any:
                        out.write(b",\n")
                    out.write(stripped)
                    wrote_any = True
                else:
                    out.write(line)
        return wrote_any

    # ── API support (called by api/routers/observe.py) ──────────────────────────────────────
    def records_page(self, run_id: str, limit: int, offset: int) -> dict[str, Any]:
        """Rows for GET .../runs/{run_id}/records. Reads records.jsonl ONLY (never parquet). KeyError for an
        unknown run. `total` comes from run.json's `records` when it has one, else the counted lines."""
        if RUN_ID_RE.fullmatch(run_id) is None:
            raise KeyError(run_id)
        run_dir = self._runs_dir / run_id
        meta = self._load_meta(run_dir)
        if meta is None:
            raise KeyError(run_id)
        rows, columns = self._read_records(run_dir / "records.jsonl")
        total = _num(meta.get("records"))
        page = rows[max(0, offset):max(0, offset) + max(0, limit)]
        out_rows: list[dict[str, Any]] = []
        truncated = False
        for row in page:
            capped = {k: DataDesignerAdapter._cap_value(v) for k, v in row.items()}
            if any(isinstance(v, str) and len(v) >= CELL_CAP for v in capped.values()):
                truncated = True
            out_rows.append(capped)
        return {"run_id": run_id, "total": int(total) if total is not None else len(rows),
                "columns": columns, "rows": out_rows, "truncated": truncated}

    def run_files(self, run_id: str) -> dict[str, Any]:
        """Rel paths + sizes of every regular file in the run dir (for the UI's download links; nothing is served).
        KeyError for an unknown run."""
        if RUN_ID_RE.fullmatch(run_id) is None:
            raise KeyError(run_id)
        run_dir = self._runs_dir / run_id
        if self._load_meta(run_dir) is None:
            raise KeyError(run_id)
        files: list[dict[str, Any]] = []
        with contextlib.suppress(OSError):
            for root, dirs, names in os.walk(run_dir, followlinks=False):
                dirs[:] = [d for d in dirs if not (Path(root) / d).is_symlink()][:64]
                for name in sorted(names)[:128]:
                    p = Path(root) / name
                    if p.is_symlink() or not p.is_file():
                        continue
                    rel = p.relative_to(run_dir).as_posix()
                    if rel == "records.jsonl" or len(files) < MAX_FILES:
                        files.append({"path": rel, "bytes": p.stat().st_size})
        return {"run_id": run_id, "files": files[:MAX_FILES]}

    # ── report ──────────────────────────────────────────────────────────────────────────────
    def _report_text(self, run_id: str) -> tuple[bool, str]:
        if run_id:
            if RUN_ID_RE.fullmatch(run_id) is None:
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
        lines = [self._event(meta).summary, ""]
        if meta.get("verdict") == "running":
            mins = max(0.0, (time.time() - (_num(meta.get("ts")) or time.time())) / 60)
            lines += [f"Running for {mins:.0f} min. This report fills in when the run ends.", ""]
        if meta.get("verdict") in ("error", "failed"):
            lines += ["Data Designer did not complete the run:", str(meta.get("error", "")), ""]
        if meta.get("verdict") == "interrupted":
            lines += ["Interrupted: the console stopped or restarted while it ran.", ""]
        mode = str(meta.get("mode"))
        if mode == "preview" and isinstance(meta.get("preview_dir"), str):
            browser = run_dir / meta["preview_dir"] / "sample_records" / "sample_records_browser.html"
            if browser.is_file() and not browser.is_symlink():
                lines.append(f"Per-record browser (HTML, open it locally): {browser}")
        if mode == "create" and self._datasets_root is not None:
            dest = self._datasets_root / str(meta.get("destination") or "")
            if meta.get("format") == "parquet":
                lines.append(f"Dataset copy (parquet batches): {dest}")
            else:
                lines.append(f"Dataset copy: {dest / (str(meta.get('dataset_name') or 'dataset') + '.'
                                                   + str(meta.get('format')))}")
        rows, columns = self._read_records(run_dir / "records.jsonl")
        if rows:
            show = columns[:8] or ["?"]
            lines += ["", "First records (at most 5, cells truncated at 120 chars):", "",
                      "| " + " | ".join(show) + " |", "| " + " | ".join("---" for _ in show) + " |"]
            for row in rows[:MAX_REPORT_ROWS]:
                cells = [redact(str(row.get(c, "")))[:REPORT_CELL].replace("|", "\\|") or " " for c in show]
                lines.append("| " + " | ".join(cells) + " |")
            lines += ["", f"records.jsonl: {run_dir / 'records.jsonl'} (capped at {MAX_PREVIEW_RECORDS} records)"]
        return True, "\n".join(lines)


def redact_meta(meta: dict[str, Any]) -> dict[str, Any]:
    """run.json holds metadata only; error text is redacted like every other output."""
    out = dict(meta)
    if isinstance(out.get("error"), str):
        out["error"] = redact(out["error"])
    return out
