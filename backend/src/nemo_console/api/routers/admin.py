"""Routing summary, audit, settings and the console's own /metrics."""
from __future__ import annotations

from typing import Annotated, Any

from fastapi import APIRouter, Query
from fastapi.responses import Response
from pydantic import BaseModel, ConfigDict

from nemo_console.adapters import SYSTEM_IDS
from nemo_console.api.deps import Admin, C, Viewer
from nemo_console.services.errors import ApiError
from nemo_console.services.routing import WINDOW_SECONDS, routing_summary

router = APIRouter()
root_router = APIRouter()


class SettingsBody(BaseModel):
    model_config = ConfigDict(extra="forbid")
    urls: dict[str, str] | None = None
    poll_interval_s: float | None = None
    playground_key_env: dict[str, str] | None = None


@router.get("/routing/summary")
async def routing(c: C, _: Viewer, window: str = "1h") -> dict[str, Any]:
    if window not in WINDOW_SECONDS:
        raise ApiError(422, "invalid_request", f"window must be one of {sorted(WINDOW_SECONDS)}")
    try:
        return await routing_summary(c.adapters["switchyard"], window)
    except Exception as e:  # noqa: BLE001
        raise ApiError(502, "adapter_error", f"could not read routing events: {type(e).__name__}") from None


@router.get("/audit")
async def audit(c: C, _: Admin, limit: Annotated[int, Query(ge=1, le=500)] = 100, system: str | None = None) -> list[dict[str, Any]]:
    if system is not None and system not in SYSTEM_IDS:
        raise ApiError(422, "invalid_request", "unknown system")
    return c.audit.list(limit, system)


@router.get("/settings")
async def settings_get(c: C, p: Viewer) -> dict[str, Any]:
    return c.settings.view(admin=p.role == "admin")


@router.put("/settings")
async def settings_put(c: C, body: SettingsBody, _: Admin) -> dict[str, Any]:
    view, interval = c.settings.update(body.urls, body.poll_interval_s, body.playground_key_env)
    if interval is not None:
        c.metrics.interval = interval
    return view


@root_router.get("/metrics")
async def console_metrics(c: C, _: Viewer) -> Response:
    c.telemetry.sse.set(c.sse.active)
    return Response(c.telemetry.render(), media_type="text/plain; version=0.0.4")
