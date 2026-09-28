"""Failure-is-data wrappers around the adapters + a short-TTL status cache shared by overview/health/topology."""
from __future__ import annotations

import asyncio
import logging
import time
from collections.abc import Awaitable, Callable

from nemo_console.adapters.base import SystemAdapter
from nemo_console.domain.models import MetricsSnapshot, Problem, SystemEvent, SystemId, SystemStatus
from nemo_console.services.common import redact, redact_obj

log = logging.getLogger(__name__)


def clean_status(st: SystemStatus) -> SystemStatus:
    """Defence in depth: adapters redact, the core scrubs again before anything leaves the process."""
    return st.model_copy(update={
        "summary": redact(st.summary),
        "problems": [p.model_copy(update={"message": redact(p.message), "fix": redact(p.fix) if p.fix else None})
                     for p in st.problems]})


def clean_event(ev: SystemEvent) -> SystemEvent:
    return ev.model_copy(update={"summary": redact(ev.summary), "data": redact_obj(ev.data)})


class StatusService:
    def __init__(self, adapters: dict[SystemId, SystemAdapter], ttl: float, timeout: float,
                 global_problems: list[Problem] | None = None,
                 enrichers: dict[SystemId, Callable[[SystemStatus], Awaitable[SystemStatus]]] | None = None) -> None:
        self.enrichers = enrichers or {}
        self.global_problems = global_problems or []
        self.adapters = adapters
        self.ttl = ttl
        self.timeout = timeout
        self._cache: dict[SystemId, tuple[float, SystemStatus]] = {}

    async def status(self, system: SystemId, fresh: bool = False) -> SystemStatus:
        hit = self._cache.get(system)
        if hit and not fresh and time.monotonic() - hit[0] < self.ttl:
            return hit[1]
        a = self.adapters[system]
        try:
            st = clean_status(await asyncio.wait_for(a.status(), self.timeout))
            if (enrich := self.enrichers.get(system)) is not None:
                try:
                    st = await enrich(st)
                except Exception:  # noqa: BLE001 - enrichment is additive and never breaks status
                    log.warning("status enrichment for %s failed", system)
            if self.global_problems:
                st = st.model_copy(update={"problems": [*self.global_problems, *st.problems]})
        except Exception as e:  # noqa: BLE001 - an adapter bug must not become a 5xx
            log.warning("adapter %s status failed: %s", system, type(e).__name__)
            st = SystemStatus(system=system, health="unknown", summary="status check failed", checked_at=time.time(),
                              problems=[Problem(severity="error", code="console.adapter.error",
                                                message=redact(f"{type(e).__name__}: {e}")[:200],
                                                fix="check the console log; the adapter raised instead of returning health")])
        self._cache[system] = (time.monotonic(), st)
        return st

    def invalidate(self, system: SystemId) -> None:
        self._cache.pop(system, None)

    async def all(self) -> list[SystemStatus]:
        return list(await asyncio.gather(*(self.status(s) for s in self.adapters)))

    async def metrics(self, system: SystemId) -> MetricsSnapshot:
        try:
            return await asyncio.wait_for(self.adapters[system].metrics(), self.timeout)
        except Exception as e:  # noqa: BLE001
            return MetricsSnapshot(system=system, ts=time.time(), available=False,
                                   error=redact(f"{type(e).__name__}: {e}")[:200])
