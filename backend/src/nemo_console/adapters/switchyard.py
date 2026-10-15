"""NeMo Switchyard adapter (switchyard-server 0.3.0).

Everything here was derived from measured output of the live container (fixtures under
tests/switchyard/fixtures): /health, /v1/models, /v1/stats, /metrics, routing.jsonl and
`switchyard-server --dry-run`. Process control goes through ctx.runner only; HTTP through ctx.http.

Config keys the core must provide (see ARCHITECTURE section 3/4):
  ctx.urls["switchyard"]            base URL, e.g. http://127.0.0.1:4010
  ctx.paths["switchyard_routes"]    absolute path of inference/switchyard/routes.toml
  ctx.paths["switchyard_dir"]       absolute path of inference/switchyard/ (compose project dir)
                                    (either one may be omitted; the other is derived from it)
  ctx.secret_env["switchyard"]      env-var NAME of the bearer the console sends (never read here)
"""
from __future__ import annotations

import asyncio
import hashlib
import ipaddress
import json
import re
import time
import tomllib
import urllib.parse
from collections.abc import AsyncIterator
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import httpx

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
    Health,
    MetricPoint,
    MetricsSnapshot,
    PlaygroundSpec,
    Problem,
    SystemEvent,
    SystemStatus,
    ValidationResult,
)

CONTAINER = "ai-lab-switchyard-switchyard-1"
COMPOSE_PROJECT = "ai-lab-switchyard"
COMPOSE_SERVICE = "switchyard"
FALLBACK_IMAGE = "ai-lab-switchyard:0.3.0"
LOG_PATH = "/var/log/switchyard/routing.jsonl"
MAX_CONFIG_BYTES = 256 * 1024
ROUTE_COUNT_WINDOW = 2000            # newest routing records used for sy.route.calls
DRY_RUN_VALIDATOR = "switchyard-server --dry-run"
SEMANTIC_VALIDATOR = "tomllib + route/target/client cross-check"

_LABEL_RE = re.compile(r'(\w+)="((?:[^"\\]|\\.)*)"')
_SAMPLE_RE = re.compile(r"^([A-Za-z_:][A-Za-z0-9_:]*)(?:\{(.*)\})?\s+(\S+)(?:\s+\S+)?$")
_LINE_RE = re.compile(r"line (\d+)")


# ── helpers ─────────────────────────────────────────────────────────────────────────────────
def parse_prometheus(text: str) -> list[tuple[str, dict[str, str], float]]:
    """Parse the Prometheus text format; tolerant: unparseable lines are skipped."""
    out: list[tuple[str, dict[str, str], float]] = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        m = _SAMPLE_RE.match(line)
        if not m:
            continue
        try:
            value = float(m.group(3))
        except ValueError:
            continue
        labels = {k: v for k, v in _LABEL_RE.findall(m.group(2) or "")}
        out.append((m.group(1), labels, value))
    return out


def histogram_quantile(q: float, buckets: dict[float, float]) -> float | None:
    """Quantile from cumulative `le` buckets (lifetime, linear interpolation). None if empty."""
    if not buckets:
        return None
    bounds = sorted(buckets)
    total = buckets[bounds[-1]]
    if total <= 0:
        return None
    rank = q * total
    prev_bound, prev_count = 0.0, 0.0
    for b in bounds:
        count = buckets[b]
        if count >= rank:
            if b == float("inf"):
                return prev_bound            # overflow bucket: best finite bound we know
            if count == prev_count:
                return b
            return prev_bound + (b - prev_bound) * ((rank - prev_count) / (count - prev_count))
        prev_bound, prev_count = b, count
    return prev_bound


def _to_epoch(ts: object) -> float | None:
    if not isinstance(ts, str):
        return None
    try:
        return datetime.fromisoformat(ts).astimezone(UTC).timestamp()
    except ValueError:
        return None


