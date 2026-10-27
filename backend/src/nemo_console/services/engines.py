"""One shared, cached, never-raising engine-container probe (topology + the Switchyard capable-tier check), and the
opt-in active capable-tier probe (addendum 9.11)."""
from __future__ import annotations

import asyncio
import contextlib
import logging
import os
import re
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

import httpx

from nemo_console.adapters.base import CommandRunner
from nemo_console.adapters.switchyard import capable_models
from nemo_console.domain.models import Problem
from nemo_console.services.common import KNOWN

log = logging.getLogger(__name__)

# "absent": no such container (the engine is not deployed on this host); "stopped": it exists and is not running
EngineState = Literal["running", "stopped", "absent", "unknown"]
CONTAINER_RE = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9_.\-]{0,127}")
TTL = 5.0


class EngineProbe:
    def __init__(self, runner: CommandRunner, ttl: float = TTL) -> None:
        self.runner = runner
        self.ttl = ttl
        self._cache: dict[str, tuple[float, EngineState]] = {}

    async def state(self, container: str) -> EngineState:
        if CONTAINER_RE.fullmatch(container) is None:
            return "unknown"
        hit = self._cache.get(container)
        if hit and time.monotonic() - hit[0] < self.ttl:
            return hit[1]
        st: EngineState
        try:
            r = await self.runner.run(["docker", "inspect", "-f", "{{.State.Status}}", container], timeout=5.0, max_bytes=200)
            if r.timed_out:
                st = "unknown"
            elif r.returncode != 0:
                # "No such object" = absent (not deployed); anything else (daemon down, ...) is unknown
                st = "absent" if "no such" in (r.stderr + r.stdout).lower() else "unknown"
            else:
                st = "running" if r.stdout.strip() == "running" else "stopped"
        except Exception:  # noqa: BLE001 - probing never raises
            st = "unknown"
        self._cache[container] = (time.monotonic(), st)
        return st


def compose_dir_hint(repo_root: Path, container: str) -> str | None:
    """ai-lab-<project>-<service>-<n> -> inference/<project> when that directory exists."""
    name = container.removeprefix("ai-lab-")
    name = re.sub(r"-\d+$", "", name)
    parts = name.split("-")
    for i in range(len(parts), 0, -1):
        cand = "-".join(parts[:i])
        if (repo_root / "inference" / cand).is_dir():
            return f"inference/{cand}"
    return None


def start_fix(repo_root: Path, container: str, engine_start: dict[str, str]) -> str:
    """The copy-pasteable remedy for a stopped engine: its configured start command, else the compose-dir guess."""
    cmd = engine_start.get(container)
    if cmd:
        return f"cd {repo_root} && {cmd}"
    hint = compose_dir_hint(repo_root, container)
    where = f"cd {hint}" if hint else "cd the engine's compose directory"
    return f"{where} && docker compose up -d"


async def capable_tier_problems(probe: EngineProbe, engine_probes: dict[str, str], routes_text: str,
                                repo_root: Path, engine_start: dict[str, str] | None = None) -> list[Problem]:
    """capable_dark (warn) for each probed alias that a stage_router route escalates to and whose container exists but
    is not running; capable_not_deployed (info) when that container does not exist at all (2026-10-17: an engine that is
    not part of this host's layout is not a fault, and its start command could tear down the layout that is).

    Matched on the PARSED capable_target ids (2026-09-26). It used to be `alias in routes_text`, which also matched
    passthroughs (direct-27b) and bare [targets.*] tables, so an engine that is dark on purpose and that no route
    escalates to kept Switchyard `degraded` for good. A file that does not parse yields nothing here; the config
    validator and the adapter report that.
    """
    try:
        capable = capable_models(tomllib.loads(routes_text))
    except tomllib.TOMLDecodeError:
        return []
    out: list[Problem] = []
    for alias, container in engine_probes.items():
        routes = capable.get(alias)
        if not routes:
            continue
        state = await probe.state(container)
        which = f"route {routes[0]}" if len(routes) == 1 else "routes " + ", ".join(routes)
        if state == "stopped":
            out.append(Problem(
                severity="warn", code="switchyard.tier.capable_dark",
                message=f"Capable tier target {alias} is not answering: engine container {container} is not running, "
                        f"so {which} cannot escalate to it",
                fix=start_fix(repo_root, container, engine_start or {})))
        elif state == "absent":
            out.append(Problem(
                severity="info", code="switchyard.tier.capable_not_deployed",
                message=f"Capable tier target {alias} is not deployed here (no container {container}), so {which} "
                        "cannot escalate to it",
                fix=f"point {which} at a deployed engine in routes.toml, or deploy {alias} if that tier should exist "
                    f"(start: {start_fix(repo_root, container, engine_start or {})})"))
    return out


