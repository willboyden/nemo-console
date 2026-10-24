"""Health, overview, topology and per-system observation (status, metrics, events)."""
from __future__ import annotations

import asyncio
import time
from typing import Annotated, Any

from fastapi import APIRouter, Depends, Query
from fastapi.responses import StreamingResponse

from nemo_console.api.deps import Adapter, C, Viewer, public
from nemo_console.config import __version__
from nemo_console.domain.models import Health, MetricsSnapshot, SystemEvent, SystemStatus, Topology
from nemo_console.metrics_keys import KEY_RE
from nemo_console.services.auth import Principal
from nemo_console.services.errors import ApiError
from nemo_console.services.events import event_stream
from nemo_console.services.metrics import WINDOWS
from nemo_console.services.status import clean_event

router = APIRouter()


@router.get("/health")
async def health(c: C, p: Annotated[Principal, Depends(public)]) -> dict[str, Any]:
    statuses = await c.status.all()
    systems: dict[str, Health] = {s.system: s.health for s in statuses}
    ok = all(h in ("up", "off") for h in systems.values())     # "off" = opt-in and stopped on purpose (addendum 9.4)
    trust = "loopback-admin" if p.name == "loopback" else "bearer"
    return {"status": "ok" if ok else "degraded", "version": __version__, "trust": trust, "role": p.role, "trust_notice": c.trust_notice, "systems": systems}


@router.get("/overview")
async def overview(c: C, _: Viewer) -> dict[str, Any]:
    statuses = await c.status.all()
    topo = await c.topology.get()
    return {"systems": [s.model_dump() for s in statuses], "topology": topo.model_dump(), "ts": time.time()}


@router.get("/topology", response_model=Topology)
async def topology(c: C, _: Viewer) -> Topology:
    return await c.topology.get()


@router.get("/systems")
async def systems(c: C, _: Viewer) -> list[dict[str, str]]:
    return [{"id": s.system, "label": c.adapters[s.system].label, "health": s.health} for s in await c.status.all()]


@router.get("/systems/{system_id}/status", response_model=SystemStatus)
async def system_status(a: Adapter, c: C, _: Viewer) -> SystemStatus:
    return await c.status.status(a.id, fresh=True)


@router.get("/systems/{system_id}/metrics", response_model=MetricsSnapshot)
async def system_metrics(a: Adapter, c: C, _: Viewer) -> MetricsSnapshot:
    return await c.status.metrics(a.id)


@router.get("/systems/{system_id}/metrics/history")
async def metrics_history(a: Adapter, c: C, _: Viewer, window: str = "15m", key: str = "") -> dict[str, Any]:
    if window not in WINDOWS:
        raise ApiError(422, "invalid_request", f"window must be one of {sorted(WINDOWS)}")
    if not KEY_RE.fullmatch(key):
        raise ApiError(422, "invalid_request", "key must be a canonical metric key, e.g. sy.requests_total")
    return {"key": key, "points": c.metrics.history(a.id, window, key)}


@router.get("/systems/{system_id}/events", response_model=list[SystemEvent])
async def events(a: Adapter, _: Viewer, limit: Annotated[int, Query(ge=1, le=500)] = 100) -> list[SystemEvent]:
    try:
        return [clean_event(e) for e in (await a.recent_events(limit=limit))[:limit]]
    except Exception as e:  # noqa: BLE001
        raise ApiError(502, "adapter_error", f"could not read events: {type(e).__name__}") from None


@router.get("/systems/{system_id}/events/stream")
async def events_stream(a: Adapter, c: C, _: Viewer) -> StreamingResponse:
    if c.sse.active >= c.sse.cap:      # cheap pre-check; the generator takes the real slot (no leak if it never starts)
        raise ApiError(429, "too_many_streams", f"at most {c.cfg.sse_max_connections} concurrent event streams")
    return StreamingResponse(event_stream(a, c.sse, c.cfg.sse_idle_timeout_s, c.cfg.sse_heartbeat_s),
                             media_type="text/event-stream", headers={"X-Accel-Buffering": "no"})


# ── Data Designer run artifacts (addendum 9.7; viewer GET only, no file serving in v1) ──────
@router.get("/systems/{system_id}/runs/{run_id}/records")
async def dd_run_records(a: Adapter, _: Viewer, run_id: str,
                         limit: Annotated[int, Query(ge=1, le=200)] = 50,
                         offset: Annotated[int, Query(ge=0)] = 0) -> dict[str, Any]:
    """The records of one run, read from records.jsonl ONLY (never parquet); values pass through as-is with no
    string over 2 KiB. `total` comes from run.json when it has one, else the counted lines."""
    from nemo_console.adapters.datadesigner import DataDesignerAdapter
    if not isinstance(a, DataDesignerAdapter):
        raise ApiError(404, "not_found", "no such endpoint for this system")
    try:
        return await asyncio.to_thread(a.records_page, run_id, limit, offset)
    except KeyError:
        raise ApiError(404, "not_found", "unknown run") from None


@router.get("/systems/{system_id}/runs/{run_id}/files")
async def dd_run_files(a: Adapter, _: Viewer, run_id: str) -> dict[str, Any]:
    """Paths + sizes of a run's artifacts (for the UI's download links); nothing is served here in v1: the UI shows
    paths and the user opens them on disk."""
    from nemo_console.adapters.datadesigner import DataDesignerAdapter
    if not isinstance(a, DataDesignerAdapter):
        raise ApiError(404, "not_found", "no such endpoint for this system")
    try:
        return await asyncio.to_thread(a.run_files, run_id)
    except KeyError:
        raise ApiError(404, "not_found", "unknown run") from None