def _dict(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


_SERVICE_NAME_RE = re.compile(r"[a-z][a-z0-9_-]*")


def base_url_problem(value: object) -> str | None:
    """Why an llm_client base_url is not allowed, or None. Allowed hosts: loopback (127.0.0.0/8, ::1,
    localhost) and bare single-label names (docker service names such as `litellm`). No userinfo."""
    if not isinstance(value, str) or not value.strip():
        return "base_url is missing or not a string"
    try:
        parts = urllib.parse.urlsplit(value.strip())
        _ = parts.port                                   # raises ValueError on a bad port
    except ValueError as exc:
        return f"base_url '{value}' does not parse: {exc}"
    if parts.scheme not in {"http", "https"}:
        return f"base_url '{value}' must use http or https"
    if "@" in parts.netloc or parts.username is not None or parts.password is not None:
        return "base_url must not contain userinfo (user:pass@host)"
    host = (parts.hostname or "").lower()
    host = host.removesuffix(".")
    if not host:
        return f"base_url '{value}' has no host"
    try:
        ip = ipaddress.ip_address(host)
    except ValueError:
        ip = None
    if ip is not None:
        if ip.is_loopback or (isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped is not None
                              and ip.ipv4_mapped.is_loopback):
            return None
        return f"base_url host {host} is not loopback; only loopback or an in-network service name is allowed"
    if host == "localhost" or _SERVICE_NAME_RE.fullmatch(host):
        return None
    return (f"base_url host '{host}' is neither loopback nor a bare service name (no dots); "
            "external hosts need an egress-allowlist change, not a routes.toml edit")


_IDENT_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:/@+\-]{0,119}")
REDACTED = "<redacted>"


def _ident(v: object, *, allow_empty: bool = False) -> str | None:
    """Client-controlled strings are copied into events only if identifier-shaped; else a placeholder.
    Non-strings -> None. (Keeps the metadata-only guarantee: no free text via model/route/session names.)"""
    if not isinstance(v, str):
        return None
    if v == "" and allow_empty:
        return ""
    return v if _IDENT_RE.fullmatch(v) else REDACTED


def _int(v: object) -> int:
    return v if isinstance(v, int) and not isinstance(v, bool) else 0


def capable_models(cfg: dict[str, Any]) -> dict[str, list[str]]:
    """Parsed routes.toml -> {capable-tier model id: [route ids]}. stage_router routes only.

    `capable_target` names a [targets.<key>] table and the model id is that table's `id` (a router alias). Passthroughs
    (direct-*) and targets no route escalates to are NOT capable tiers. Never raises on a malformed document.
    """
    out: dict[str, list[str]] = {}
    targets = _dict(cfg.get("targets"))
    for key, route in _dict(cfg.get("routes")).items():
        if not isinstance(route, dict) or route.get("type") != "stage_router":
            continue
        ref = route.get("capable_target")
        tgt = targets.get(ref) if isinstance(ref, str) else None
        if isinstance(tgt, dict) and isinstance(tgt.get("id"), str):
            out.setdefault(tgt["id"], []).append(str(route.get("id", key)))
    return out


def parse_routing_line(line: str) -> SystemEvent | None:
    """One routing.jsonl line -> event. METADATA ONLY (whitelisted fields); None if malformed.

    Older records (before route_id/algorithm were logged) lack those keys: tolerated."""
    line = line.strip()
    if not line:
        return None
    try:
        rec = json.loads(line)
    except json.JSONDecodeError:
        return None
    if not isinstance(rec, dict):
        return None
    ts = _to_epoch(rec.get("ts"))
    if ts is None:
        return None
    route = _ident(rec.get("route_id"))
    algo = _ident(rec.get("algorithm"))
    model = _ident(rec.get("model"))
    tier = _ident(rec.get("tier"), allow_empty=True) or ""
    session = _ident(rec.get("session_id"))
    data: dict[str, Any] = {
        "route": route, "algorithm": algo, "model": model, "tier": tier,
        "session_id": session,
        "prompt_tokens": _int(rec.get("prompt_tokens")),
        "completion_tokens": _int(rec.get("completion_tokens")),
        "reasoning_tokens": _int(rec.get("reasoning_tokens")),
        "cached_tokens": _int(rec.get("cached_tokens")),
        "cache_creation_tokens": _int(rec.get("cache_creation_tokens")),
        "total_tokens": _int(rec.get("total_tokens")),
    }
    tier_txt = f" [{tier}]" if tier else ""
    summary = (f"{route or '-'} -> {model or '?'}{tier_txt} "
               f"{data['prompt_tokens']}+{data['completion_tokens']} tok")
    return SystemEvent(ts=ts, system="switchyard", kind="routing.decision", summary=summary, data=data)


def _tidy_dry_run_message(text: str) -> str:
    """The binary repeats its message ('X: X: X'); keep one copy, drop the path prefix."""
    text = text.strip()
    text = re.sub(r"^invalid server config \S+: ", "", text)
    cut = text.find("\n: ")
    if cut != -1:
        text = text[:cut]
    half = text.split(": ")
    if len(half) == 2 and half[0] == half[1]:
        text = half[0]
    return text.strip()