def capable_targets(engine_probes: dict[str, str], routes_text: str) -> list[tuple[str, str, list[str]]]:
    """(alias, container, routes) for each probed alias that a stage_router route escalates to; [] if unparseable."""
    try:
        capable = capable_models(tomllib.loads(routes_text))
    except tomllib.TOMLDecodeError:
        return []
    return [(alias, container, capable[alias]) for alias, container in engine_probes.items() if capable.get(alias)]


# ── active capable-tier probe (addendum 9.11, opt-in) ──────────────────────────────────────────
ProbeVerdict = Literal["ok", "broken", "denied", "unconfigured"]


@dataclass(frozen=True)
class ProbeResult:
    verdict: ProbeVerdict
    detail: str                  # status only: an HTTP code, an exception class, or "no choices"; never model text
    at: float                    # time.time() of the request


class CapableProbe:
    """One tiny real completion per capable-tier alias through the router, at most once per `interval_s` per alias,
    one request at a time, in the background: status reads cached results and never waits on a model.

    Why: a container that runs is not an engine that answers (a TP rank can die while the API server's /health stays
    200), and the container check only sees the first. The request is `max_tokens: 1`, `temperature: 0`, one fixed
    user word, and LiteLLM's response cache is bypassed (`cache: {"no-cache": true}`): a cached answer would say
    nothing about the engine (found live 2026-10-26: an earlier identical probe's answer came back in 14 ms). Nothing
    of the answer is kept beyond the verdict. A request that fails is retried once after `retry_delay_s` before it
    counts, so an engine that is only busy (the probe queues behind other requests' prefills) is not reported broken.
    The credential is read from the env var NAMED by `key_env` at request time and registered with the redactor;
    without it the probe sends nothing."""

    PROMPT = "ping"

    def __init__(self, http: httpx.AsyncClient, router_url: str, key_env: str, *, interval_s: float = 300.0,
                 timeout_s: float = 60.0, retry_delay_s: float = 30.0, environ: dict[str, str] | None = None) -> None:
        self.http = http
        self.url = router_url.rstrip("/") + "/v1/chat/completions"
        self.key_env = key_env
        self.interval_s = interval_s
        self.timeout_s = timeout_s
        self.retry_delay_s = retry_delay_s
        self._environ = environ
        self._results: dict[str, ProbeResult] = {}
        self._task: asyncio.Task[None] | None = None

    def _key(self) -> str | None:
        value = (self._environ if self._environ is not None else os.environ).get(self.key_env)
        if value:
            KNOWN.add(value)
        return value or None

    def results(self) -> dict[str, ProbeResult]:
        return dict(self._results)

    def _due(self, aliases: list[str], now: float) -> list[str]:
        return [a for a in aliases if a not in self._results or now - self._results[a].at >= self.interval_s]

    def schedule(self, aliases: list[str]) -> None:
        """Start one background pass over the aliases that are due, unless a pass is already running."""
        if self._task is not None and not self._task.done():
            return
        due = self._due(aliases, time.time())
        if due:
            self._task = asyncio.create_task(self._run(due), name="capable-probe")

    async def _run(self, aliases: list[str]) -> None:
        for alias in aliases:                      # one at a time: never a burst against the router
            res = await self.probe(alias)
            if res.verdict == "broken":            # once more before it counts: busy is not broken
                await asyncio.sleep(self.retry_delay_s)
                res = await self.probe(alias)
                if res.verdict == "broken":
                    res = ProbeResult("broken", f"{res.detail}, twice {self.retry_delay_s:.0f} s apart", res.at)
            self._results[alias] = res

    async def probe(self, alias: str) -> ProbeResult:
        now = time.time()
        key = self._key()
        if key is None:
            return ProbeResult("unconfigured", f"env var {self.key_env} is not set in the console process", now)
        body = {"model": alias, "messages": [{"role": "user", "content": self.PROMPT}], "max_tokens": 1,
                "temperature": 0, "stream": False, "cache": {"no-cache": True}}
        started = time.monotonic()
        try:
            resp = await self.http.post(self.url, json=body, headers={"authorization": f"Bearer {key}"},
                                        timeout=httpx.Timeout(self.timeout_s, connect=3.0))
        except httpx.TimeoutException:
            res = ProbeResult("broken", f"no answer within {self.timeout_s:.0f} s", now)
        except httpx.HTTPError as e:
            res = ProbeResult("broken", f"router unreachable ({type(e).__name__})", now)
        else:
            if resp.status_code in (401, 403):
                res = ProbeResult("denied", f"router answered {resp.status_code}", now)
            elif resp.status_code >= 400:
                res = ProbeResult("broken", f"router answered {resp.status_code}", now)
            else:
                try:
                    doc = resp.json()
                except ValueError:
                    doc = None
                ok = isinstance(doc, dict) and isinstance(doc.get("choices"), list) and bool(doc["choices"])
                res = ProbeResult("ok" if ok else "broken", "answered" if ok else "no choices in the answer", now)
        log.info("capable probe %s: %s (%s, %.1f s)", alias, res.verdict, res.detail, time.monotonic() - started)
        return res

    async def aclose(self) -> None:
        if self._task is not None and not self._task.done():
            self._task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await self._task


