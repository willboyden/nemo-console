"""OpenShell adapter: the gateway that contains Data Designer runs (clients/data-designer/runner), as a managed system.

Facts this adapter is built on (contract 9.8, 2026-10-24; evidence in tests/openshell/fixtures, captured live
against CLI 0.0.71 + gateway 0.0.77 unless a fixture file says "hand-written"):
  * The CLI is `openshell` (no wrapper). `--version` prints "openshell 0.0.71". Everything else talks to the
    gateway over mTLS at https://127.0.0.1:17670 (a socat forwarder container publishes it). The runner passes no
    NO_COLOR, so the human output carries ANSI escapes: every text is stripped before it is parsed or shown.
  * `openshell status` prints "Gateway / Server / Status / Version" lines. Its exit code is 0 even when no gateway
    is registered ("Status: No gateway configured."), so ONLY the `Status:` line decides "connected".
  * `openshell sandbox list -o json` prints a JSON array (empty: `[]`) of {id, name, phase, created_at (UTC,
    "YYYY-MM-DD HH:MM:SS"), current_policy_version, labels, resource_version}.
  * `openshell policy prove` needs BOTH `--policy FILE` and `--credentials FILE` (a descriptor YAML:
    credentials[{name, type, scopes, injected_via, target_hosts}]). It cannot prove a policy without one, so the
    descriptor is derived from the lab's provider profile. Exit 0 = no findings, 1 = findings (or an "Error:" line).
  * The gateway is started on demand by `data-designer-local` (`docker compose -f runner/docker-compose.yml up -d`
    after checking that `openshell-docker` is an INTERNAL network, then re-pinning the router at 172.16.0.5 on it).
    A stopped gateway is therefore health "off", never "down". `gateway_up` mirrors that sequence exactly;
    `gateway_stop` is `compose stop` (never `down -v`).
  * Trust files (gateway.toml, policy.yaml, the provider profile, the compose file) are READ-ONLY (ADR 0007): a web
    console never loosens a sandbox's trust policy. `policy prove` is offered instead.
  * Nothing here ever reads a key: the provider's credential lives in the gateway's store; the profile names an ENV
    VARIABLE only.
"""
from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import os
import re
import time
import tomllib
from collections import deque
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Literal

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
from nemo_console.services.common import redact

CLI = "openshell"
DEFAULT_CONTAINERS = ("ai-lab-dd-gateway", "ai-lab-dd-cli-fwd")      # = Settings.openshell_containers default
DEFAULT_ENDPOINT = "https://127.0.0.1:17670"
SANDBOX_NET = "openshell-docker"                                      # must be --internal (data-designer-local too)
SANDBOX_SUBNET = "172.16.0.0/24"
LITELLM_CONTAINER = "ai-lab-litellm-litellm-1"                        # re-pinned on SANDBOX_NET by ensure_gateway()
LITELLM_IP = "172.16.0.5"
PROFILE_NAME = "profile-lab-router.yaml"
EVENT_KIND = "openshell.sandbox"
NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
SCOPE_RE = re.compile(r"[a-z][a-z0-9_.:-]{0,31}")
ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
CTRL_RE = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")
VERSION_RE = re.compile(r"\d+\.\d+\.\d+[0-9A-Za-z.+-]*")
KV_RE = re.compile(r"^\s*(Gateway|Server|Status|Version):\s*(.*?)\s*$", re.MULTILINE)
VERSION_TTL = 300.0
SNAPSHOT_TTL = 2.0
MAX_CONFIG_BYTES = 256 * 1024
MAX_SANDBOXES = 200                 # `sandbox list --limit`, and the bound on tracked sandbox state
RING = 200                          # events kept in memory
PHASES = ("Provisioning", "Ready", "Error", "Deleting", "Other")      # bounded label set for os.sandboxes_by_phase
LOOPBACK = {"127.0.0.1", "::1", "localhost"}
GATEWAY_READY_TIMEOUT = 90.0        # = data-designer-local GATEWAY_TIMEOUT
Done = Callable[..., ActionResult]


def strip_ansi(text: str) -> str:
    return CTRL_RE.sub("", ANSI_RE.sub("", text))