# ── adapter ─────────────────────────────────────────────────────────────────────────────────
class SwitchyardAdapter(SystemAdapter):
    id = "switchyard"
    label = "NeMo Switchyard"

    container = CONTAINER
    poll_seconds = 30.0              # post-apply /health budget
    poll_interval = 1.0
    follow_interval = 2.0

    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self._route_ids: list[str] = []                       # last live /v1/models ids
        self._prev_model_counts: dict[str, tuple[int, int]] = {}   # model -> (calls = SUCCESSES, errors)
        self._route_calls_cache: tuple[float, list[MetricPoint]] | None = None

    # ── paths / urls ────────────────────────────────────────────────────────────────────────
    @property
    def base_url(self) -> str:
        return self.ctx.urls.get("switchyard", "http://127.0.0.1:4010").rstrip("/")

    def _dir(self) -> Path:
        if "switchyard_dir" in self.ctx.paths:
            return self.ctx.paths["switchyard_dir"]
        return self.ctx.paths["switchyard_routes"].parent

    def _routes_path(self) -> Path:
        if "switchyard_routes" in self.ctx.paths:
            return self.ctx.paths["switchyard_routes"]
        return self.ctx.paths["switchyard_dir"] / "routes.toml"

    def config_path(self, file_id: str) -> Path:
        if file_id != "routes":
            raise KeyError(file_id)
        return self._routes_path()

    # ── http ────────────────────────────────────────────────────────────────────────────────
    async def _get(self, path: str, timeout: float = 5.0) -> httpx.Response | None:
        try:
            return await self.ctx.http.get(self.base_url + path, timeout=timeout)
        except httpx.HTTPError:
            return None

    async def _get_json(self, path: str) -> Any | None:
        r = await self._get(path)
        if r is None or r.status_code != 200:
            return None
        try:
            return r.json()
        except ValueError:
            return None

    # ── docker (through the runner only) ────────────────────────────────────────────────────
    async def _inspect(self) -> tuple[str, str, str] | None:
        """(state, health, image) after verifying the compose labels; None when absent/foreign."""
        fmt = ("{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{end}}|{{.Config.Image}}|"
               '{{index .Config.Labels "com.docker.compose.project"}}|'
               '{{index .Config.Labels "com.docker.compose.service"}}')
        res = await self.ctx.runner.run(["docker", "inspect", "--format", fmt, self.container], timeout=10.0)
        if res.returncode != 0:
            return None
        parts = res.stdout.strip().split("|")
        if len(parts) != 5 or parts[3] != COMPOSE_PROJECT or parts[4] != COMPOSE_SERVICE:
            return None
        return parts[0], parts[1], parts[2]

    async def _tail_log(self, n: int) -> list[str] | None:
        """Newest n routing lines, or None when the log cannot be read."""
        if await self._inspect() is None:
            return None
        res = await self.ctx.runner.run(
            ["docker", "exec", self.container, "tail", "-n", str(n), LOG_PATH],
            timeout=15.0, max_bytes=min(2_000_000, n * 700 + 4096))
        if res.returncode != 0:
            return None
        return res.stdout.splitlines()

    # ── status ──────────────────────────────────────────────────────────────────────────────
    def _endpoints(self) -> list[Endpoint]:
        b = self.base_url
        return [
            Endpoint(name="chat", url=f"{b}/v1/chat/completions", role="api", auth="forwarded"),
            Endpoint(name="messages", url=f"{b}/v1/messages", role="api", auth="forwarded"),
            Endpoint(name="models", url=f"{b}/v1/models", role="api", auth="none"),
            Endpoint(name="health", url=f"{b}/health", role="health", auth="none"),
            Endpoint(name="metrics", url=f"{b}/metrics", role="metrics", auth="none"),
        ]

    def _compose_up_cmd(self) -> str:
        return f"docker compose -f {self._dir() / 'docker-compose.yml'} up -d --no-build"

    def _read_routes_file(self) -> dict[str, Any] | None:
        try:
            p = self._routes_path()
            if p.stat().st_size > MAX_CONFIG_BYTES:
                return None
            return tomllib.loads(p.read_text("utf-8"))
        except (OSError, KeyError, UnicodeDecodeError, tomllib.TOMLDecodeError):
            return None

    async def status(self) -> SystemStatus:
        now = time.time()
        problems: list[Problem] = []
        components: list[Component] = []

        try:
            insp = await self._inspect()
        except Exception:  # noqa: BLE001 - failure is data
            insp = None
        if insp is None:
            components.append(Component(kind="container", name=self.container, state="absent",
                                        detail="not found or not the compose service ai-lab-switchyard/switchyard"))
        else:
            st, hl, img = insp
            state = "running" if st == "running" else ("stopped" if st in {"exited", "created", "paused"}
                                                       else "unknown")
            components.append(Component(kind="container", name=self.container, state=state, image=img,
                                        detail=f"status={st} health={hl or 'none'}"))
            if hl == "unhealthy":
                problems.append(Problem(severity="error", code="switchyard.container.unhealthy",
                                        message="Docker healthcheck reports the switchyard container unhealthy.",
                                        fix=f"docker logs --tail 50 {self.container}"))

        health_r = await self._get("/health")
        up = health_r is not None and health_r.status_code == 200
        components.append(Component(kind="http", name=f"{self.base_url}/health",
                                    state="running" if up else "failed",
                                    detail=None if health_r is None else f"HTTP {health_r.status_code}"))

        if not up:
            problems.append(Problem(
                severity="error", code="switchyard.down",
                message=f"Switchyard is not answering /health at {self.base_url}.",
                fix=self._compose_up_cmd()))
            return SystemStatus(system="switchyard", health="down", version=None,
                                summary="Switchyard is down", endpoints=self._endpoints(),
                                components=components, problems=problems, checked_at=now)

        version = await self._version()
        models = await self._get_json("/v1/models")
        route_ids: list[str] = []
        if isinstance(models, dict) and isinstance(models.get("data"), list):
            route_ids = sorted(m["id"] for m in models["data"] if isinstance(m, dict) and isinstance(m.get("id"), str))
            self._route_ids = route_ids
        stats = await self._get_json("/v1/stats")
        problems.extend(self._route_problems(route_ids))
        if isinstance(stats, dict):
            problems.extend(self._stats_problems(stats))

        worst: Health = "degraded" if problems else "up"
        reqs = _int(stats.get("total_requests")) if isinstance(stats, dict) else 0
        summary = f"{len(route_ids)} routes"
        if isinstance(stats, dict):
            summary += f", {reqs} requests, {_int(stats.get('total_errors'))} errors (since start)"
        if problems:
            summary += f"; {len(problems)} problem(s)"
        return SystemStatus(system="switchyard", health=worst, version=version,
                            summary=summary, endpoints=self._endpoints(), components=components,
                            problems=problems, checked_at=now)

    async def _version(self) -> str | None:
        r = await self._get("/metrics")
        if r is None or r.status_code != 200:
            return None
        for name, labels, _ in parse_prometheus(r.text):
            if name == "switchyard_build_info":
                return labels.get("version")
        return None

    def _route_problems(self, live_ids: list[str]) -> list[Problem]:
        cfg = self._read_routes_file()
        if cfg is None or not live_ids:
            return []
        file_ids = sorted(str(r.get("id", k)) for k, r in (cfg.get("routes") or {}).items() if isinstance(r, dict))
        if file_ids and file_ids != live_ids:
            missing = sorted(set(file_ids) - set(live_ids))
            extra = sorted(set(live_ids) - set(file_ids))
            return [Problem(
                severity="warn", code="switchyard.config.drift",
                message=("routes.toml differs from what the running server publishes "
                         f"(only in file: {missing or '-'}; only live: {extra or '-'}). "
                         "The container has not been recreated since the file changed."),
                fix=f"docker compose -f {self._dir() / 'docker-compose.yml'} up -d --force-recreate --no-build")]
        return []

    def _capable_models(self) -> dict[str, list[str]]:
        """capable-tier model id -> route ids, from routes.toml (stage_router routes only)."""
        return capable_models(self._read_routes_file() or {})

    def _stats_problems(self, stats: dict[str, Any]) -> list[Problem]:
        problems: list[Problem] = []
        models = _dict(stats.get("models"))
        for mid, routes in sorted(self._capable_models().items()):
            m = models.get(mid)
            if not isinstance(m, dict):
                continue
            # `calls` counts SUCCESSES only: calls + errors = attempts (live 0.3.0: sum(calls) + sum(errors) ==
            # total_requests; fixture stats.json 72 + 1 == 73). So a target that never succeeded has calls == 0 and
            # the old `calls > 0 and errs * 2 >= calls` could never flag it (auto-cluster's capable tier: 0 / 2).
            calls, errs = _int(m.get("calls")), _int(m.get("errors"))
            prev = self._prev_model_counts.get(mid)
            failing_now = prev is not None and errs > prev[1] and calls <= prev[0]   # new errors, no new success
            mostly_failing = errs > 0 and errs >= calls                              # >= 50% of attempts failed
            if failing_now or mostly_failing:
                problems.append(Problem(
                    severity="warn", code="switchyard.capable_tier.upstream_errors",
                    message=(f"Capable-tier model '{mid}' (routes {', '.join(routes)}) is returning upstream errors "
                             f"({errs} errors, {calls} successes). The engine behind it may be stopped."),
                    fix=(f"curl -s http://127.0.0.1:4010/v1/stats | jq '.models[\"{mid}\"]'  "
                         "# then start the engine that serves this alias, or route via a direct-* / other auto-* route")))
        for mid, m in models.items():
            if isinstance(m, dict):
                self._prev_model_counts[mid] = (_int(m.get("calls")), _int(m.get("errors")))
        fb = stats.get("routing_fallbacks")
        if isinstance(fb, dict) and _int(fb.get("unavailable")) > 0:
            problems.append(Problem(
                severity="warn", code="switchyard.routing.fallback_unavailable",
                message=f"{_int(fb.get('unavailable'))} request(s) were re-routed because a target was unavailable.",
                fix="curl -s http://127.0.0.1:4010/v1/stats | jq .routing_fallbacks"))
        return problems

    # ── metrics ─────────────────────────────────────────────────────────────────────────────
    async def metrics(self) -> MetricsSnapshot:
        now = time.time()
        try:
            return await self._metrics(now)
        except Exception as exc:  # noqa: BLE001 - failure is data
            return MetricsSnapshot(system="switchyard", ts=now, available=False, points=[],
                                   error=f"{type(exc).__name__}: {exc}")

    async def _metrics(self, now: float) -> MetricsSnapshot:
        mr = await self._get("/metrics")
        stats = await self._get_json("/v1/stats")
        samples = parse_prometheus(mr.text) if mr is not None and mr.status_code == 200 else []
        if not samples and not isinstance(stats, dict):
            return MetricsSnapshot(system="switchyard", ts=now, available=False, points=[],
                                   error="neither /metrics nor /v1/stats answered")

        pts: list[MetricPoint] = []

        def add(key: str, value: float, unit: str = "1", **labels: str) -> None:
            pts.append(MetricPoint(key=key, value=float(value), unit=unit, labels=dict(labels)))

        def total(name: str) -> float | None:
            vals = [v for n, _, v in samples if n == name]
            return sum(vals) if vals else None

        if isinstance(stats, dict):
            if "total_requests" in stats:
                add("sy.requests_total", _int(stats["total_requests"]))
            if "total_errors" in stats:
                add("sy.errors_total", _int(stats["total_errors"]))
            tt = stats.get("total_tokens")
            if isinstance(tt, dict):
                for kind in ("prompt", "completion", "reasoning", "cached"):
                    if kind in tt:
                        add("sy.tokens", _int(tt[kind]), "tok", kind=kind)
            models = stats.get("models")
            if isinstance(models, dict):
                for mid, m in sorted(models.items()):
                    if isinstance(m, dict) and "calls" in m:
                        add("sy.model.calls", _int(m["calls"]), model=mid)
        else:                                                  # fall back to /metrics counters
            for key, name in (("sy.requests_total", "switchyard_total_requests"),
                              ("sy.errors_total", "switchyard_total_errors")):
                v = total(name)
                if v is not None:
                    add(key, v)
            for kind, name in (("prompt", "switchyard_prompt_tokens_total"),
                               ("completion", "switchyard_completion_tokens_total"),
                               ("reasoning", "switchyard_reasoning_tokens_total")):
                v = total(name)
                if v is not None:
                    add("sy.tokens", v, "tok", kind=kind)
            for n, labels, v in samples:
                if n == "switchyard_requests_total" and "model" in labels:
                    add("sy.model.calls", v, model=labels["model"])

        inflight = total("switchyard_algorithms_in_flight")
        if inflight is not None:
            add("sy.in_flight", inflight)

        buckets: dict[float, float] = {}
        for n, labels, v in samples:
            if n == "switchyard_llm_call_duration_ms_bucket" and "le" in labels:
                le = float("inf") if labels["le"] in {"+Inf", "Inf"} else float(labels["le"])
                buckets[le] = buckets.get(le, 0.0) + v
        for qname, q in (("p50", 0.5), ("p95", 0.95)):
            val = histogram_quantile(q, buckets)
            if val is not None:
                add("sy.latency_ms", val, "ms", q=qname)

        pts.extend(await self._route_call_points())
        return MetricsSnapshot(system="switchyard", ts=now, available=True, points=pts)

    async def _route_call_points(self) -> list[MetricPoint]:
        """sy.route.calls{route}: /metrics has no per-route counter, so count the newest
        ROUTE_COUNT_WINDOW routing.jsonl records (classifier judge rows excluded). Labelled with the
        window so nobody mistakes it for a lifetime counter. Cached 15 s (it costs a docker exec)."""
        cached = self._route_calls_cache
        if cached is not None and time.monotonic() - cached[0] < 15.0:
            return cached[1]
        lines = await self._tail_log(ROUTE_COUNT_WINDOW)
        if lines is None:
            return []
        counts: dict[str, int] = {}
        n = 0
        for line in lines:
            ev = parse_routing_line(line)
            if ev is None:
                continue
            n += 1
            route, tier = ev.data.get("route"), ev.data.get("tier")
            if route and tier != "classifier":
                counts[route] = counts.get(route, 0) + 1
        window = f"last_{n}_records"
        pts = [MetricPoint(key="sy.route.calls", value=float(c), unit="1",
                           labels={"route": r, "window": window}) for r, c in sorted(counts.items())]
        self._route_calls_cache = (time.monotonic(), pts)
        return pts

    # ── events ──────────────────────────────────────────────────────────────────────────────
    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        """Newest `limit` routing decisions, oldest first. [] when the log is unreadable."""
        limit = max(1, min(int(limit), 1000))
        try:
            lines = await self._tail_log(limit)
        except Exception:  # noqa: BLE001
            return []
        if lines is None:
            return []
        evs = [e for e in (parse_routing_line(ln) for ln in lines) if e is not None]
        return evs[-limit:]

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        """Poll the log tail every follow_interval s (no streaming exec in the runner port).
        since_ts=None starts at 'now': only records newer than the newest existing one are emitted."""
        cutoff = since_ts
        seen_at_cutoff: set[str] = set()
        primed = False
        while True:
            try:
                lines = await self._tail_log(300)
            except Exception:  # noqa: BLE001
                lines = None
            if lines is not None:
                batch = [(ev, ln) for ln in lines if (ev := parse_routing_line(ln)) is not None]
                if not primed:
                    if cutoff is None:
                        cutoff = max((ev.ts for ev, _ in batch), default=time.time())
                    seen_at_cutoff = {hashlib.sha1(ln.encode()).hexdigest() for ev, ln in batch
                                      if ev.ts == cutoff}
                    primed = True
                assert cutoff is not None
                for ev, ln in batch:
                    h = hashlib.sha1(ln.encode()).hexdigest()
                    if ev.ts < cutoff or (ev.ts == cutoff and h in seen_at_cutoff):
                        continue
                    if ev.ts > cutoff:
                        cutoff, seen_at_cutoff = ev.ts, set()
                    seen_at_cutoff.add(h)
                    yield ev
            await asyncio.sleep(self.follow_interval)

    # ── config ──────────────────────────────────────────────────────────────────────────────
    def config_files(self) -> list[ConfigFileSpec]:
        return [ConfigFileSpec(
            id="routes", label="Switchyard routes",
            description="Route table, upstream LLM clients and targets for switchyard-server 0.3.0. "
                        "Mounted read-only into the container, so a change needs a recreate.",
            display_path="inference/switchyard/routes.toml", format="toml", editable=True,
            validators=[DRY_RUN_VALIDATOR, SEMANTIC_VALIDATOR], authoritative=True,
            apply_effect=("Recreates the switchyard container (docker compose up -d --force-recreate --no-build): "
                          "a few seconds of refused/dropped requests, and the in-memory /v1/stats counters reset. "
                          "routing.jsonl survives (named volume)."))]

    async def read_config(self, file_id: str) -> ConfigFile:
        path = self.config_path(file_id)
        spec = self.config_files()[0]

        def _read() -> tuple[bytes, float]:
            st = path.stat()
            if st.st_size > MAX_CONFIG_BYTES:
                raise ValueError(f"{path.name} exceeds {MAX_CONFIG_BYTES} bytes")
            return path.read_bytes(), st.st_mtime

        raw, mtime = await asyncio.to_thread(_read)
        return ConfigFile(spec=spec, content=raw.decode("utf-8"), sha256=hashlib.sha256(raw).hexdigest(),
                          mtime=mtime)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        if file_id != "routes":
            raise KeyError(file_id)
        diags: list[Diagnostic] = []
        raw = content.encode("utf-8")
        if len(raw) > MAX_CONFIG_BYTES:
            return ValidationResult(ok=False, validator=DRY_RUN_VALIDATOR, authoritative=True,
                                    diagnostics=[Diagnostic(severity="error",
                                                            message=f"content exceeds {MAX_CONFIG_BYTES} bytes")])
        parsed: dict[str, Any] | None = None
        try:
            parsed = tomllib.loads(content)
        except tomllib.TOMLDecodeError as exc:
            m = _LINE_RE.search(str(exc))
            diags.append(Diagnostic(severity="error", message=f"TOML syntax: {exc}",
                                    line=int(m.group(1)) if m else None))
        if parsed is not None:
            diags.extend(self.semantic_check(parsed))
        # The authoritative pass runs even after a semantic error so the user sees the binary's view too.
        dry, note = await self._dry_run(raw)
        diags.extend(dry)
        ok = not any(d.severity == "error" for d in diags)
        return ValidationResult(ok=ok, validator=DRY_RUN_VALIDATOR, authoritative=True, diagnostics=diags,
                                note=note)

    @staticmethod
    def semantic_check(cfg: dict[str, Any]) -> list[Diagnostic]:
        d: list[Diagnostic] = []
        clients = _dict(cfg.get("llm_clients"))
        targets = _dict(cfg.get("targets"))
        routes = _dict(cfg.get("routes"))
        for cname, c in clients.items():
            if isinstance(c, dict) and (why := base_url_problem(c.get("base_url"))):
                d.append(Diagnostic(severity="error", message=f"llm_client '{cname}': {why}"))
        seen: dict[tuple[str, str], str] = {}
        for name, t in targets.items():
            if not isinstance(t, dict):
                continue
            client = t.get("llm_client")
            if client not in clients:
                d.append(Diagnostic(severity="error",
                                    message=f"target '{name}' uses llm_client '{client}' which is not defined "
                                            "under [llm_clients]"))
            ident = (str(t.get("id")), str(client))
            if ident in seen:
                d.append(Diagnostic(
                    severity="warn",
                    message=(f"targets '{seen[ident]}' and '{name}' share (id '{ident[0]}', llm_client "
                             f"'{ident[1]}'): switchyard keeps only one and silently drops the other (with "
                             "its system_prompt). Give one its own [llm_clients.*] block.")))
            else:
                seen[ident] = str(name)
        for rname, r in routes.items():
            if not isinstance(r, dict):
                continue
            refs: list[tuple[str, object]] = [(k, v) for k, v in r.items() if k == "target" or k.endswith("_target")]
            cls = r.get("classifier")
            if isinstance(cls, dict) and "target" in cls:
                refs.append(("classifier.target", cls["target"]))
            for field_name, ref in refs:
                if not isinstance(ref, str) or ref not in targets:
                    d.append(Diagnostic(severity="error",
                                        message=f"route '{rname}': {field_name} = '{ref}' is not a defined "
                                                "[targets.*] entry"))
            if "id" in r and r["id"] != rname:
                d.append(Diagnostic(severity="warn",
                                    message=f"route table key '{rname}' differs from its id '{r['id']}' "
                                            "(clients see the id)"))
        if not routes:
            d.append(Diagnostic(severity="error", message="no [routes.*] defined"))
        return d

    async def _image(self) -> str:
        try:
            insp = await self._inspect()
        except Exception:  # noqa: BLE001
            insp = None
        return insp[2] if insp else FALLBACK_IMAGE

    async def _dry_run(self, raw: bytes) -> tuple[list[Diagnostic], str | None]:
        """Candidate goes over stdin into a throwaway, network-less, read-only container."""
        image = await self._image()
        argv = ["docker", "run", "--rm", "-i", "--pull", "never", "--network", "none", "--read-only",
                "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true", "--pids-limit", "64",
                "--memory", "256m", "--entrypoint", "switchyard-server", image,
                "--config", "/dev/stdin", "--dry-run"]
        try:
            res: CommandResult = await self.ctx.runner.run(argv, timeout=60.0, stdin=raw)
        except Exception as exc:  # noqa: BLE001
            return ([Diagnostic(severity="error",
                                message=f"could not run the authoritative dry-run: {type(exc).__name__}: {exc}")], None)
        if res.timed_out:
            return [Diagnostic(severity="error", message="dry-run timed out")], None
        if res.returncode == 0:
            diags = []
            for ln in res.stderr.splitlines():
                if " WARN " in ln:
                    diags.append(Diagnostic(severity="warn", message=ln.split(" WARN ", 1)[1].strip()))
            return diags, res.stdout.strip() or None
        msg = _tidy_dry_run_message(res.stderr or res.stdout)
        if "Unable to find image" in msg or "No such image" in msg or not msg:
            msg = f"dry-run could not start (image {image} missing?): {msg or 'exit ' + str(res.returncode)}"
        m = _LINE_RE.search(msg)
        return [Diagnostic(severity="error", message=msg, line=int(m.group(1)) if m else None)], None

    # ── apply ───────────────────────────────────────────────────────────────────────────────
    def _recreate_argv(self) -> list[str]:
        return ["docker", "compose", "-f", str(self._dir() / "docker-compose.yml"),
                "up", "-d", "--force-recreate", "--no-build"]

    async def _wait_healthy(self) -> bool:
        deadline = time.monotonic() + self.poll_seconds
        while True:
            r = await self._get("/health", timeout=3.0)
            if r is not None and r.status_code == 200:
                return True
            if time.monotonic() >= deadline:
                return False
            await asyncio.sleep(self.poll_interval)

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        if file_id != "routes":
            raise KeyError(file_id)
        if not restart:
            return ApplyResult(ok=True, restarted=False, post_check="skipped",
                               detail="file written; the running server still uses the old routes until recreated")
        res = await self.ctx.runner.run(self._recreate_argv(), timeout=120.0, cwd=self._dir())
        if res.returncode != 0 or res.timed_out:
            return ApplyResult(ok=False, restarted=False, post_check="unhealthy",
                               detail=(res.stderr or res.stdout or "compose recreate failed")[-2000:])
        healthy = await self._wait_healthy()
        return ApplyResult(
            ok=healthy, restarted=True, post_check="healthy" if healthy else "unhealthy",
            detail=("recreated; /health OK" if healthy
                    else f"recreated but /health not OK within {self.poll_seconds:.0f}s"))

    # ── actions ─────────────────────────────────────────────────────────────────────────────
    def actions(self) -> list[ActionSpec]:
        return [
            ActionSpec(id="restart", label="Recreate Switchyard", danger="disruptive",
                       description="docker compose up -d --force-recreate --no-build in inference/switchyard. "
                                   "Recreate (not plain restart) so a replaced routes.toml is re-mounted. "
                                   "Drops in-flight requests for a few seconds and resets /v1/stats."),
            ActionSpec(id="reset_stats", label="Reset stats", danger="destructive",
                       description="POST /v1/stats/reset: zeroes the in-memory counters behind /v1/stats. "
                                   "routing.jsonl is not touched."),
            ActionSpec(id="dry_run_config", label="Dry-run live routes.toml", danger="safe",
                       description="Validate the on-disk routes.toml with switchyard-server --dry-run in a "
                                   "throwaway network-less container."),
        ]

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        t0 = time.monotonic()

        def ms() -> int:
            return int((time.monotonic() - t0) * 1000)

        if action_id == "restart":
            r = await self.post_apply("routes", restart=True)
            probs = [] if r.ok else [Problem(severity="error", code="switchyard.restart.unhealthy",
                                             message=r.detail,
                                             fix=f"docker logs --tail 50 {self.container}")]
            return ActionResult(ok=r.ok, output=r.detail, duration_ms=ms(), problems=probs)
        if action_id == "reset_stats":
            try:
                resp = await self.ctx.http.post(self.base_url + "/v1/stats/reset", timeout=10.0)
            except httpx.HTTPError as exc:
                return ActionResult(ok=False, output=f"request failed: {type(exc).__name__}", duration_ms=ms(),
                                    problems=[Problem(severity="error", code="switchyard.down",
                                                      message="Switchyard did not answer.",
                                                      fix=self._compose_up_cmd())])
            ok = 200 <= resp.status_code < 300
            # /v1/stats/reset needs no credential (verified live by the coordinator): report status plainly.
            return ActionResult(ok=ok, output=f"HTTP {resp.status_code}", duration_ms=ms(), problems=[])
        if action_id == "dry_run_config":
            cf = await self.read_config("routes")
            v = await self.validate_config("routes", cf.content)
            lines = [f"[{x.severity}] {x.message}" for x in v.diagnostics]
            if v.note:
                lines.append(v.note)
            return ActionResult(ok=v.ok, output="\n".join(lines) or "ok", duration_ms=ms())
        raise KeyError(action_id)

    # ── playground ──────────────────────────────────────────────────────────────────────────
    def playground(self) -> PlaygroundSpec | None:
        ids = list(self._route_ids)
        if not ids:                                   # not yet observed live: fall back to the file
            cfg = self._read_routes_file() or {}
            ids = sorted(str(r.get("id", k)) for k, r in (cfg.get("routes") or {}).items() if isinstance(r, dict))
        if not ids:
            ids = ["auto-local"]
        default = "auto-local" if "auto-local" in ids else ids[0]
        return PlaygroundSpec(
            wire="openai_chat", models=ids, default_model=default,
            note=("Models are Switchyard route ids (auto-* route by tier, direct-* are fixed passthroughs). "
                  "Switchyard uses forward_auth: it forwards the caller's bearer to LiteLLM, so the console "
                  "sends its own key, resolved server-side from the env var named in secret_env['switchyard']."))

    def playground_url(self) -> str | None:
        return self.base_url + "/v1/chat/completions"
