from __future__ import annotations

import asyncio
import json
import time

from nemo_console.domain.models import SystemEvent
from nemo_console.services.events import SseLimiter, event_stream

from .conftest import Env


def _ev(n: int) -> SystemEvent:
    return SystemEvent(ts=time.time(), system="switchyard", kind="routing.decision", summary=f"e{n} Bearer abcdefghijklmnop",
                       data={"route": "r", "api_key": "hidden-value"})


async def test_stream_delivers_events_redacted_and_releases_slot(env: Env) -> None:
    env.adapters["switchyard"].follow = [_ev(1), _ev(2)]
    async with env.client() as c:
        r = await c.get("/api/v1/systems/switchyard/events/stream")
    assert r.status_code == 200 and r.headers["content-type"].startswith("text/event-stream")
    datas = [json.loads(line[6:]) for line in r.text.splitlines() if line.startswith("data: ")]
    assert [d["summary"].split()[0] for d in datas] == ["e1", "e2"]
    assert "abcdefghijklmnop" not in r.text and "hidden-value" not in r.text
    assert env.container.sse.active == 0
    assert r.headers["cache-control"] == "no-store"


async def test_connection_cap_returns_429(env: Env) -> None:
    assert env.container.sse.acquire() and env.container.sse.acquire()          # cap is 2 in the test settings
    async with env.client() as c:
        r = await c.get("/api/v1/systems/switchyard/events/stream")
    assert r.status_code == 429 and r.json()["error"]["code"] == "too_many_streams"
    assert "content-security-policy" in r.headers
    env.container.sse.release()
    env.adapters["switchyard"].follow = []
    async with env.client() as c:
        assert (await c.get("/api/v1/systems/switchyard/events/stream")).status_code == 200


async def test_idle_timeout_closes_stream_and_frees_slot(env: Env) -> None:
    env.adapters["switchyard"].follow_block = True
    lim = SseLimiter(1)
    t0 = time.monotonic()
    chunks = [c async for c in event_stream(env.adapters["switchyard"], lim, idle_timeout=0.3, heartbeat=0.1)]
    assert time.monotonic() - t0 < 2
    assert any(b"idle-timeout" in c for c in chunks) and any(b"keep-alive" in c for c in chunks)
    assert lim.active == 0


async def test_cancelled_consumer_releases_slot(env: Env) -> None:
    env.adapters["switchyard"].follow_block = True
    lim = SseLimiter(1)
    gen = event_stream(env.adapters["switchyard"], lim, idle_timeout=30, heartbeat=0.05)

    async def consume() -> None:
        async for _ in gen:
            pass

    task = asyncio.create_task(consume())
    await asyncio.sleep(0.2)
    task.cancel()
    await asyncio.gather(task, return_exceptions=True)
    await gen.aclose()
    assert lim.active == 0


async def test_server_shutdown_ends_open_streams_cleanly(env: Env) -> None:
    """2026-10-24: a browser's stream kept the process alive on SIGTERM until uvicorn's graceful timeout cancelled it
    (and logged an error); `nemo-console stop` gave up first. begin_shutdown() (the server's exit hook) must end every
    open stream at once with `event: shutdown` and free its slot."""
    from nemo_console.services.events import begin_shutdown

    env.adapters["switchyard"].follow_block = True
    lim = SseLimiter(1)
    chunks: list[bytes] = []

    async def consume() -> None:
        async for c in event_stream(env.adapters["switchyard"], lim, idle_timeout=60, heartbeat=30):
            chunks.append(c)

    task = asyncio.create_task(consume())
    await asyncio.sleep(0.1)                           # connected, waiting on a 30 s heartbeat
    t0 = time.monotonic()
    begin_shutdown()
    await asyncio.wait_for(task, timeout=2)
    assert time.monotonic() - t0 < 1
    assert chunks[-1].startswith(b"event: shutdown") and lim.active == 0