def probe_problems(results: dict[str, ProbeResult], running: list[tuple[str, str, list[str]]],
                   repo_root: Path, engine_start: dict[str, str] | None = None) -> list[Problem]:
    """Problems from the cached probe results for the capable targets whose container is RUNNING (a stopped one is
    already capable_dark). One info problem covers a probe that cannot run (no credential, or the key is refused)."""
    out: list[Problem] = []
    config_issue: ProbeResult | None = None
    for alias, container, routes in running:
        res = results.get(alias)
        if res is None or res.verdict == "ok":
            continue
        if res.verdict in ("unconfigured", "denied"):
            config_issue = config_issue or res
            continue
        which = f"route {routes[0]}" if len(routes) == 1 else "routes " + ", ".join(routes)
        age = max(0, int(time.time() - res.at))
        out.append(Problem(
            severity="warn", code="switchyard.tier.capable_broken",
            message=(f"Capable tier target {alias} runs (container {container}) but did not answer a 1-token test "
                     f"completion through the router {age} s ago: {res.detail}. {which.capitalize()} cannot escalate "
                     "to it until it answers."),
            fix=f"docker logs --tail 80 {container}   # then restart it: {start_fix(repo_root, container, engine_start or {})}"))
    if config_issue is not None:
        code = "switchyard.tier.probe_unconfigured" if config_issue.verdict == "unconfigured" else "switchyard.tier.probe_denied"
        out.append(Problem(
            severity="info", code=code,
            message=f"The active capable-tier probe is on but cannot run: {config_issue.detail}.",
            fix=("export the router key in the console's environment (setting capable_probe_key_env names the variable), "
                 "or turn the probe off: NEMO_CONSOLE_CAPABLE_PROBE=false")))
    return out
