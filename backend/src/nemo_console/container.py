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
from nemo_console.services.engines import (
    CapableProbe,
    EngineProbe,
    capable_targets,
    capable_tier_problems,
    probe_problems,
)
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
    capable_probe: CapableProbe | None = None          # addendum 9.11, None unless Settings.capable_probe
    busy: dict[SystemId, asyncio.Lock] = field(default_factory=dict)

    def lock(self, system: SystemId) -> asyncio.Lock:
        return self.busy.setdefault(system, asyncio.Lock())


def _configure_switchyard(adapter: SystemAdapter, cfg: Settings) -> None:
    """Addendum 9.11: the Switchyard container, compose project/service, fallback image and in-container log path."""
    from nemo_console.adapters.switchyard import SwitchyardAdapter
    if isinstance(adapter, SwitchyardAdapter):
        adapter.container = cfg.switchyard_container
        adapter.compose_project = cfg.switchyard_compose_project
        adapter.compose_service = cfg.switchyard_compose_service
        adapter.fallback_image = cfg.switchyard_image
        adapter.log_path = cfg.switchyard_log_path


def _configure_relay(adapter: SystemAdapter, cfg: Settings) -> None:
    """Addendum 9.11: the Relay gateway's systemd --user unit."""
    from nemo_console.adapters.relay import RelayAdapter
    if isinstance(adapter, RelayAdapter):
        adapter.unit = cfg.relay_unit


def _configure_dynamo(adapter: SystemAdapter, cfg: Settings) -> None:
    """Hand the Dynamo card settings (`dynamo_*`, addendum 9.4) and its names (addendum 9.11) to the registry-built
    adapter. Imported lazily so a broken adapter module still degrades to MissingAdapter; injected adapters (tests)
    keep their own policy."""
    from nemo_console.adapters.dynamo import CardPolicy, DynamoAdapter, DynamoNames
    if isinstance(adapter, DynamoAdapter):
        svc, ctr = cfg.dynamo_services, cfg.dynamo_containers
        adapter.names = DynamoNames(
            project=cfg.dynamo_compose_project, frontend=ctr["frontend"], worker=ctr["worker"], mocker=ctr["mocker"],
            sv_frontend=ctr["sv_frontend"], svc_frontend=svc["frontend"], svc_worker=svc["worker"],
            svc_mocker=svc["mocker"], svc_sv_frontend=svc["sv_frontend"])
        adapter.policy = CardPolicy(
            opt_in=cfg.dynamo_opt_in, start=cfg.dynamo_start, restore=cfg.default_model_start,
            exclusive=tuple(cfg.dynamo_exclusive_engines), coexisting=tuple(cfg.dynamo_coexisting_engines),
            ignored=tuple(cfg.dynamo_ignored_containers), dark=tuple(cfg.dynamo_dark_while_held),
            own=adapter.names.all)


def _configure_openshell(adapter: SystemAdapter, cfg: Settings) -> None:
    """Addendum 9.8: hand the OpenShell container names (`openshell_containers`) to the registry-built adapter."""
    from nemo_console.adapters.openshell import OpenShellAdapter
    if isinstance(adapter, OpenShellAdapter):
        adapter.containers = tuple(cfg.openshell_containers)


def _configure_guardrails(adapter: SystemAdapter, cfg: Settings) -> None:
    """Addendum 9.8: hand the NeMo Guardrails server container name (`guardrails_container`) to the adapter; its
    gateway and router-broker siblings are derived from it (`<name>-gw`, `<name>-router-broker`)."""
    from nemo_console.adapters.guardrails import GuardrailsAdapter
    if isinstance(adapter, GuardrailsAdapter):
        adapter.container = cfg.guardrails_container
        adapter.compose_project = cfg.guardrails_compose_project


def _configure_dcgm(adapter: SystemAdapter, cfg: Settings) -> None:
    """Addendum 9.8: hand the DCGM settings (exporter container, diag image, Prometheus jobs) to the adapter."""
    from nemo_console.adapters.dcgm import DcgmAdapter
    if isinstance(adapter, DcgmAdapter):
        adapter.configure(exporter_container=cfg.dcgm_exporter_container, diag_image=cfg.dcgm_diag_image,
                          prometheus_jobs=cfg.dcgm_prometheus_jobs, compose_project=cfg.dcgm_compose_project)


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
            if sid == "switchyard":
                _configure_switchyard(built[sid], cfg)
            elif sid == "relay":
                _configure_relay(built[sid], cfg)
            elif sid == "dynamo":
                _configure_dynamo(built[sid], cfg)
            elif sid == "openshell":
                _configure_openshell(built[sid], cfg)
            elif sid == "guardrails":
                _configure_guardrails(built[sid], cfg)
            elif sid == "dcgm":
                _configure_dcgm(built[sid], cfg)
        except Exception as e:  # noqa: BLE001 - a missing/broken adapter degrades that system only
            log.warning("adapter %s unavailable: %s", sid, type(e).__name__)
            built[sid] = MissingAdapter(sid, ctx, f"{type(e).__name__}: {e}")
    ordered = {sid: built[sid] for sid in SYSTEM_IDS}
    root_problem = cfg.repo_root_problem()
    global_problems = [Problem(severity="error", code="console.repo_root.missing", message=root_problem,
                               fix="export NEMO_CONSOLE_REPO_ROOT=/path/to/lab-repo and restart nemo-console")] if root_problem else []
    probe = EngineProbe(runner)
    active = (CapableProbe(http, settings.effective_urls().get("litellm", "http://127.0.0.1:4000"),
                           cfg.capable_probe_key_env, interval_s=cfg.capable_probe_interval_s,
                           timeout_s=cfg.capable_probe_timeout_s, retry_delay_s=cfg.capable_probe_retry_s)
              if cfg.capable_probe else None)
    validate_sem = asyncio.Semaphore(2)
    config_store = ConfigStore(store, ordered, cfg.config_cap_bytes, cfg.revisions_keep, validate_sem)

    async def enrich_switchyard(st: SystemStatus) -> SystemStatus:
        try:
            routes = ordered["switchyard"].config_path("routes")
            text = routes.read_text() if routes.stat().st_size <= cfg.config_cap_bytes else ""
        except (KeyError, OSError, UnicodeDecodeError):
            return st
        problems = await capable_tier_problems(probe, cfg.engine_probes, text, cfg.repo_root, cfg.engine_start)
        if active is not None:
            running = [t for t in capable_targets(cfg.engine_probes, text) if await probe.state(t[1]) == "running"]
            active.schedule([alias for alias, _, _ in running])     # background, at most once per interval per alias
            problems += probe_problems(active.results(), running, cfg.repo_root, cfg.engine_start)
        if not problems:
            return st
        # only a capable tier that exists and is stopped (or, with the probe on, runs but does not answer) degrades
        # Switchyard; "not deployed" and a probe that cannot run are information
        dark = any(p.code == "switchyard.tier.capable_dark" for p in problems)
        broken = any(p.code == "switchyard.tier.capable_broken" for p in problems)
        health = "degraded" if (dark or broken) and st.health == "up" else st.health
        prefix = "capable tier dark: " if dark else "capable tier not answering: " if broken else ""
        return st.model_copy(update={"health": health, "summary": f"{prefix}{st.summary}",
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
        topology=TopologyService(status, http, probe, settings.effective_urls().get("litellm", "http://127.0.0.1:4000"),
                                 cfg.topology_engines),
        sse=SseLimiter(cfg.sse_max_connections), telemetry=telemetry, trust_notice=trust_notice,
        validate_sem=validate_sem, capable_probe=active)
