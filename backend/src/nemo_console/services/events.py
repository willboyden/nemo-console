"""SSE plumbing: connection cap, idle timeout, heartbeat. Events are already redacted metadata."""
from __future__ import annotations

import asyncio
import contextlib
import json
from collections.abc import AsyncIterator

from nemo_console.adapters.base import SystemAdapter
from nemo_console.domain.models import SystemEvent
from nemo_console.services.common import redact_obj


class SseLimiter:
    def __init__(self, cap: int) -> None:
        self.cap = cap
        self.active = 0

    def acquire(self) -> bool:
        if self.active >= self.cap:
            return False
        self.active += 1
        return True

    def release(self) -> None:
        self.active = max(0, self.active - 1)


async def event_stream(a: SystemAdapter, limiter: SseLimiter, idle_timeout: float, heartbeat: float) -> AsyncIterator[bytes]:
    """Takes its slot on the first iteration and releases it in `finally`: a stream that never starts holds nothing."""
    if not limiter.acquire():
        yield b"event: error\ndata: {\"error\": \"too many streams\"}\n\n"
        return
    it = a.follow_events().__aiter__()
    loop = asyncio.get_running_loop()
    last_real = loop.time()
    pending: asyncio.Future[SystemEvent] | None = None
    try:
        yield b": connected\n\n"
        while True:
            remaining = idle_timeout - (loop.time() - last_real)
            if remaining <= 0:
                yield b"event: idle-timeout\ndata: {}\n\n"
                return
            if pending is None:
                pending = asyncio.ensure_future(it.__anext__())
            done, _ = await asyncio.wait({pending}, timeout=min(heartbeat, remaining))
            if not done:
                yield b": keep-alive\n\n"
                continue
            task, pending = pending, None
            try:
                ev = task.result()
            except StopAsyncIteration:
                return
            except Exception:  # noqa: BLE001
                yield b"event: error\ndata: {}\n\n"
                return
            last_real = loop.time()
            payload = json.dumps(redact_obj(ev.model_dump()), default=str)
            yield f"data: {payload}\n\n".encode()
    finally:
        if pending is not None:
            pending.cancel()
            with contextlib.suppress(BaseException):
                await pending
        aclose = getattr(it, "aclose", None)
        if aclose is not None:
            with contextlib.suppress(Exception):
                await aclose()
        limiter.release()
