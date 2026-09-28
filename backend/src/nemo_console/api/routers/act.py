"""Actions (danger levels enforced here, params validated against the ActionSpec) and the playground proxy."""
from __future__ import annotations

import asyncio
import re
import time
from typing import Annotated, Any

from fastapi import APIRouter, Response
from fastapi.responses import StreamingResponse
from pydantic import BaseModel, ConfigDict, Field

from nemo_console.api.deps import ActionId, Adapter, Admin, C, Viewer, exclusive
from nemo_console.domain.models import ActionResult, ActionSpec, PlaygroundSpec, Problem
from nemo_console.services import playground
from nemo_console.services.common import cap, redact
from nemo_console.services.errors import ApiError
from nemo_console.services.params import validate_values

router = APIRouter()
MODEL_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:/@\-]{0,127}")


class ActionBody(BaseModel):
    model_config = ConfigDict(extra="forbid")
    params: dict[str, Any] = {}
    confirm: bool = False


@router.get("/systems/{system_id}/actions", response_model=list[ActionSpec])
async def actions_list(a: Adapter, _: Viewer) -> list[ActionSpec]:
    return a.actions()


@router.post("/systems/{system_id}/actions/{action}", response_model=ActionResult)
async def action_run(a: Adapter, c: C, action: ActionId, body: ActionBody, p: Viewer) -> ActionResult:
    spec = next((s for s in a.actions() if s.id == action), None)
    if spec is None:
        raise ApiError(404, "not_found", "no such action")
    if spec.role == "admin" and p.role != "admin":
        raise ApiError(403, "forbidden", "this action needs the admin role")
    if spec.danger != "safe" and not body.confirm:
        raise ApiError(400, "confirm_required", f"'{action}' is {spec.danger}; send confirm:true")
    params = validate_values(spec.params, body.params)
    async with exclusive(c, a.id):
        t0 = time.monotonic()
        try:
            res = await asyncio.wait_for(a.run_action(action, params),
                                       c.cfg.action_timeouts.get(f"{a.id}.{action}", c.cfg.action_timeout_s))
        except KeyError:
            raise ApiError(404, "not_found", "no such action") from None
        except Exception as e:  # noqa: BLE001 - failure is data
            msg = "timed out" if isinstance(e, TimeoutError) else f"{type(e).__name__}: {e}"
            return ActionResult(ok=False, duration_ms=int((time.monotonic() - t0) * 1000),
                                problems=[Problem(severity="error", code="console.action.failed", message=redact(msg)[:300])])
        c.status.invalidate(a.id)          # state probably changed
        return res.model_copy(update={
            "output": cap(redact(res.output), 64_000),
            "problems": [pr.model_copy(update={"message": redact(pr.message), "fix": redact(pr.fix) if pr.fix else None})
                         for pr in res.problems]})


# ── playground ──────────────────────────────────────────────────────────────────────────────
class ChatMessage(BaseModel):
    model_config = ConfigDict(extra="forbid")
    role: Annotated[str, Field(pattern=r"^(system|user|assistant|tool)$")]
    content: str | list[dict[str, Any]]


class ChatBody(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model: str
    messages: Annotated[list[ChatMessage], Field(min_length=1, max_length=200)]
    max_tokens: Annotated[int, Field(ge=1)] | None = None
    stream: bool = True
    system: str | None = Field(default=None, max_length=100_000)


@router.get("/systems/{system_id}/playground", response_model=PlaygroundSpec | None)
async def playground_spec(a: Adapter, _: Viewer) -> PlaygroundSpec | None:
    return a.playground()


@router.post("/systems/{system_id}/playground/chat")
async def playground_chat(a: Adapter, c: C, body: ChatBody, _: Admin) -> Response:
    if MODEL_RE.fullmatch(body.model) is None:
        raise ApiError(422, "invalid_request", "model must be a plain model/route id")
    prepared = playground.prepare(a, body.model_dump(exclude_none=True), c.settings.effective_key_env(),
                                  c.cfg.playground_max_tokens)
    resp = await playground.open_upstream(c.http, prepared)
    extra = playground.served_by(resp.headers)
    if prepared.stream:
        return StreamingResponse(playground.relay_stream(resp, prepared.secret), media_type="text/event-stream",
                                 headers={**extra, "X-Accel-Buffering": "no"})
    data = await playground.read_capped(resp, prepared.secret)
    return Response(data, media_type="application/json", headers=extra)
