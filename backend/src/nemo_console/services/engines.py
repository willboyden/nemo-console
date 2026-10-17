"""One shared, cached, never-raising engine-container probe (topology + the Switchyard capable-tier check)."""
from __future__ import annotations

import re
import time
import tomllib
from pathlib import Path
from typing import Literal

from nemo_console.adapters.base import CommandRunner
from nemo_console.adapters.switchyard import capable_models
from nemo_console.domain.models import Problem

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
