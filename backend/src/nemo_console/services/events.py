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


# Server shutdown ends every open SSE stream cleanly (a final `event: shutdown`). Without it a browser's stream never
# closes on its own: uvicorn waited for it on SIGTERM until its graceful-shutdown timeout cancelled the task (and logged
# an error), and `nemo-console stop` gave up first. One event per event loop (tests run several loops).
_shutdown: tuple[asyncio.AbstractEventLoop, asyncio.Event] | None = None


def _shutdown_event() -> asyncio.Event:
    global _shutdown
    loop = asyncio.get_running_loop()
    if _shutdown is None or _shutdown[0] is not loop:
        _shutdown = (loop, asyncio.Event())
    return _shutdown[1]


def begin_shutdown() -> None:
    """Called from the server's exit handler (a signal handler on the loop thread): wake every open stream.
    call_soon_threadsafe writes to the loop's self-pipe, so a loop sleeping in select() wakes at once."""
    if _shutdown is not None:
        loop, event = _shutdown
        with contextlib.suppress(RuntimeError):          # the loop is already closed
            loop.call_soon_threadsafe(event.set)


async def event_stream(a: SystemAdapter, limiter: SseLimiter, idle_timeout: float, heartbeat: float) -> AsyncIterator[bytes]:
    """Takes its slot on the first iteration and releases it in `finally`: a stream that never starts holds nothing."""
    if not limiter.acquire():
        yield b"event: error\ndata: {\"error\": \"too many streams\"}\n\n"
        return
    it = a.follow_events().__aiter__()
    loop = asyncio.get_running_loop()
    last_real = loop.time()
    pending: asyncio.Future[SystemEvent] | None = None
    stop = _shutdown_event()
    stop_wait: asyncio.Future[bool] = asyncio.ensure_future(stop.wait())
    try:
        yield b": connected\n\n"
        while True:
            if stop.is_set():
                yield b"event: shutdown\ndata: {}\n\n"
                return
            remaining = idle_timeout - (loop.time() - last_real)
            if remaining <= 0:
                yield b"event: idle-timeout\ndata: {}\n\n"
                return
            if pending is None:
                pending = asyncio.ensure_future(it.__anext__())
            waiters: set[asyncio.Future[object]] = {pending, stop_wait}  # type: ignore[arg-type]
            done, _ = await asyncio.wait(waiters, timeout=min(heartbeat, remaining), return_when=asyncio.FIRST_COMPLETED)
            if stop_wait in done:
                continue                                  # the loop top sends `event: shutdown` and ends the stream
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
        stop_wait.cancel()
        with contextlib.suppress(BaseException):
            await stop_wait
        if pending is not None:
            pending.cancel()
            with contextlib.suppress(BaseException):
                await pending
        aclose = getattr(it, "aclose", None)
        if aclose is not None:
            with contextlib.suppress(Exception):
                await aclose()
        limiter.release()
