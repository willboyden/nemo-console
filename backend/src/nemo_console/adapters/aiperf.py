"""AIPerf adapter: NVIDIA AIPerf (a load / latency benchmark CLI, not a service) as a managed system.

Facts this adapter is built on (contract 9.8, 2026-10-24; evidence in tests/aiperf/fixtures, captured LIVE the same day
against aitop/nemotron-lightning through the lab router):
  * `aiperf-local` is the lab wrapper (clients/aiperf/install.sh): AIPerf 0.13.0 pinned to the scanned commit 794f8bb.
    `--version` prints "0.13.0" [fixture version.txt]. The wrapper owns the target (router, loopback only), the router
    key (its own 0600 file, handed to AIPerf by environment variable) and the no-internet policy (builtin tokenizer read
    from clients/aiperf/.tiktoken-cache, HF libraries offline). The console passes NO env and never sees a key.
  * `aiperf-local profile --model M --endpoint-type chat --streaming --concurrency C --request-count N
    --prompt-input-tokens-mean I --prompt-output-tokens-mean O ... --artifact-dir DIR` writes DIR/profile_export_aiperf.json
    [fixture profile_export_aiperf.json]: top-level metric objects `{unit, avg, p1..p99, min, max, std, count, sum}`
    (time_to_first_token, inter_token_latency, request_latency in ms) and scalar-throughput objects `{unit, avg}`
    (output_token_throughput tokens/s, request_throughput requests/s, request_error_rate %), plus was_cancelled,
    is_complete, error_summary, start_time/end_time and an input_config whose api_key is already "<redacted>".
  * A run that completes no request exits 1 with an error panel and writes no summary [fixture all-failed.txt].
  * profiles.yaml (clients/aiperf/profiles.yaml) is this console's own schema: validated here, authoritative.
"""
from __future__ import annotations

import asyncio
import contextlib
import json
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
    Severity,
    SystemEvent,
    SystemStatus,
    ValidationResult,
)
from nemo_console.services.common import redact, sha256_hex

LAUNCHER = "aiperf-local"
PROFILE_TIMEOUT = 3600.0                  # = runner.AIPERF_PROFILE; the runner caps `aiperf-local profile` at this
PROFILE_MAX_BYTES = 1_048_576             # AIPerf prints one results table plus INFO lines (~30 KiB for a smoke run)
VERSION_TTL = 300.0
KEEP_RUNS = 100
EVENT_KIND = "aiperf.run"
VERDICTS = ("succeeded", "failed", "error", "interrupted")
SUMMARY_FILE = "profile_export_aiperf.json"
RECORDS_FILE = "profile_export.jsonl"          # per-request records (--export-level records): the only place errors say why
MAX_RECORD_LINES = 500
MAX_JSON_BYTES = 8 * 1024 * 1024
MAX_PROFILES_BYTES = 256 * 1024
MAX_PROFILES = 32
PROFILES_ID = "profiles"
RUN_ID_RE = re.compile(r"\d{8}T\d{6}-\d{3}-[a-z0-9][a-z0-9-]{0,39}")
PRESET_RE = re.compile(r"[a-z0-9][a-z0-9-]{0,39}")
MODEL_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:/@+\-]{0,127}")
ENDPOINT_TYPES = ("chat", "completions")
LAB_INSTALLER = Path("clients") / "aiperf" / "install.sh"
LAB_MINT_KEY = Path("clients") / "aiperf" / "mint-key.sh"
PINNED_VERSION = "0.13.0"
Done = Callable[..., ActionResult]          # run_action's `done(ok, output, problems=None)`
ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
BOX_RE = re.compile(r"[│╭╮╰╯─┃┏┓┗┛━┡┩╇┳┻╋]+")

# profiles.yaml schema: key -> (type label, min, max). Integer keys only; the rest are checked by hand below.
INT_KEYS: dict[str, tuple[int, int, bool]] = {          # key -> (min, max, required)
    "concurrency": (1, 64, True),
    "requests": (1, 2000, True),
    "isl": (1, 32768, True),
    "osl": (1, 4096, True),
    "warmup_requests": (0, 100, False),
    "timeout_s": (1, 600, False),
    "seed": (0, 2_147_483_647, False),
}
PROFILE_KEYS = {"description", "model", "endpoint_type", "streaming", *INT_KEYS}


def _num(v: object) -> float | None:
    return float(v) if isinstance(v, int | float) and not isinstance(v, bool) else None


