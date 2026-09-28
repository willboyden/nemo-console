"""Role dependencies (every route carries exactly one of public/viewer/admin) and shared lookups."""
from __future__ import annotations

import contextlib
import re
from collections.abc import AsyncIterator
from typing import Annotated

from fastapi import Depends, Request

from nemo_console.adapters import SYSTEM_IDS
from nemo_console.adapters.base import SystemAdapter
from nemo_console.container import Container
from nemo_console.domain.models import SystemId
from nemo_console.services.auth import PUBLIC, Principal
from nemo_console.services.errors import ApiError

ID_RE = re.compile(r"[a-z0-9][a-z0-9_.\-]{0,63}")


def get_container(request: Request) -> Container:
    c: Container = request.app.state.container
    return c


def _principal(request: Request) -> Principal:
    p = getattr(request.state, "principal", None)
    return p if isinstance(p, Principal) else PUBLIC


async def public(request: Request) -> Principal:
    """Marker: the route is intentionally open (health, static shell)."""
    return _principal(request)


async def viewer(request: Request) -> Principal:
    p = _principal(request)
    if p.role not in ("viewer", "admin"):
        raise ApiError(401, "unauthorized", "a valid bearer API key is required")
    return p


async def admin(request: Request) -> Principal:
    p = _principal(request)
    if p.role == "admin":
        return p
    if p.role == "viewer":
        raise ApiError(403, "forbidden", "this key has the viewer role (read-only)")
    raise ApiError(401, "unauthorized", "a valid bearer API key is required")


ROLE_DEPS = (public, viewer, admin)
C = Annotated[Container, Depends(get_container)]
Viewer = Annotated[Principal, Depends(viewer)]
Admin = Annotated[Principal, Depends(admin)]


def get_adapter(system_id: str, c: C) -> SystemAdapter:
    for sid in SYSTEM_IDS:
        if system_id == sid:
            return c.adapters[sid]
    raise ApiError(404, "not_found", "unknown system")


def valid_id(value: str) -> str:
    if ID_RE.fullmatch(value) is None:
        raise ApiError(404, "not_found", "not found")
    return value


def file_id_dep(file_id: str) -> str:
    return valid_id(file_id)


def action_dep(action: str) -> str:
    return valid_id(action)


Adapter = Annotated[SystemAdapter, Depends(get_adapter)]
FileId = Annotated[str, Depends(file_id_dep)]
ActionId = Annotated[str, Depends(action_dep)]


@contextlib.asynccontextmanager
async def exclusive(c: Container, system: SystemId) -> AsyncIterator[None]:
    """One mutating operation per system at a time (restarts must not overlap)."""
    lock = c.lock(system)
    if lock.locked():
        raise ApiError(409, "busy", f"another change to {system} is in progress")
    async with lock:
        yield
