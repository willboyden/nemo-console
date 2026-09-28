"""Composition root. Everything is built here so tests can inject fake adapters / runner / http."""
from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass, field

import httpx

from nemo_console.adapters import SYSTEM_IDS, build_adapter
from nemo_console.adapters.base import AdapterContext, CommandRunner, SystemAdapter
from nemo_console.config import Settings
from nemo_console.domain.models import Problem, SystemId, SystemStatus
from nemo_console.services.audit import AuditLog, DenialLog
from nemo_console.services.auth import AuthService
from nemo_console.services.config_store import ConfigStore
from nemo_console.services.engines import EngineProbe, capable_tier_problems
from nemo_console.services.events import SseLimiter
from nemo_console.services.metrics import MetricsPoller
from nemo_console.services.missing import MissingAdapter
from nemo_console.services.runner import SubprocessRunner
from nemo_console.services.settings import SettingsService
from nemo_console.services.status import StatusService
from nemo_console.services.store import Store
from nemo_console.services.telemetry import Telemetry
from nemo_console.services.topology import TopologyService

log = logging.getLogger(__name__)


def make_http() -> httpx.AsyncClient:
    # trust_env=False: no proxy env / netrc; redirects are never followed (no cross-host hops)
    return httpx.AsyncClient(trust_env=False, follow_redirects=False, timeout=httpx.Timeout(10.0, connect=3.0))


@dataclass
class Container:
    cfg: Settings
    store: Store
    auth: AuthService
    audit: AuditLog
    runner: CommandRunner
    http: httpx.AsyncClient
    adapters: dict[SystemId, SystemAdapter]
    settings: SettingsService
    status: StatusService
    metrics: MetricsPoller
    config_store: ConfigStore
    topology: TopologyService
    sse: SseLimiter
    telemetry: Telemetry
    denials: DenialLog = field(default_factory=DenialLog)
    trust_notice: str | None = None
    validate_sem: asyncio.Semaphore = field(default_factory=lambda: asyncio.Semaphore(2))
    busy: dict[SystemId, asyncio.Lock] = field(default_factory=dict)

    def lock(self, system: SystemId) -> asyncio.Lock:
        return self.busy.setdefault(system, asyncio.Lock())


def build_container(cfg: Settings, *, adapters: dict[SystemId, SystemAdapter] | None = None,
                    runner: CommandRunner | None = None, http: httpx.AsyncClient | None = None) -> Container:
    store = Store(cfg.state_dir)
    trust, trust_notice = cfg.effective_loopback_trust()
    if trust_notice:
        log.warning("%s", trust_notice)
    settings = SettingsService(cfg, store)
    runner = runner or SubprocessRunner(cfg.extra_bin_dirs)
    http = http or make_http()
    ctx = AdapterContext(runner=runner, http=http, urls=settings.effective_urls(), paths=cfg.resolved_paths(),
                         secret_env=settings.effective_key_env(), state_dir=cfg.state_dir)
    built: dict[SystemId, SystemAdapter] = dict(adapters or {})
    for sid in SYSTEM_IDS:
        if sid in built:
            continue
        try:
            built[sid] = build_adapter(sid, ctx)
        except Exception as e:  # noqa: BLE001 - a missing/broken adapter degrades that system only
            log.warning("adapter %s unavailable: %s", sid, type(e).__name__)
            built[sid] = MissingAdapter(sid, ctx, f"{type(e).__name__}: {e}")
    ordered = {sid: built[sid] for sid in SYSTEM_IDS}
    root_problem = cfg.repo_root_problem()
    global_problems = [Problem(severity="error", code="console.repo_root.missing", message=root_problem,
                               fix="export NEMO_CONSOLE_REPO_ROOT=/path/to/lab-repo and restart nemo-console")] if root_problem else []
    probe = EngineProbe(runner)
    validate_sem = asyncio.Semaphore(2)
    config_store = ConfigStore(store, ordered, cfg.config_cap_bytes, cfg.revisions_keep, validate_sem)

    async def enrich_switchyard(st: SystemStatus) -> SystemStatus:
        try:
            routes = ordered["switchyard"].config_path("routes")
            text = routes.read_text() if routes.stat().st_size <= cfg.config_cap_bytes else ""
        except (KeyError, OSError, UnicodeDecodeError):
            return st
        problems = await capable_tier_problems(probe, cfg.engine_probes, text, cfg.repo_root, cfg.engine_start)
        if not problems:
            return st
        health = "degraded" if st.health == "up" else st.health
        return st.model_copy(update={"health": health, "summary": f"capable tier dark: {st.summary}",
                                     "problems": [*st.problems, *problems]})

    status = StatusService(ordered, cfg.status_ttl_s, cfg.adapter_timeout_s, global_problems,
                           {"switchyard": enrich_switchyard})
    telemetry = Telemetry()
    telemetry.setup_otlp(cfg.otlp_endpoint)
    return Container(
        cfg=cfg, store=store, auth=AuthService(store, cfg.state_dir, trust), audit=AuditLog(store),
        runner=runner, http=http, adapters=ordered, settings=settings, status=status,
        metrics=MetricsPoller(status, store, settings.effective_poll_interval(), cfg.ring_seconds),
        config_store=config_store,
        topology=TopologyService(status, http, probe, settings.effective_urls().get("litellm", "http://127.0.0.1:4000")),
        sse=SseLimiter(cfg.sse_max_connections), telemetry=telemetry, trust_notice=trust_notice,
        validate_sem=validate_sem)