def _dict(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


@dataclass(frozen=True)
class Preset:
    name: str
    model: str
    endpoint_type: str
    streaming: bool
    concurrency: int
    requests: int
    isl: int
    osl: int
    warmup_requests: int
    timeout_s: int
    seed: int
    description: str


@dataclass
class _LiveRun:
    """The ONE background profile run this console owns. The task holds the only reference keeping it alive."""
    run_id: str
    preset: str
    started: float
    task: asyncio.Task[None]


# ── profiles.yaml: our own schema, authoritative ────────────────────────────────────────────────
def _line_map(text: str) -> dict[tuple[str, ...], int]:
    """(path...) -> 1-based line, for top-level keys, profile names and profile fields. Never raises."""
    out: dict[tuple[str, ...], int] = {}
    try:
        root = yaml.compose(text)
    except yaml.YAMLError:
        return out
    if not isinstance(root, yaml.MappingNode):
        return out
    for k, v in root.value:
        if isinstance(k, yaml.ScalarNode):
            out[(str(k.value),)] = k.start_mark.line + 1
            if k.value == "profiles" and isinstance(v, yaml.MappingNode):
                for pk, pv in v.value:
                    if not isinstance(pk, yaml.ScalarNode):
                        continue
                    out[("profiles", str(pk.value))] = pk.start_mark.line + 1
                    if isinstance(pv, yaml.MappingNode):
                        for fk, _fv in pv.value:
                            if isinstance(fk, yaml.ScalarNode):
                                out[("profiles", str(pk.value), str(fk.value))] = fk.start_mark.line + 1
    return out


def _close(word: str, options: set[str]) -> str:
    """A 'did you mean' hint: the option that shares the longest common prefix (>= 3 chars) with a typo."""
    best, best_n = "", 0
    for o in sorted(options):
        n = 0
        for a, b in zip(word, o, strict=False):
            if a != b:
                break
            n += 1
        if n > best_n:
            best, best_n = o, n
    return f" (did you mean '{best}'?)" if best_n >= 3 else ""


def parse_profiles(text: str) -> tuple[dict[str, Preset], list[Diagnostic]]:
    """Validate a profiles.yaml document. Returns (valid presets, diagnostics); a preset with an error is dropped.
    Never raises."""
    diags: list[Diagnostic] = []
    presets: dict[str, Preset] = {}
    if len(text.encode("utf-8", "replace")) > MAX_PROFILES_BYTES:
        return presets, [Diagnostic(severity="error", message=f"profiles.yaml is larger than {MAX_PROFILES_BYTES} bytes")]
    try:
        doc = yaml.safe_load(text)
    except yaml.YAMLError as e:
        mark = getattr(e, "problem_mark", None)
        return presets, [Diagnostic(severity="error", message=f"YAML syntax: {str(getattr(e, 'problem', e))[:200]}",
                                    line=(mark.line + 1) if mark is not None else None)]
    lines = _line_map(text)

    def err(msg: str, *path: str, severity: Severity = "error") -> None:
        diags.append(Diagnostic(severity=severity, message=msg, line=lines.get(path)))

    if not isinstance(doc, dict):
        return presets, [Diagnostic(severity="error", message="the document must be a mapping with 'version' and 'profiles'")]
    for k in doc:
        if k not in ("version", "profiles"):
            err(f"unknown top-level key {k!r}{_close(str(k), {'version', 'profiles'})}", str(k))
    version = doc.get("version")
    if version != 1 or isinstance(version, bool):
        err("'version' must be 1", "version")
    profiles = doc.get("profiles")
    if not isinstance(profiles, dict) or not profiles:
        err("'profiles' must be a mapping with at least one preset", "profiles")
        return presets, diags
    if len(profiles) > MAX_PROFILES:
        err(f"at most {MAX_PROFILES} presets are supported (found {len(profiles)})", "profiles")
    for name, body in list(profiles.items())[:MAX_PROFILES]:
        pname = str(name)
        if not isinstance(name, str) or PRESET_RE.fullmatch(name) is None:
            err(f"preset name {pname!r} must match [a-z0-9][a-z0-9-]{{0,39}}", "profiles", pname)
            continue
        if not isinstance(body, dict):
            err(f"preset {name!r} must be a mapping", "profiles", name)
            continue
        before = len([d for d in diags if d.severity == "error"])
        for k in body:
            if k not in PROFILE_KEYS:
                err(f"{name}: unknown key {k!r}{_close(str(k), PROFILE_KEYS)}", "profiles", name, str(k))
        model = body.get("model")
        if not isinstance(model, str) or MODEL_RE.fullmatch(model) is None:
            err(f"{name}: 'model' is required: a router model alias (letters, digits and . _ : / @ + -)",
                "profiles", name, "model")
        etype = body.get("endpoint_type", "chat")
        if etype not in ENDPOINT_TYPES:
            err(f"{name}: 'endpoint_type' must be one of {', '.join(ENDPOINT_TYPES)}", "profiles", name, "endpoint_type")
        streaming = body.get("streaming", True)
        if not isinstance(streaming, bool):
            err(f"{name}: 'streaming' must be true or false", "profiles", name, "streaming")
        desc = body.get("description", "")
        if not isinstance(desc, str) or len(desc) > 300 or "\n" in desc:
            err(f"{name}: 'description' must be a single line of at most 300 characters", "profiles", name, "description")
        ints: dict[str, int] = {}
        for key, (lo, hi, required) in INT_KEYS.items():
            if key not in body:
                if required:
                    err(f"{name}: '{key}' is required", "profiles", name)
                continue
            v = body[key]
            if isinstance(v, bool) or not isinstance(v, int) or not lo <= v <= hi:
                err(f"{name}: '{key}' must be an integer from {lo} to {hi}", "profiles", name, key)
            else:
                ints[key] = v
        if len([d for d in diags if d.severity == "error"]) > before:
            continue
        if streaming is False:
            err(f"{name}: streaming is off, so time to first token and inter-token latency are not measured",
                "profiles", name, "streaming", severity="warn")
        if ints["requests"] < ints["concurrency"]:
            err(f"{name}: fewer requests than the concurrency, so the target concurrency is never reached",
                "profiles", name, "requests", severity="warn")
        presets[name] = Preset(
            name=name, model=str(model), endpoint_type=str(etype), streaming=bool(streaming),
            concurrency=ints["concurrency"], requests=ints["requests"], isl=ints["isl"], osl=ints["osl"],
            warmup_requests=ints.get("warmup_requests", 0), timeout_s=ints.get("timeout_s", 120),
            seed=ints.get("seed", 42), description=str(desc))
    return presets, diags


def render_argv(launcher: str, p: Preset, artifact_dir: Path) -> list[str]:
    """The exact aiperf-local argv for a preset. The wrapper adds the URL, the credential, the tokenizer and the
    offline/telemetry-off policy; nothing secret is, or can be, in this list."""
    argv = [launcher, "profile", "--model", p.model, "--endpoint-type", p.endpoint_type]
    if p.streaming:
        argv.append("--streaming")
    argv += ["--concurrency", str(p.concurrency), "--request-count", str(p.requests),
             "--prompt-input-tokens-mean", str(p.isl), "--prompt-output-tokens-mean", str(p.osl)]
    if p.warmup_requests > 0:
        argv += ["--warmup-request-count", str(p.warmup_requests)]
    argv += ["--request-timeout-seconds", str(p.timeout_s), "--random-seed", str(p.seed),
             "--export-level", "records", "--use-server-token-count", "--artifact-dir", str(artifact_dir)]
    return argv


# ── artifact parsing ────────────────────────────────────────────────────────────────────────────
def summarize(doc: dict[str, Any]) -> dict[str, float]:
    """The few numbers the console keeps from profile_export_aiperf.json. Missing keys are simply absent."""
    out: dict[str, float] = {}

    def put(key: str, section: str, field: str) -> None:
        v = _num(_dict(doc.get(section)).get(field))
        if v is not None:
            out[key] = v

    for q in ("p50", "p99"):
        put(f"ttft_{q}_ms", "time_to_first_token", q)
        put(f"itl_{q}_ms", "inter_token_latency", q)
        put(f"latency_{q}_ms", "request_latency", q)
    put("output_tps", "output_token_throughput", "avg")
    put("request_tps", "request_throughput", "avg")
    put("requests", "request_count", "avg")
    put("error_rate_pct", "request_error_rate", "avg")
    put("duration_s", "benchmark_duration", "avg")
    put("isl_avg", "input_sequence_length", "avg")
    put("osl_avg", "output_sequence_length", "avg")
    return out


LOG_LINE_RE = re.compile(r"^\d\d:\d\d:\d\d\.\d+ (?:INFO|WARNING|NOTICE|DEBUG)\b")


def _clean_error(text: str) -> str:
    """The failure text of a run (fallback when no per-request error was recorded): ANSI and box-drawing removed,
    routine log lines dropped, the error panel kept, redacted."""
    plain = BOX_RE.sub(" ", ANSI_RE.sub("", text))
    lines = [" ".join(line.split()) for line in plain.splitlines()]
    lines = [line for line in lines if line and LOG_LINE_RE.match(line) is None]
    keep = [line for line in lines if re.search(r"error|reason|failed|refused|missing|not answering|prefetch", line, re.IGNORECASE)]
    return redact(" | ".join((keep or lines)[-8:]))[:1500]


def request_errors(path: Path) -> tuple[int, str]:
    """(failed requests seen, the first failure as one line) from profile_export.jsonl. Never raises; ("0", "") when
    the file is absent or has no error records. The message is the router's own JSON error text, redacted."""
    count, first = 0, ""
    try:
        if path.is_symlink() or not path.is_file():
            return 0, ""
        with path.open("rb") as fh:
            for n, line in enumerate(fh):
                if n >= MAX_RECORD_LINES:
                    break
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                err = _dict(_dict(rec).get("error"))
                if not err:
                    continue
                count += 1
                if not first:
                    msg = str(err.get("message") or "")
                    with contextlib.suppress(ValueError):
                        inner = _dict(_dict(json.loads(msg)).get("error"))
                        if isinstance(inner.get("message"), str):
                            msg = inner["message"]
                    first = redact(f"HTTP {err.get('code', '?')} {err.get('type', '')}: {msg}".strip())[:400]
    except OSError:
        return count, first
    return count, first


class AiPerfAdapter(SystemAdapter):
    id = "aiperf"
    label = "AIPerf"

    # ctx.paths: aiperf_dir (clients/aiperf; the tokenizer cache is checked under it), aiperf_profiles (profiles.yaml),
    # aiperf_key_file (EXISTENCE only), repo (install-fix pointers only). ctx.urls: "litellm" (the load target; only its
    # liveness is probed here, the wrapper talks to it).
    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self._version_cache: tuple[float, str | None, str | None] = (0.0, None, None)   # (ts, launcher, version)
        self.follow_interval = 2.0
        self._live: _LiveRun | None = None

    # ── helpers ─────────────────────────────────────────────────────────────────────────────
    @property
    def _litellm(self) -> str:
        return self.ctx.urls.get("litellm", "http://127.0.0.1:4000").rstrip("/")

    @property
    def _profiles_path(self) -> Path | None:
        return self.ctx.paths.get("aiperf_profiles")

    @property
    def _key_file(self) -> Path | None:
        return self.ctx.paths.get("aiperf_key_file")

    @property
    def _runs_dir(self) -> Path:
        return self.ctx.state_dir / "aiperf" / "runs"

    def _lab_file(self, rel: Path) -> Path | None:
        repo = self.ctx.paths.get("repo")
        return repo / rel if repo is not None and (repo / rel).exists() else None

    def _install_fix(self) -> str:
        lab = self._lab_file(LAB_INSTALLER)
        return (f"{lab}   # pinned AIPerf {PINNED_VERSION} + the aiperf-local wrapper + the tokenizer prefetch" if lab else
                f"run clients/aiperf/install.sh in the lab repository (pinned AIPerf {PINNED_VERSION}; it links the "
                "aiperf-local wrapper onto PATH)")

    def _key_fix(self) -> str:
        lab = self._lab_file(LAB_MINT_KEY)
        return (f"{lab}   # mints AIPerf's own router key (key_alias 'aiperf')" if lab else
                "run clients/aiperf/mint-key.sh in the lab repository (it writes ~/.config/aiperf/litellm.key, 0600)")

    def _prefetch_fix(self) -> str:
        aiperf_dir = self.ctx.paths.get("aiperf_dir")
        return (f"{aiperf_dir / LAUNCHER} prefetch   # one-time download of the tokenizer vocabulary (the only step "
                "with egress)" if aiperf_dir is not None else "aiperf-local prefetch   # one-time tokenizer download")

    def _has_tokenizer(self) -> bool:
        aiperf_dir = self.ctx.paths.get("aiperf_dir")
        if aiperf_dir is None:
            return True                      # no path configured: do not guess, the wrapper checks itself
        cache = aiperf_dir / ".tiktoken-cache"
        try:
            return cache.is_dir() and any(p.is_file() for p in cache.iterdir())
        except OSError:
            return False

    async def _run(self, argv: list[str], *, timeout: float, max_bytes: int = 256_000,
                   cwd: Path | None = None) -> CommandResult:
        try:
            return await self.ctx.runner.run(argv, timeout=timeout, max_bytes=max_bytes, cwd=cwd)
        except Exception as e:  # noqa: BLE001 - not allowlisted / not on PATH: data, not an exception
            return CommandResult(returncode=127, stdout="", stderr=f"{type(e).__name__}: {e}"[:300])

    async def _launcher(self) -> tuple[str | None, str | None]:
        ts, launcher, version = self._version_cache
        if time.monotonic() - ts < VERSION_TTL and launcher is not None:
            return launcher, version
        launcher, version = None, None
        r = await self._run([LAUNCHER, "--version"], timeout=20.0, max_bytes=4096)
        if r.returncode == 0:
            first = ANSI_RE.sub("", r.stdout).strip().splitlines()[:1]
            launcher, version = LAUNCHER, (first[0][:40] if first else None)
        self._version_cache = (time.monotonic(), launcher, version)
        return launcher, version

    async def _router_up(self) -> tuple[bool, str]:
        try:
            resp = await self.ctx.http.get(f"{self._litellm}/health/liveliness", timeout=3.0, follow_redirects=False)
        except Exception as e:  # noqa: BLE001
            return False, type(e).__name__
        return resp.status_code < 500, f"HTTP {resp.status_code}"

    def _live_run_id(self) -> str | None:
        live = self._live
        return live.run_id if live is not None and not live.task.done() else None

    def _read_profiles(self) -> tuple[dict[str, Preset], list[Diagnostic], bool]:
        """(presets, diagnostics, file_found). A missing or unreadable file is zero presets, never an exception."""
        path = self._profiles_path
        if path is None or path.is_symlink() or not path.is_file():
            return {}, [], False
        try:
            if path.stat().st_size > MAX_PROFILES_BYTES:
                return {}, [Diagnostic(severity="error", message="profiles.yaml is too large")], True
            text = path.read_text()
        except (OSError, UnicodeDecodeError):
            return {}, [Diagnostic(severity="error", message="profiles.yaml cannot be read")], True
        presets, diags = parse_profiles(text)
        return presets, diags, True

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
    def _headline(meta: dict[str, Any]) -> str:
        parts = [str(meta.get("preset", "?")), str(meta.get("model", "?"))]
        if _num(meta.get("concurrency")) is not None:
            parts.append(f"c={_num(meta.get('concurrency')):.0f}")
        parts.append(str(meta.get("verdict", "?")))
        m = _dict(meta.get("metrics"))
        if _num(m.get("ttft_p50_ms")) is not None:
            parts.append(f"TTFT p50 {_num(m.get('ttft_p50_ms')):.0f} ms")
        if _num(m.get("output_tps")) is not None:
            parts.append(f"{_num(m.get('output_tps')):.0f} output tok/s")
        if meta.get("verdict") == "running":
            mins = max(0.0, (time.time() - (_num(meta.get("ts")) or time.time())) / 60)
            parts.append(f"running for {mins:.0f} min")
        return " · ".join(parts)[:300]

    def _event(self, meta: dict[str, Any]) -> SystemEvent:
        m = _dict(meta.get("metrics"))
        data: dict[str, Any] = {k: meta.get(k) for k in ("run_id", "preset", "model", "verdict", "concurrency",
                                                         "requests", "isl", "osl", "duration_ms")}
        for k in ("ttft_p50_ms", "ttft_p99_ms", "itl_p50_ms", "output_tps", "request_tps", "error_rate_pct"):
            if _num(m.get(k)) is not None:
                data[k] = _num(m.get(k))
        return SystemEvent(ts=_num(meta.get("ts")) or 0.0, system="aiperf", kind=EVENT_KIND,
                           summary=self._headline(meta), data=data)

    def _run_dirs(self) -> list[Path]:
        try:
            return sorted((p for p in self._runs_dir.iterdir() if RUN_ID_RE.fullmatch(p.name) and p.is_dir()),
                          key=lambda p: p.name, reverse=True)
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

    def _new_run_dir(self, preset: str) -> tuple[str, Path]:
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{int(time.time() * 1000) % 1000:03d}"
        run_id = f"{stamp}-{preset}"
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
    def _load_json(p: Path) -> dict[str, Any] | None:
        try:
            if p.is_symlink() or p.stat().st_size > MAX_JSON_BYTES:
                return None
            d = json.loads(p.read_text())
        except (OSError, ValueError):
            return None
        return d if isinstance(d, dict) else None

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    async def status(self) -> SystemStatus:
        (launcher, version), (router_ok, router_detail) = await asyncio.gather(self._launcher(), self._router_up())
        presets, diags, found = await asyncio.to_thread(self._read_profiles)
        last = await asyncio.to_thread(self._metas, 1)
        has_tok = await asyncio.to_thread(self._has_tokenizer)
        key_ok = self._key_file is None or self._key_file.exists()
        problems: list[Problem] = []
        components = [
            Component(kind="process", name=launcher or LAUNCHER, state="running" if launcher else "absent",
                      detail=(f"{launcher} {version or ''}: router-only, loopback, its own key file, offline tokenizer; "
                              "the console passes no environment and never sees a key") if launcher else
                      "aiperf-local is not on PATH (installed by clients/aiperf/install.sh)"),
            Component(kind="http", name="router " + self._litellm, state="running" if router_ok else "stopped",
                      detail=f"{router_detail} on GET /health/liveliness (the load target)"),
        ]
        live = self._live if self._live_run_id() else None
        if live is not None:
            mins = max(0.0, (time.time() - live.started) / 60)
            components.append(Component(kind="process", name=f"profile {live.preset}", state="running",
                                        detail=f"load run {live.run_id}, running for {mins:.0f} min (one at a time)"))
        if launcher is None:
            problems.append(Problem(severity="error", code="ap.not_installed",
                                    message="AIPerf is not installed (or aiperf-local is not on the console's PATH).",
                                    fix=self._install_fix()))
        if not router_ok:
            problems.append(Problem(
                severity="warn", code="ap.router_down",
                message=f"The router ({self._litellm}) is not answering; profile runs will fail.",
                fix=f"curl -s -o /dev/null -w '%{{http_code}}\\n' {self._litellm}/health/liveliness"))
        if launcher is not None and not key_ok:
            problems.append(Problem(severity="warn", code="ap.no_key",
                                    message=f"AIPerf's router key file {self._key_file} is absent; profile runs will "
                                            "be refused.", fix=self._key_fix()))
        if launcher is not None and not has_tok:
            problems.append(Problem(severity="warn", code="ap.no_tokenizer",
                                    message="The offline tokenizer cache (clients/aiperf/.tiktoken-cache) is empty; "
                                            "profile runs will be refused.", fix=self._prefetch_fix()))
        if not found:
            problems.append(Problem(severity="warn", code="ap.no_profiles",
                                    message=f"No profiles file at {self._profiles_path}: there is no preset to run.",
                                    fix="restore clients/aiperf/profiles.yaml from the lab repository (git checkout "
                                        "-- clients/aiperf/profiles.yaml)"))
        bad = [d for d in diags if d.severity == "error"]
        if found and bad:
            first = bad[0]
            problems.append(Problem(severity="warn", code="ap.profiles_invalid",
                                    message=f"profiles.yaml has {len(bad)} error(s); first: {first.message}"
                                            + (f" (line {first.line})" if first.line else ""),
                                    fix="fix it on the Configure tab (the validator names each line) or restore "
                                        "clients/aiperf/profiles.yaml with git"))
        health = "down" if launcher is None else ("degraded" if not (router_ok and key_ok and has_tok) else "up")
        summary = f"{len(presets)} preset(s)"
        if last:
            summary += f" · last run: {self._headline(last[0])}"
        return SystemStatus(
            system="aiperf", health=health, version=version, summary=summary[:300],
            endpoints=[Endpoint(name="router (load target)", url=self._litellm, role="api", auth="bearer")],
            components=components, problems=problems, checked_at=time.time())

    async def metrics(self) -> MetricsSnapshot:
        now = time.time()
        metas = await asyncio.to_thread(self._metas, KEEP_RUNS)
        points = [MetricPoint(key="ap.runs", value=float(len(metas)), unit="1")]
        if metas:
            points.append(MetricPoint(key="ap.live_running", value=1.0 if self._live_run_id() else 0.0, unit="1"))
            for verdict in VERDICTS:
                points.append(MetricPoint(key="ap.runs_by_verdict", value=float(
                    [m.get("verdict") for m in metas].count(verdict)), unit="1", labels={"verdict": verdict}))
        newest = next((m for m in metas if m.get("verdict") == "succeeded" and _dict(m.get("metrics"))), None)
        if newest is not None:
            m = _dict(newest.get("metrics"))
            for key, name, unit in (("ttft", "ap.ttft_ms", "ms"), ("itl", "ap.itl_ms", "ms"),
                                    ("latency", "ap.latency_ms", "ms")):
                for q in ("p50", "p99"):
                    v = _num(m.get(f"{key}_{q}_ms"))
                    if v is not None:
                        points.append(MetricPoint(key=name, value=v, unit=unit, labels={"q": q}))
            for mk, name, unit in (("output_tps", "ap.output_tps", "tok/s"), ("request_tps", "ap.request_tps", "1/s"),
                                   ("error_rate_pct", "ap.error_rate", "%")):
                v = _num(m.get(mk))
                if v is not None:
                    points.append(MetricPoint(key=name, value=v, unit=unit))
        return MetricsSnapshot(system="aiperf", ts=now, available=True, points=points)

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

    # ── configure: profiles.yaml (the core's CAS/backup/revision machinery) ──────────────────
    def config_files(self) -> list[ConfigFileSpec]:
        path = self._profiles_path
        return [ConfigFileSpec(
            id=PROFILES_ID, label="Load profiles", display_path="clients/aiperf/profiles.yaml" if path else PROFILES_ID,
            description="Named AIPerf load presets: model alias, endpoint type, streaming, concurrency, request count "
                        "and input/output lengths. Each preset drives a live engine through the router, so keep "
                        "loads small.",
            format="yaml", editable=True,
            validators=["nemo-console aiperf profiles schema"], authoritative=True,
            apply_effect="takes effect on the next profile run; nothing to restart")]

    def config_path(self, file_id: str) -> Path:
        path = self._profiles_path
        if file_id != PROFILES_ID or path is None:
            raise KeyError(file_id)
        if path.is_symlink():
            raise KeyError(file_id)
        return path

    async def read_config(self, file_id: str) -> ConfigFile:
        p = self.config_path(file_id)
        try:
            if p.stat().st_size > MAX_PROFILES_BYTES:
                raise FileNotFoundError(file_id)
            content = p.read_text()
            mtime = p.stat().st_mtime
        except OSError:
            raise FileNotFoundError(file_id) from None
        return ConfigFile(spec=self.config_files()[0], content=content, sha256=sha256_hex(content.encode()), mtime=mtime)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        self.config_path(file_id)
        _, diags = parse_profiles(content)
        return ValidationResult(
            ok=not any(d.severity == "error" for d in diags), validator="nemo-console aiperf profiles schema",
            authoritative=True, diagnostics=diags,
            note="This is the console's own schema, which is the only thing that reads the file (aiperf-local gets "
                 "plain flags rendered from it). Model aliases are not checked against the router.")

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        self.config_path(file_id)
        return ApplyResult(ok=True, post_check="healthy",
                           detail="nothing to restart: presets are read by the next profile run")

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    def actions(self) -> list[ActionSpec]:
        presets, _, _ = self._read_profiles()
        names = sorted(presets)
        return [
            ActionSpec(
                id="profile", label="Run a load profile", danger="disruptive",
                description="Runs `aiperf-local profile` for a preset in the background (ONE at a time): synthetic "
                            "requests through the router at the preset's concurrency, measuring time to first token, "
                            "inter-token latency and throughput. disruptive: it loads a live engine and spends "
                            "router capacity. Results land in the console's state dir (newest 100 runs); "
                            "'Show a stored run' reports them.",
                params=[ParamSpec(key="preset", label="Preset", type="enum", choices=names or None,
                                  default=names[0] if names else None, requires_restart=False,
                                  help="A preset from the Load profiles config file.")]),
            ActionSpec(
                id="report", label="Show a stored run", danger="safe",
                description="The report of a stored run (newest first): verdict, the load shape and the latency and "
                            "throughput numbers.",
                params=[ParamSpec(key="run_id", label="Run", type="string", requires_restart=False,
                                  help="A run id from the runs list; empty = the newest run.")]),
        ]

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        t0 = time.monotonic()

        def done(ok: bool, out: str, problems: list[Problem] | None = None) -> ActionResult:
            return ActionResult(ok=ok, output=redact(out)[:20_000], duration_ms=int((time.monotonic() - t0) * 1000),
                                problems=problems or [])

        if action_id == "report":
            return done(*await asyncio.to_thread(self._report_text, str(params.get("run_id") or "")))
        if action_id != "profile":
            raise KeyError(action_id)
        return await self._start_profile(params, done)

    def _problem(self, code: str, message: str, fix: str) -> list[Problem]:
        return [Problem(severity="error", code=code, message=message, fix=fix)]

    def _busy(self) -> str | None:
        return self._live_run_id()

    async def _start_profile(self, params: dict[str, object], done: Done) -> ActionResult:
        busy = self._busy()
        if busy is not None:
            return done(False, f"A load run is already running ({busy}); one runs at a time. It shows in the runs list.",
                        [Problem(severity="warn", code="ap.profile.busy", message=f"load run {busy} is still running",
                                 fix=f"'Show a stored run' with run id {busy} shows its state")])
        presets, diags, _ = await asyncio.to_thread(self._read_profiles)
        name = str(params.get("preset") or "")
        preset = presets.get(name)
        if preset is None:
            bad = [d.message for d in diags if d.severity == "error"]
            return done(False, f"unknown or invalid preset {name!r}" + (f" ({bad[0]})" if bad else ""))
        launcher, _ = await self._launcher()
        if launcher is None:
            return done(False, "AIPerf is not installed.", self._problem(
                "ap.not_installed", "aiperf-local is not on PATH.", self._install_fix()))
        if self._key_file is not None and not self._key_file.exists():
            return done(False, "AIPerf's router key file is missing.", self._problem(
                "ap.no_key", f"{self._key_file} is absent.", self._key_fix()))
        if not await asyncio.to_thread(self._has_tokenizer):
            return done(False, "The offline tokenizer cache is empty.", self._problem(
                "ap.no_tokenizer", "clients/aiperf/.tiktoken-cache is empty.", self._prefetch_fix()))
        router_ok, router_detail = await self._router_up()
        if not router_ok:
            return done(False, f"The router ({self._litellm}) is not answering ({router_detail}).", self._problem(
                "ap.router_down", "the router is down", f"curl -s {self._litellm}/health/liveliness"))
        busy = self._busy()                          # re-check: the probes above awaited
        if busy is not None:
            return done(False, f"A load run is already running ({busy}); one runs at a time.")
        # From here to create_task nothing awaits, so no second request can slip in between the check and the start.
        run_id, run_dir = self._new_run_dir(name)
        argv = render_argv(launcher, preset, run_dir / "artifacts")
        started = time.time()
        meta: dict[str, Any] = {"run_id": run_id, "preset": name, "mode": "profile", "ts": started,
                                "verdict": "running", "model": preset.model, "endpoint_type": preset.endpoint_type,
                                "streaming": preset.streaming, "concurrency": preset.concurrency,
                                "requests": preset.requests, "isl": preset.isl, "osl": preset.osl,
                                "warmup_requests": preset.warmup_requests, "launcher": launcher}
        self._write_meta(run_dir, meta)
        task = asyncio.create_task(self._profile_task(run_dir, argv, meta), name=f"aiperf-profile-{run_id}")
        self._live = _LiveRun(run_id=run_id, preset=name, started=started, task=task)
        return done(True, f"Started load run {run_id} ({name}: {preset.model}, concurrency {preset.concurrency}, "
                          f"{preset.requests} requests, {preset.isl} in / {preset.osl} out). It runs in the background: "
                          "watch the runs list, then 'Show a stored run' with this run id.")

    async def _profile_task(self, run_dir: Path, argv: list[str], meta: dict[str, Any]) -> None:
        t0 = time.monotonic()
        finishing = False
        try:
            r = await self._run(argv, timeout=PROFILE_TIMEOUT, max_bytes=PROFILE_MAX_BYTES, cwd=run_dir)
            finishing = True                # from here this task writes the outcome; never overwrite it
            await asyncio.to_thread(self._finish, run_dir, meta, r, int((time.monotonic() - t0) * 1000))
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

    def _finish(self, run_dir: Path, meta: dict[str, Any], r: CommandResult, ms: int) -> None:
        meta["duration_ms"] = ms
        doc = self._load_json(run_dir / "artifacts" / SUMMARY_FILE)
        if r.timed_out:
            meta.update(verdict="failed", error=f"timed out after {int(PROFILE_TIMEOUT)} s")
        elif r.returncode != 0:
            n_err, first = request_errors(run_dir / "artifacts" / RECORDS_FILE)
            if n_err:
                meta["failed_requests"] = n_err
            detail = (f"{n_err} request(s) failed; the first: {first}" if first else
                      _clean_error(r.stderr + "\n" + r.stdout))
            meta.update(verdict="failed", error=detail or f"exit {r.returncode}")
        elif doc is None:
            meta.update(verdict="error", error="aiperf-local exited 0 but wrote no readable "
                                               f"{SUMMARY_FILE}")
        else:
            meta["metrics"] = summarize(doc)
            n_err, first = request_errors(run_dir / "artifacts" / RECORDS_FILE)
            if n_err:
                meta.update(failed_requests=n_err, first_error=first)
            meta["aiperf_version"] = doc.get("aiperf_version") if isinstance(doc.get("aiperf_version"), str) else None
            if doc.get("was_cancelled") is True:
                meta.update(verdict="interrupted", interrupted_by="aiperf (cancelled)")
            elif doc.get("is_complete") is False:
                meta.update(verdict="failed", error="aiperf reported an incomplete run")
            elif not _dict(meta["metrics"]).get("requests"):
                meta.update(verdict="error", error="the summary records no completed request")
            else:
                meta["verdict"] = "succeeded"
        self._write_meta(run_dir, meta)
        self._prune()

    # ── report ──────────────────────────────────────────────────────────────────────────────
    def _report_text(self, run_id: str) -> tuple[bool, str]:
        if run_id:
            if RUN_ID_RE.fullmatch(run_id) is None:
                return False, "unknown run id"
            run_dir = self._runs_dir / run_id
        else:
            metas = self._metas(1)
            if not metas:
                return False, "no stored runs yet"
            run_dir = self._runs_dir / str(metas[0].get("run_id"))
        meta = self._load_meta(run_dir)
        if meta is None:
            return False, "unknown run id"
        lines = [self._headline(meta), ""]
        verdict = meta.get("verdict")
        if verdict == "running":
            mins = max(0.0, (time.time() - (_num(meta.get("ts")) or time.time())) / 60)
            lines += [f"Running for {mins:.0f} min. This report fills in when the run ends.", ""]
        if verdict in ("error", "failed"):
            lines += ["AIPerf did not complete the run:", str(meta.get("error", "")), ""]
        if verdict == "interrupted":
            lines += ["Interrupted: the run was cancelled or the console stopped or restarted while it ran.", ""]
        lines.append(f"Load: model {meta.get('model', '?')}, {meta.get('endpoint_type', '?')} endpoint, "
                     f"streaming {'on' if meta.get('streaming') else 'off'}, concurrency {meta.get('concurrency', '?')}, "
                     f"{meta.get('requests', '?')} requests (+{meta.get('warmup_requests', 0)} warmup), "
                     f"{meta.get('isl', '?')} input / {meta.get('osl', '?')} output tokens.")
        if verdict == "succeeded" and _num(meta.get("failed_requests")):
            lines.append(f"{_num(meta.get('failed_requests')):.0f} request(s) failed; the first: {meta.get('first_error', '?')}")
        m = _dict(meta.get("metrics"))
        if m:
            def cell(key: str, digits: int = 1) -> str:
                v = _num(m.get(key))
                return f"{v:.{digits}f}" if v is not None else "-"
            lines += ["", "| Metric | p50 | p99 |", "| --- | --- | --- |",
                      f"| Time to first output (ms) | {cell('ttft_p50_ms')} | {cell('ttft_p99_ms')} |",
                      f"| Inter-token latency (ms) | {cell('itl_p50_ms', 2)} | {cell('itl_p99_ms', 2)} |",
                      f"| Request latency (ms) | {cell('latency_p50_ms')} | {cell('latency_p99_ms')} |",
                      "",
                      (f"Output throughput: {cell('output_tps')} tokens/s · request throughput: "
                       f"{cell('request_tps', 2)} requests/s · error rate: {cell('error_rate_pct')} % · "
                       f"duration {cell('duration_s', 2)} s."),
                      "These numbers include the router hop (this measures the lab path, not the bare engine)."]
        artifacts = run_dir / "artifacts"
        if (artifacts / SUMMARY_FILE).is_file():
            lines += ["", f"Artifacts (JSON, CSV, console table, log): {artifacts}"]
        return True, "\n".join(lines)