def _dict(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


def _list(v: object) -> list[Any]:
    return v if isinstance(v, list) else []


def _utc(stamp: object) -> float | None:
    """created_at: "YYYY-MM-DD HH:MM:SS" in UTC (the fixture's value matches the log epoch of the same sandbox)."""
    if not isinstance(stamp, str):
        return None
    try:
        return datetime.strptime(stamp.strip()[:19], "%Y-%m-%d %H:%M:%S").replace(tzinfo=UTC).timestamp()
    except ValueError:
        return None


def phase_bucket(phase: str) -> str:
    p = phase.strip().lower()
    return next((k for k in PHASES[:-1] if k.lower() == p), "Other")


@dataclass(frozen=True)
class _Ctr:
    name: str
    state: str                      # docker State.Status: running / exited / restarting / created / paused / dead
    exit_code: int
    oom: bool
    started: str
    restarts: int
    image: str
    host_ips: tuple[str, ...]       # published host bindings (empty: nothing published)

    @property
    def running(self) -> bool:
        return self.state == "running"

    @property
    def failed(self) -> bool:
        """Crashed, not stopped: `docker stop` exits 0, 137 (SIGKILL after the grace) or 143 (SIGTERM)."""
        return (self.state in ("restarting", "dead") or self.oom
                or (self.state == "exited" and self.exit_code not in (0, 137, 143)))


@dataclass(frozen=True)
class _Conn:
    ran: bool                       # the `openshell status` command itself ran
    connected: bool
    text: str                       # the Status: line (or the reason it is missing)
    server: str
    version: str | None
    gateway: str


@dataclass
class _Snap:
    ts: float
    ok: bool
    rows: list[dict[str, Any]]
    error: str


class OpenShellAdapter(SystemAdapter):
    id = "openshell"
    label = "OpenShell"

    # Wiring (container.py, the _configure_dynamo pattern): `adapter.containers = tuple(cfg.openshell_containers)`.
    # The gateway is the first name containing "gateway" (else the first); the others are CLI forwarders.
    containers: tuple[str, ...] = DEFAULT_CONTAINERS
    gateway_name = "openshell"      # the gateway registration the CLI is pinned to (`-g`); never the "active" one

    # ctx.paths: openshell_runner_dir, openshell_gateway_config, openshell_policy, openshell_compose (all optional).
    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self.follow_interval = 3.0
        self.poll_interval = 3.0                    # gateway_up readiness poll
        self.ready_timeout = GATEWAY_READY_TIMEOUT
        self._version_cache: tuple[float, str | None] = (0.0, None)
        self._snap = _Snap(0.0, False, [], "not polled")
        self._snap_lock = asyncio.Lock()
        self._known: dict[str, tuple[str, str]] = {}          # name -> (id, phase); bounded by MAX_SANDBOXES
        self._baselined = False
        self._gw_cache: tuple[float, bool] = (0.0, False)
        self._ring: deque[tuple[int, SystemEvent]] = deque(maxlen=RING)
        self._seq = 0

    # ── helpers ─────────────────────────────────────────────────────────────────────────────
    @property
    def _gateway_ctr(self) -> str:
        return next((c for c in self.containers if "gateway" in c), self.containers[0] if self.containers else "")

    def _path(self, key: str) -> Path | None:
        return self.ctx.paths.get(key)

    @property
    def _runner_dir(self) -> Path | None:
        return self._path("openshell_runner_dir")

    @property
    def _compose(self) -> Path | None:
        return self._path("openshell_compose")

    def _start_fix(self) -> str:
        c = self._compose
        return (f"docker compose -f {c} up -d   # or run any data-designer-local command: it starts the gateway"
                if c else "run any data-designer-local command: it starts the gateway on demand")

    async def _run(self, argv: list[str], *, timeout: float, max_bytes: int = 256_000,
                   cwd: Path | None = None) -> CommandResult:
        try:
            return await self.ctx.runner.run(argv, timeout=timeout, max_bytes=max_bytes, cwd=cwd)
        except Exception as e:  # noqa: BLE001 - not allowlisted / not on PATH: data, not an exception
            return CommandResult(returncode=127, stdout="", stderr=f"{type(e).__name__}: {e}"[:300])

    async def _cli(self, *args: str, timeout: float = 20.0, max_bytes: int = 256_000) -> CommandResult:
        return await self._run([CLI, "-g", self.gateway_name, *args], timeout=timeout, max_bytes=max_bytes)

    async def _cli_version(self) -> str | None:
        ts, version = self._version_cache
        if version is not None and time.monotonic() - ts < VERSION_TTL:
            return version
        r = await self._run([CLI, "--version"], timeout=15.0, max_bytes=4096)
        m = VERSION_RE.search(strip_ansi(r.stdout)) if r.returncode == 0 else None
        version = m.group(0) if m else None
        self._version_cache = (time.monotonic(), version)
        return version

    # ── probes ──────────────────────────────────────────────────────────────────────────────
    async def _inspect(self) -> tuple[dict[str, _Ctr], bool]:
        """(containers found by name, docker answered). A name docker does not know is simply not in the dict."""
        if not self.containers:
            return {}, True
        fmt = ("{{.Name}}|{{.State.Status}}|{{.State.ExitCode}}|{{.State.OOMKilled}}|{{.State.StartedAt}}|"
               "{{.RestartCount}}|{{.Config.Image}}|{{json .NetworkSettings.Ports}}")
        r = await self._run(["docker", "inspect", "--format", fmt, *self.containers], timeout=15.0, max_bytes=64_000)
        found: dict[str, _Ctr] = {}
        for line in r.stdout.splitlines():
            parts = line.split("|", 7)
            if len(parts) != 8:
                continue
            name = parts[0].lstrip("/")
            try:
                ports = json.loads(parts[7])
                code, restarts = int(parts[2]), int(parts[5])
            except ValueError:
                continue
            ips = tuple(str(b.get("HostIp", "")) for bs in _dict(ports).values() for b in _list(bs) if isinstance(b, dict))
            found[name] = _Ctr(name=name, state=parts[1], exit_code=code, oom=parts[3] == "true", started=parts[4],
                               restarts=restarts, image=parts[6], host_ips=ips)
        # docker answered when it listed something or said "no such object"; anything else (daemon down, rc 127) did not
        answered = bool(found) or "no such" in r.stderr.lower()
        return found, answered

    async def _network_internal(self) -> bool | None:
        """True / False from `docker network inspect`; None when the network is missing or docker did not answer."""
        r = await self._run(["docker", "network", "inspect", SANDBOX_NET, "--format", "{{.Internal}}"],
                            timeout=15.0, max_bytes=4096)
        out = r.stdout.strip().lower()
        return True if r.returncode == 0 and out == "true" else (False if r.returncode == 0 and out == "false" else None)

    async def _connection(self) -> _Conn:
        r = await self._cli("status", timeout=15.0, max_bytes=16_000)
        text = strip_ansi(r.stdout + "\n" + r.stderr)
        kv = dict(KV_RE.findall(text))
        status = kv.get("Status", "")
        m = VERSION_RE.search(kv.get("Version", ""))
        if r.returncode == 127 or (not status and not text.strip()):
            return _Conn(False, False, "the CLI did not run", "", None, "")
        if not status:
            return _Conn(True, False, (text.strip().splitlines() or ["no status line"])[0][:160], "", None, "")
        return _Conn(True, status.lower().startswith("connected"), status[:160], kv.get("Server", "")[:200],
                     m.group(0) if m else None, kv.get("Gateway", "")[:80])

    async def _sandboxes(self, max_age: float = SNAPSHOT_TTL) -> _Snap:
        """`sandbox list -o json`, coalesced: concurrent callers share one subprocess and a max_age-second cache."""
        async with self._snap_lock:
            if time.monotonic() - self._snap.ts < max_age:
                return self._snap
            r = await self._cli("sandbox", "list", "--limit", str(MAX_SANDBOXES), "-o", "json", timeout=20.0)
            rows: list[dict[str, Any]] = []
            ok, error = False, ""
            if r.returncode == 0:
                try:
                    data = json.loads(strip_ansi(r.stdout) or "[]")
                    data = _list(_dict(data).get("sandboxes")) if isinstance(data, dict) else data
                    if isinstance(data, list):
                        for e in data[:MAX_SANDBOXES]:
                            name = _dict(e).get("name")
                            if isinstance(name, str) and NAME_RE.fullmatch(name):
                                rows.append(_dict(e))
                        ok = True
                    else:
                        error = "unexpected JSON shape"
                except ValueError:
                    error = "the list was not JSON"
            else:
                error = redact(strip_ansi(r.stderr or r.stdout).strip())[:200] or f"exit {r.returncode}"
            self._snap = _Snap(time.monotonic(), ok, rows, error)
            if ok:
                self._track(rows)
            return self._snap

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    def _config_problems(self) -> list[Problem]:
        """Hygiene derived from the read-only gateway.toml (the fix is a by-hand edit: ADR 0007)."""
        p = self._path("openshell_gateway_config")
        if p is None:
            return []
        try:
            if p.stat().st_size > MAX_CONFIG_BYTES:
                return []
            drv = _dict(_dict(_dict(tomllib.loads(p.read_text())).get("openshell")).get("drivers")).get("docker")
        except (OSError, tomllib.TOMLDecodeError, UnicodeDecodeError):
            return [Problem(severity="warn", code="openshell.config.unreadable",
                            message=f"{p} is missing or is not valid TOML; the gateway cannot start from it.",
                            fix=f"restore {p} from the lab repository (git -C <lab repo> checkout -- {p})")]
        out: list[Problem] = []
        d = _dict(drv)
        if d.get("image_pull_policy") != "Never":
            out.append(Problem(severity="warn", code="openshell.config.pull_policy",
                               message="gateway.toml does not set image_pull_policy = \"Never\": the Docker daemon could "
                                       "pull images at runtime (egress outside the lab allowlist).",
                               fix=f'set image_pull_policy = "Never" under [openshell.drivers.docker] in {p}'))
        if "@sha256:" not in str(d.get("supervisor_image", "")):
            out.append(Problem(severity="warn", code="openshell.config.supervisor_unpinned",
                               message="The sandbox supervisor image is not pinned by digest in gateway.toml.",
                               fix=f"pin supervisor_image to <image>@sha256:<digest> in {p}"))
        return out

    async def status(self) -> SystemStatus:
        version, (ctrs, docker_ok), internal = await asyncio.gather(
            self._cli_version(), self._inspect(), self._network_internal())
        problems: list[Problem] = []
        components: list[Component] = []
        gw = ctrs.get(self._gateway_ctr)
        gw_running = gw is not None and gw.running
        conn = _Conn(False, False, "not checked: the gateway container is not running", "", None, "")
        if version is not None and (gw_running or not docker_ok):
            conn = await self._connection()
        snap = await self._sandboxes() if conn.connected else _Snap(0.0, False, [], "")

        components.append(Component(kind="process", name="openshell CLI", state="running" if version else "absent",
                                    detail=f"openshell {version}" if version else
                                    "the openshell binary is not on the console's PATH"))
        for name in self.containers:
            c = ctrs.get(name)
            if c is None:
                st: Any = "unknown" if not docker_ok else "absent"
                components.append(Component(kind="container", name=name, state=st,
                                            detail="docker did not answer" if not docker_ok else "container does not exist"))
                continue
            state: Any = "running" if c.running else "failed" if c.failed else "stopped"
            detail = f"{c.state}" + (f", exit {c.exit_code}" if c.state == "exited" else "") + \
                (f", {c.restarts} restart(s)" if c.restarts else "")
            components.append(Component(kind="container", name=name, state=state, image=_short_image(c.image),
                                        detail=detail))
        components.append(Component(
            kind="http", name=f"gateway connection {conn.server or DEFAULT_ENDPOINT}",
            state="running" if conn.connected else ("stopped" if conn.ran else "unknown"), detail=conn.text))

        # problems ------------------------------------------------------------------------------
        if version is None:
            problems.append(Problem(severity="error", code="openshell.cli.missing",
                                    message="The openshell CLI is not installed (or not on the console's PATH).",
                                    fix="install openshell 0.0.71 into ~/.local/bin (see clients/data-designer/README.md "
                                        "and runner/IMPLEMENTATION.md section 2 for the pinned pair)"))
        if not docker_ok:
            problems.append(Problem(severity="warn", code="openshell.docker.unavailable",
                                    message="Docker did not answer, so the gateway containers could not be inspected.",
                                    fix="docker info   # the rootless daemon must be running for the console's user"))
        if internal is False:
            problems.append(Problem(
                severity="error", code="openshell.network.not_internal",
                message=f"Docker network {SANDBOX_NET} is NOT internal: the gateway (it holds the Docker socket) and "
                        "every sandbox supervisor have a route off the box, outside the lab egress allowlist. "
                        "data-designer-local refuses to run until this is fixed.",
                fix=(f"docker compose -f {self._compose or '<runner>/docker-compose.yml'} stop && "
                     f"docker network disconnect {SANDBOX_NET} {LITELLM_CONTAINER}; "
                     f"docker network rm {SANDBOX_NET} && docker network create --internal --subnet "
                     f"{SANDBOX_SUBNET} {SANDBOX_NET}   # data-designer-local re-pins the router on its next run")))
        elif internal is None and docker_ok:
            problems.append(Problem(
                severity="warn", code="openshell.network.missing",
                message=f"Docker network {SANDBOX_NET} does not exist; the gateway cannot start without it.",
                fix=f"docker network create --internal --subnet {SANDBOX_SUBNET} {SANDBOX_NET}"))
        for c in ctrs.values():
            exposed = [ip for ip in c.host_ips if ip not in LOOPBACK]
            if exposed:
                problems.append(Problem(
                    severity="error", code="openshell.port.exposed",
                    message=f"{c.name} publishes a port on {', '.join(sorted(set(exposed)))}: the gateway (mTLS only) "
                            "is reachable beyond this machine. It must be published on 127.0.0.1 only.",
                    fix=f"change the ports: entry of {c.name} to 127.0.0.1:<port>:8080 in "
                        f"{self._compose or 'the runner docker-compose.yml'} and recreate the container"))
        gw_failed = gw is not None and gw.failed
        gw_off = docker_ok and not gw_failed and not gw_running and (gw is None or gw.state in ("exited", "created", "paused"))
        if gw_off and gw is None:
            problems.append(Problem(severity="info", code="openshell.gateway.off",
                                    message="The OpenShell gateway container does not exist yet. It is created on demand "
                                            "by data-designer-local (or the gateway_up action).",
                                    fix=self._start_fix()))
        elif gw_off:
            problems.append(Problem(severity="info", code="openshell.gateway.off",
                                    message="The OpenShell gateway is stopped. That is normal when no Data Designer run "
                                            "needs it: data-designer-local starts it on demand.",
                                    fix=self._start_fix()))
        if gw_failed and gw is not None:
            problems.append(Problem(severity="error", code="openshell.gateway.failed",
                                    message=f"{gw.name} is {gw.state} (exit {gw.exit_code}, {gw.restarts} restart(s)).",
                                    fix=f"docker logs --tail 50 {gw.name}"))
        fwd = [c for n, c in ctrs.items() if n != self._gateway_ctr]
        if gw_running and any(not c.running for c in fwd):
            problems.append(Problem(severity="warn", code="openshell.forwarder.stopped",
                                    message="The CLI forwarder container is not running: the openshell CLI cannot reach "
                                            "the gateway on 127.0.0.1.",
                                    fix=self._start_fix()))
        if gw_running and conn.ran and not conn.connected:
            if "no gateway configured" in conn.text.lower():
                problems.append(Problem(severity="warn", code="openshell.gateway.unregistered",
                                        message=f"The CLI has no gateway registered as {self.gateway_name!r}.",
                                        fix=f"openshell gateway add {DEFAULT_ENDPOINT} --local --name {self.gateway_name}"))
            else:
                problems.append(Problem(severity="warn", code="openshell.gateway.disconnected",
                                        message=f"The gateway container runs but the CLI is not connected ({conn.text}).",
                                        fix=f"openshell -g {self.gateway_name} status   # then: docker logs --tail 50 "
                                            f"{self._gateway_ctr}"))
        if version and conn.version and version != conn.version:
            problems.append(Problem(
                severity="info", code="openshell.version.skew",
                message=f"CLI {version} talks to gateway {conn.version}. This pair is pinned on purpose (the 0.1.x "
                        "gateway needs a newer CLI).",
                fix="no action; bump both together and re-scan (runner/IMPLEMENTATION.md section 2)"))
        problems += self._config_problems()

        # health ----------------------------------------------------------------------------------
        if version is None or gw_failed:
            health = "down"
        elif gw_off:
            health = "off"
        elif gw_running or not docker_ok:
            degraded = (not conn.connected or internal is False or any(p.severity == "error" for p in problems)
                        or any(not c.running for c in fwd))
            health = "unknown" if (not docker_ok and not conn.ran) else ("degraded" if degraded else "up")
        else:
            health = "unknown"
        gver = conn.version
        if conn.connected:
            summary = f"connected · gateway {gver or '?'} · {len(snap.rows)} sandbox(es)" if snap.ok else \
                f"connected · gateway {gver or '?'} · sandbox list unavailable"
        elif health == "off":
            summary = "gateway stopped (starts on demand with Data Designer runs)"
        else:
            summary = f"gateway {conn.text}"[:120]
        if version:
            summary += f" · CLI {version}"
        return SystemStatus(
            system="openshell", health=health, version=gver or version, summary=summary[:300],
            endpoints=[Endpoint(name="gateway (mTLS, CLI only)", url=_loopback(conn.server), role="api", auth="none")],
            components=components, problems=problems, checked_at=time.time())

    async def metrics(self) -> MetricsSnapshot:
        now = time.time()
        version = await self._cli_version()
        if version is None:
            return MetricsSnapshot(system="openshell", ts=now, available=False, error="the openshell CLI is not installed")
        (ctrs, docker_ok), internal = await asyncio.gather(self._inspect(), self._network_internal())
        gw = ctrs.get(self._gateway_ctr)
        points: list[MetricPoint] = []
        conn: _Conn | None = None
        if (gw is not None and gw.running) or not docker_ok:
            conn = await self._connection()
        if conn is not None and conn.ran:
            points.append(MetricPoint(key="os.gateway_up", value=1.0 if conn.connected else 0.0, unit="1"))
        elif docker_ok:
            points.append(MetricPoint(key="os.gateway_up", value=0.0, unit="1"))
        if internal is not None:
            points.append(MetricPoint(key="os.network_internal", value=1.0 if internal else 0.0, unit="1"))
        if gw is not None and gw.running:
            started = _iso(gw.started)
            if started is not None:
                points.append(MetricPoint(key="os.gateway_uptime_s", value=max(0.0, now - started), unit="s"))
            points.append(MetricPoint(key="os.gateway_restarts", value=float(gw.restarts), unit="1"))
        if conn is not None and conn.connected:
            snap = await self._sandboxes()
            if snap.ok:
                points.append(MetricPoint(key="os.sandboxes", value=float(len(snap.rows)), unit="1"))
                counts = dict.fromkeys(PHASES, 0)
                for row in snap.rows:
                    counts[phase_bucket(str(row.get("phase", "")))] += 1
                for ph in PHASES:
                    points.append(MetricPoint(key="os.sandboxes_by_phase", value=float(counts[ph]), unit="1",
                                              labels={"phase": ph}))
                ages = [now - t for t in (_utc(r.get("created_at")) for r in snap.rows) if t is not None]
                if ages:
                    points.append(MetricPoint(key="os.sandbox_oldest_age_s", value=max(0.0, max(ages)), unit="s"))
            r = await self._cli("provider", "list", "-o", "json", timeout=15.0, max_bytes=64_000)
            if r.returncode == 0:
                with contextlib.suppress(ValueError):
                    provs = json.loads(strip_ansi(r.stdout) or "[]")
                    if isinstance(provs, list):
                        points.append(MetricPoint(key="os.providers", value=float(len(provs)), unit="1"))
        return MetricsSnapshot(system="openshell", ts=now, available=True, points=points)

    # ── events: diffs of the sandbox list ───────────────────────────────────────────────────
    def _emit(self, ts: float, summary: str, data: dict[str, Any]) -> None:
        self._seq += 1
        self._ring.append((self._seq, SystemEvent(ts=ts, system="openshell", kind=EVENT_KIND,
                                                  summary=redact(summary)[:300], data=data)))

    def _track(self, rows: list[dict[str, Any]]) -> None:
        """Compare a SUCCESSFUL list with the last one. A failed list never reaches here, so an unreachable
        gateway cannot look like 'every sandbox was deleted'."""
        now = time.time()
        current: dict[str, tuple[str, str]] = {}
        by_name: dict[str, dict[str, Any]] = {}
        for row in rows:
            name = str(row["name"])
            current[name] = (str(row.get("id", ""))[:64], str(row.get("phase", ""))[:24])
            by_name[name] = row
        first = not self._baselined
        for name, (sid, phase) in current.items():
            old = self._known.get(name)
            created = _utc(by_name[name].get("created_at")) or now
            base: dict[str, Any] = {"name": name, "id": sid, "phase": phase}
            if old is None or old[0] != sid:
                action = "present" if first else "created"
                self._emit(created if first else now, f"sandbox {name} {action} ({phase})",
                           {**base, "action": action, "previous_phase": None})
            elif old[1] != phase:
                self._emit(now, f"sandbox {name}: {old[1]} -> {phase}",
                           {**base, "action": "phase", "previous_phase": old[1]})
        for name, (sid, phase) in self._known.items():
            if name not in current:
                self._emit(now, f"sandbox {name} deleted (was {phase})",
                           {"name": name, "id": sid, "phase": phase, "action": "deleted", "previous_phase": phase})
        self._known = current
        self._baselined = True

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        await self._observe()
        return [ev for _, ev in reversed(self._ring)][: max(limit, 0)]

    async def _observe(self) -> None:
        if await self._gateway_running():
            await self._sandboxes()

    async def _gateway_running(self) -> bool:
        """Cached for 10 s so the events poll does not run `docker inspect` every few seconds."""
        ts, running = self._gw_cache
        if time.monotonic() - ts < 10.0:
            return running
        ctrs, docker_ok = await self._inspect()
        gw = ctrs.get(self._gateway_ctr)
        running = gw.running if gw is not None else not docker_ok
        self._gw_cache = (time.monotonic(), running)
        return running

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        # Cursor = ring sequence number. With since_ts, replay buffered events newer than it first; without it,
        # start at "now". Memory is the shared ring (RING events) plus one integer per consumer.
        await self._observe()
        cursor = self._seq if since_ts is None else 0
        while True:
            for seq, ev in list(self._ring):
                if seq > cursor:
                    cursor = seq
                    if since_ts is None or ev.ts > since_ts:
                        yield ev
            await asyncio.sleep(self.follow_interval)
            await self._observe()

    # ── configure: trust files are READ-ONLY (ADR 0007) ─────────────────────────────────────
    def _files(self) -> dict[str, tuple[str, Path, Literal["toml", "yaml"], str]]:
        """id -> (label, path, format, description) for every configured trust file."""
        out: dict[str, tuple[str, Path, Literal["toml", "yaml"], str]] = {}
        gwc, pol, comp, rd = (self._path("openshell_gateway_config"), self._path("openshell_policy"), self._compose,
                              self._runner_dir)
        if gwc is not None:
            out["gateway"] = ("OpenShell gateway config (gateway.toml)", gwc, "toml",
                              ("Schema-v1 gateway config: compute driver, host_gateway_ip, the digest-pinned supervisor "
                               "image and image_pull_policy = \"Never\"."))
        if pol is not None:
            out["policy"] = ("Sandbox policy (policy.yaml)", pol, "yaml",
                             ("The Landlock filesystem allowlist and the default-deny network policy attached to every "
                              "Data Designer sandbox at create time."))
        if rd is not None:
            out["provider-profile"] = ("Provider profile (profile-lab-router.yaml)", rd / PROFILE_NAME, "yaml",
                                       ("Declares the router credential by ENV NAME only, its endpoint binding and the "
                                        "binaries that may use it. The key itself lives in the gateway's store."))
        if comp is not None:
            out["compose"] = ("Gateway compose stack (docker-compose.yml)", comp, "yaml",
                              ("The gateway and CLI-forwarder containers: cap_drop, no-new-privileges, the 127.0.0.1-only "
                               "published port and the internal sandbox network."))
        return out

    def _why_readonly(self) -> str:
        return ("Trust and containment policy for the sandboxes that run untrusted dataset scripts. Weakening it is a "
                "security decision (ADR 0007), so the console only shows it; edit the file by hand in the lab "
                "repository, review the diff, and use the prove_policy action to check the result.")

    def config_files(self) -> list[ConfigFileSpec]:
        return [ConfigFileSpec(id=i, label=lbl, description=desc, display_path=_display(p), format=fmt,
                               editable=False, why_readonly=self._why_readonly(),
                               validators=["TOML parse" if fmt == "toml" else "YAML parse"], authoritative=False,
                               apply_effect="read-only: nothing to apply")
                for i, (lbl, p, fmt, desc) in self._files().items()]

    def config_path(self, file_id: str) -> Path:
        entry = self._files().get(file_id)
        rd = self._runner_dir
        if entry is None:
            raise KeyError(file_id)
        raw = entry[1]
        # containment: the real file must stay inside the runner directory (no symlink out of it)
        if rd is not None and not raw.resolve().is_relative_to(rd.resolve()):
            raise KeyError(file_id)
        return raw

    async def read_config(self, file_id: str) -> ConfigFile:
        p = self.config_path(file_id)
        spec = next(s for s in self.config_files() if s.id == file_id)

        def read() -> tuple[bytes, float]:
            st = p.stat()
            if st.st_size > MAX_CONFIG_BYTES:
                raise FileNotFoundError(file_id)
            return p.read_bytes(), st.st_mtime

        try:
            raw, mtime = await asyncio.to_thread(read)
        except OSError:
            raise FileNotFoundError(file_id) from None
        return ConfigFile(spec=spec, content=redact(raw.decode("utf-8", "replace")),
                          sha256=hashlib.sha256(raw).hexdigest(), mtime=mtime)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        entry = self._files().get(file_id)
        if entry is None:
            raise KeyError(file_id)
        diags: list[Diagnostic] = []
        try:
            if entry[2] == "toml":
                tomllib.loads(content)
            else:
                yaml.safe_load(content)
        except (tomllib.TOMLDecodeError, yaml.YAMLError) as e:
            diags.append(Diagnostic(severity="error", message=f"{entry[2].upper()} syntax error: {str(e)[:300]}"))
        return ValidationResult(ok=not diags, validator=f"{entry[2].upper()} parse", authoritative=False, diagnostics=diags,
                                note="Read-only in the console (ADR 0007); this parse only reviews a proposed diff. "
                                     "It does not run `openshell policy prove` or start a sandbox.")

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        if file_id not in self._files():
            raise KeyError(file_id)
        return ApplyResult(ok=False, post_check="skipped", detail="read-only: the console never writes OpenShell trust files")

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    def actions(self) -> list[ActionSpec]:
        return [
            ActionSpec(id="doctor", label="Run doctor", danger="safe",
                       description="`openshell doctor check`: validates the local prerequisites for a gateway (Docker "
                                   "reachable, DOCKER_HOST). Read-only."),
            ActionSpec(id="list_sandboxes", label="List sandboxes", danger="safe",
                       description="`openshell sandbox list`: name, phase and creation time of every sandbox on the "
                                   "gateway. Data Designer creates one per run and deletes it afterwards."),
            ActionSpec(id="sandbox_get", label="Show a sandbox", danger="safe",
                       description="`openshell sandbox get NAME`: phase and the active policy of one sandbox.",
                       params=[ParamSpec(key="name", label="Sandbox name", type="string", requires_restart=False,
                                         help="A name from the sandbox list.")]),
            ActionSpec(id="sandbox_logs", label="Sandbox logs", danger="safe",
                       description="`openshell logs NAME -n N`: the supervisor and gateway log lines of one sandbox "
                                   "(OCSF events: SSH opens, Landlock, policy decisions). Redacted.",
                       params=[ParamSpec(key="name", label="Sandbox name", type="string", requires_restart=False,
                                         help="A name from the sandbox list."),
                               ParamSpec(key="lines", label="Lines", type="int", default=100, min=1, max=500,
                                         requires_restart=False, help="Newest lines to return (1-500).")]),
            ActionSpec(id="gateway_logs", label="Gateway container logs", danger="safe",
                       description="`docker logs --tail N` of the gateway container. Redacted (tokens, JWTs).",
                       params=[ParamSpec(key="lines", label="Lines", type="int", default=100, min=1, max=500,
                                         requires_restart=False, help="Newest lines to return (1-500).")]),
            ActionSpec(id="prove_policy", label="Prove sandbox policy", danger="safe",
                       description="`openshell policy prove --policy policy.yaml --credentials <descriptor>`: the formal "
                                   "prover reports what the policy lets the credentialed binary do. The prover cannot run "
                                   "without a credential descriptor, so one is derived from the provider profile "
                                   "(credential name, auth style, the ENV NAME it is injected through, the endpoint "
                                   "hosts) plus the scopes you declare below; no key is read. Findings are items for a "
                                   "human to review, not failures.",
                       params=[ParamSpec(key="scopes", label="Declared scopes", type="string", requires_restart=False,
                                         help="Comma-separated scopes the key is MEANT to have (e.g. read). Empty = none "
                                              "declared. Anything the policy allows beyond them is reported."),
                               ParamSpec(key="compact", label="Compact output", type="bool", default=True,
                                         requires_restart=False, help="One line per finding.")]),
            ActionSpec(id="gateway_up", label="Start the gateway", danger="disruptive",
                       description="Mirrors data-designer-local's ensure_gateway(): refuses unless openshell-docker is an "
                                   "INTERNAL network, runs `docker compose -f runner/docker-compose.yml up -d`, re-pins "
                                   "the router at 172.16.0.5 on that network, then waits up to 90 s for `openshell status` "
                                   "to say Connected."),
            ActionSpec(id="gateway_stop", label="Stop the gateway", danger="disruptive",
                       description="`docker compose -f runner/docker-compose.yml stop` (never `down -v`): stops the gateway "
                                   "and the CLI forwarder, keeping the containers and the gateway's state. Refused while "
                                   "sandboxes exist unless `force` is set: a stopped gateway orphans a running Data "
                                   "Designer run.",
                       params=[ParamSpec(key="force", label="Stop even with sandboxes", type="bool", default=False,
                                         requires_restart=False, help="Stop although sandboxes exist.")]),
            ActionSpec(id="delete_sandbox", label="Delete a sandbox", danger="destructive",
                       description="`openshell sandbox delete NAME`: destroys the sandbox container and its workspace. A "
                                   "Data Designer run that is using it fails. The name must be in the live sandbox list.",
                       params=[ParamSpec(key="name", label="Sandbox name", type="string", requires_restart=False,
                                         help="A name from the sandbox list.")]),
        ]

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        t0 = time.monotonic()

        def done(ok: bool, out: str, problems: list[Problem] | None = None) -> ActionResult:
            return ActionResult(ok=ok, output=redact(strip_ansi(out))[:20_000],
                                duration_ms=int((time.monotonic() - t0) * 1000), problems=problems or [])

        handlers = {
            "doctor": self._doctor, "list_sandboxes": self._list_action, "sandbox_get": self._sandbox_get,
            "sandbox_logs": self._sandbox_logs, "gateway_logs": self._gateway_logs, "prove_policy": self._prove,
            "gateway_up": self._gateway_up, "gateway_stop": self._gateway_stop, "delete_sandbox": self._delete,
        }
        handler = handlers.get(action_id)
        if handler is None:
            raise KeyError(action_id)
        if action_id not in ("gateway_logs", "gateway_up", "gateway_stop") and await self._cli_version() is None:
            return done(False, "The openshell CLI is not installed.", [Problem(
                severity="error", code="openshell.cli.missing", message="The openshell CLI is not on the console's PATH.",
                fix="install openshell 0.0.71 into ~/.local/bin (runner/IMPLEMENTATION.md section 2)")])
        return await handler(params, done)

    async def _doctor(self, params: dict[str, object], done: Done) -> ActionResult:
        r = await self._run([CLI, "doctor", "check"], timeout=60.0, max_bytes=64_000)
        return done(r.returncode == 0, (r.stdout + ("\n" + r.stderr if r.stderr.strip() else "")).strip())

    @staticmethod
    def _table(rows: list[dict[str, Any]]) -> str:
        if not rows:
            return "No sandboxes found."
        lines = [f"{'NAME':<40} {'PHASE':<14} {'CREATED (UTC)':<20} ID"]
        lines += [f"{str(r.get('name'))[:40]:<40} {str(r.get('phase', '?'))[:14]:<14} "
                  f"{str(r.get('created_at', '?'))[:20]:<20} {str(r.get('id', ''))[:36]}" for r in rows]
        return "\n".join(lines)

    async def _list_action(self, params: dict[str, object], done: Done) -> ActionResult:
        snap = await self._sandboxes(max_age=0.0)
        if not snap.ok:
            return done(False, f"The sandbox list is unavailable: {snap.error}", [self._unreachable_problem()])
        return done(True, self._table(snap.rows))

    def _unreachable_problem(self) -> Problem:
        return Problem(severity="warn", code="openshell.gateway.unreachable",
                       message="The gateway did not answer the CLI (it is probably stopped).", fix=self._start_fix())

    async def _live_name(self, params: dict[str, object]) -> tuple[str | None, str]:
        """(name, error): the name must look like a sandbox name AND be in the live list."""
        name = params.get("name")
        if not isinstance(name, str) or NAME_RE.fullmatch(name) is None:
            return None, "name must be a sandbox name (letters, digits and . _ -, starting with a letter or digit)"
        snap = await self._sandboxes(max_age=0.0)
        if not snap.ok:
            return None, f"The sandbox list is unavailable: {snap.error}"
        if name not in {str(r["name"]) for r in snap.rows}:
            return None, f"no sandbox named {name!r} (live: {', '.join(str(r['name']) for r in snap.rows[:20]) or 'none'})"
        return name, ""

    async def _sandbox_get(self, params: dict[str, object], done: Done) -> ActionResult:
        name, err = await self._live_name(params)
        if name is None:
            return done(False, err)
        r = await self._cli("sandbox", "get", name, timeout=30.0)
        return done(r.returncode == 0, (r.stdout + ("\n" + r.stderr if r.stderr.strip() else "")).strip())

    async def _sandbox_logs(self, params: dict[str, object], done: Done) -> ActionResult:
        name, err = await self._live_name(params)
        if name is None:
            return done(False, err)
        n = _int_param(params.get("lines", 100), 1, 500)
        if n is None:
            return done(False, "lines must be an integer from 1 to 500")
        r = await self._cli("logs", name, "-n", str(n), timeout=30.0, max_bytes=512_000)
        text = (r.stdout + ("\n" + r.stderr if r.stderr.strip() else "")).strip()
        return done(r.returncode == 0, text[-19_000:])

    async def _gateway_logs(self, params: dict[str, object], done: Done) -> ActionResult:
        n = _int_param(params.get("lines", 100), 1, 500)
        if n is None:
            return done(False, "lines must be an integer from 1 to 500")
        r = await self._run(["docker", "logs", "--tail", str(n), self._gateway_ctr], timeout=30.0, max_bytes=512_000)
        text = (r.stdout + ("\n" + r.stderr if r.stderr.strip() else "")).strip()    # docker logs mixes both streams
        return done(r.returncode == 0, text[-19_000:] or "(no log lines)")

    async def _delete(self, params: dict[str, object], done: Done) -> ActionResult:
        name, err = await self._live_name(params)
        if name is None:
            return done(False, err)
        r = await self._cli("sandbox", "delete", name, timeout=60.0)
        self._snap = _Snap(0.0, False, [], "invalidated")                    # next read re-lists
        return done(r.returncode == 0, (r.stdout + ("\n" + r.stderr if r.stderr.strip() else "")).strip()
                    or f"deleted {name}")

    # prove ---------------------------------------------------------------------------------
    def _descriptor(self, scopes: list[str]) -> tuple[str | None, str]:
        """The prover's credential descriptor derived from the provider profile: (yaml text, error)."""
        rd = self._runner_dir
        if rd is None:
            return None, "no OpenShell runner directory is configured"
        prof = rd / PROFILE_NAME
        try:
            if prof.is_symlink() or prof.stat().st_size > MAX_CONFIG_BYTES:
                return None, f"{prof} is a symlink or too large"
            data = _dict(yaml.safe_load(prof.read_text()))
        except (OSError, yaml.YAMLError):
            return None, f"{prof} is missing or is not valid YAML"
        hosts = [str(e["host"]) for e in _list(data.get("endpoints")) if isinstance(e, dict) and e.get("host")]
        creds: list[dict[str, Any]] = []
        for c in _list(data.get("credentials")):
            c = _dict(c)
            envs = [str(v) for v in _list(c.get("env_vars"))]
            if not c.get("name") or not envs:
                continue
            creds.append({"name": str(c["name"]), "type": str(c.get("auth_style") or "bearer"), "scopes": scopes,
                          "injected_via": envs[0], "target_hosts": hosts})
        if not creds:
            return None, f"{prof} declares no credential with env_vars"
        return yaml.safe_dump({"credentials": creds}, sort_keys=False), ""

    async def _prove(self, params: dict[str, object], done: Done) -> ActionResult:
        policy = self._path("openshell_policy")
        if policy is None or not policy.is_file():
            return done(False, f"policy file not found: {policy}")
        raw = str(params.get("scopes") or "")
        scopes = [s.strip() for s in raw.split(",") if s.strip()]
        if len(scopes) > 16 or any(SCOPE_RE.fullmatch(s) is None for s in scopes):
            return done(False, "scopes must be a comma-separated list of lowercase words (letters, digits, . _ : -)")
        compact = params.get("compact", True)
        text, err = self._descriptor(scopes)
        if text is None:
            return done(False, err)
        tmp_dir = self.ctx.state_dir / "openshell" / "tmp"
        await asyncio.to_thread(tmp_dir.mkdir, 0o700, True, True)
        tmp = tmp_dir / f"credentials.{os.urandom(6).hex()}.yaml"
        try:
            fd = await asyncio.to_thread(os.open, tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "w") as fh:
                fh.write(text)
            argv = [CLI, "policy", "prove", "--policy", str(policy), "--credentials", str(tmp)]
            if compact is not False:
                argv.append("--compact")
            r = await self._run(argv, timeout=60.0, max_bytes=256_000)
        finally:
            with contextlib.suppress(OSError):
                tmp.unlink(missing_ok=True)
        out = strip_ansi(r.stdout + ("\n" + r.stderr if r.stderr.strip() else "")).strip()
        header = f"Policy: {policy.name} · derived credential descriptor ({PROFILE_NAME}; scopes: {', '.join(scopes) or 'none'})\n\n"
        if r.timed_out or r.returncode == 127 or re.search(r"^\s*Error:", out, re.MULTILINE):
            return done(False, header + out)
        if r.returncode == 0:
            return done(True, header + out)
        n = len(re.findall(r"^\s*•", out, re.MULTILINE))
        return done(True, header + out, [Problem(
            severity="warn", code="openshell.policy.findings",
            message=f"The prover reports {n or 'some'} finding(s) for human review (exit {r.returncode}).",
            fix="read them, then change policy.yaml by hand if a finding is not intended")])

    # gateway lifecycle ---------------------------------------------------------------------
    async def _gateway_up(self, params: dict[str, object], done: Done) -> ActionResult:
        compose = self._compose
        if compose is None or not compose.is_file():
            return done(False, f"compose file not found: {compose}")
        if await self._network_internal() is not True:
            return done(False, f"refusing to start: docker network {SANDBOX_NET} is missing or NOT internal (it would "
                               "give the gateway, which holds the Docker socket, a route off the box).", [Problem(
                severity="error", code="openshell.network.not_internal",
                message=f"{SANDBOX_NET} is missing or not internal.",
                fix=f"docker network create --internal --subnet {SANDBOX_SUBNET} {SANDBOX_NET}")])
        lines: list[str] = []
        up = await self._run(["docker", "compose", "-f", str(compose), "up", "-d"], timeout=150.0, cwd=compose.parent)
        lines.append((up.stdout + "\n" + up.stderr).strip())
        if up.returncode != 0:
            return done(False, "gateway compose failed:\n" + "\n".join(lines))
        lines.append(await self._pin_router())
        deadline = time.monotonic() + self.ready_timeout
        while True:
            conn = await self._connection()
            if conn.connected:
                lines.append(f"openshell status: {conn.text} (gateway {conn.version or '?'})")
                return done(True, "\n".join(lines))
            if time.monotonic() >= deadline:
                break
            await asyncio.sleep(self.poll_interval)
        lines.append(f"the gateway did not become reachable within {self.ready_timeout:.0f} s ({conn.text})")
        return done(False, "\n".join(lines), [Problem(severity="warn", code="openshell.gateway.disconnected",
                                                      message="The containers started but the CLI is not connected.",
                                                      fix=f"docker logs --tail 50 {self._gateway_ctr}")])

    async def _pin_router(self) -> str:
        """Re-pin the router at LITELLM_IP on openshell-docker (the profile/policy endpoint), as ensure_gateway() does."""
        insp = await self._run(["docker", "inspect", LITELLM_CONTAINER, "--format",
                                "{{range $k,$v := .NetworkSettings.Networks}}{{$k}}={{$v.IPAddress}} {{end}}"],
                               timeout=30.0, max_bytes=16_000)
        if insp.returncode != 0:
            return f"router container {LITELLM_CONTAINER} not found: sandboxes cannot reach the router until it runs"
        if f"{SANDBOX_NET}={LITELLM_IP}" in insp.stdout:
            return f"router already pinned at {LITELLM_IP} on {SANDBOX_NET}"
        await self._run(["docker", "network", "disconnect", SANDBOX_NET, LITELLM_CONTAINER], timeout=30.0)
        c = await self._run(["docker", "network", "connect", "--ip", LITELLM_IP, "--alias", "litellm", SANDBOX_NET,
                             LITELLM_CONTAINER], timeout=30.0)
        return (f"router pinned at {LITELLM_IP} on {SANDBOX_NET}" if c.returncode == 0 else
                f"could not attach the router to {SANDBOX_NET}: {c.stderr.strip()[:200]}")

    async def _gateway_stop(self, params: dict[str, object], done: Done) -> ActionResult:
        compose = self._compose
        if compose is None or not compose.is_file():
            return done(False, f"compose file not found: {compose}")
        snap = await self._sandboxes(max_age=0.0)
        if snap.ok and snap.rows and params.get("force") is not True:
            names = ", ".join(str(r["name"]) for r in snap.rows[:10])
            return done(False, f"{len(snap.rows)} sandbox(es) exist ({names}). Stopping the gateway would orphan a "
                               "running Data Designer run. Wait for it, delete the sandboxes, or set force.", [Problem(
                severity="warn", code="openshell.stop.sandboxes_exist",
                message="Sandboxes exist; the gateway was not stopped.", fix="run again with force enabled")])
        r = await self._run(["docker", "compose", "-f", str(compose), "stop"], timeout=120.0, cwd=compose.parent)
        self._snap = _Snap(0.0, False, [], "invalidated")
        return done(r.returncode == 0, (r.stdout + "\n" + r.stderr).strip() or "stopped")


def _int_param(v: object, lo: int, hi: int) -> int | None:
    return v if isinstance(v, int) and not isinstance(v, bool) and lo <= v <= hi else None


def _short_image(image: str) -> str:
    """repo@sha256:<64 hex> -> repo@sha256:<12 hex> (display)."""
    if "@sha256:" in image:
        repo, digest = image.split("@sha256:", 1)
        return f"{repo}@sha256:{digest[:12]}"
    return image[:200]


def _iso(stamp: str) -> float | None:
    """Docker StartedAt ("2026-10-24T07:47:51.919656462Z") -> epoch seconds; the zero time -> None."""
    try:
        t = datetime.strptime(stamp[:19], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=UTC).timestamp()
    except ValueError:
        return None
    return t if t > 0 else None


def _loopback(server: str) -> str:
    """The gateway URL as shown: the CLI's own Server line when it is a loopback https URL, else the default."""
    m = re.fullmatch(r"https?://(127\.0\.0\.1|localhost|\[::1\]):\d{1,5}/?", server.strip())
    return server.strip().rstrip("/") if m else DEFAULT_ENDPOINT


def _display(p: Path) -> str:
    home = Path.home()
    try:
        return "~/" + str(p.relative_to(home))
    except ValueError:
        return str(p)
