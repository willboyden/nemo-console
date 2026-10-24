from __future__ import annotations

from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

import httpx
import pytest
from os_helpers import FakeRunner, World, make_ctx, make_runner

from nemo_console.adapters.openshell import OpenShellAdapter


@pytest.fixture
async def make_adapter(tmp_path: Path) -> AsyncIterator[Callable[..., tuple[OpenShellAdapter, FakeRunner, World]]]:
    clients: list[httpx.AsyncClient] = []

    def _make(world: World | None = None, **kw: Any) -> tuple[OpenShellAdapter, FakeRunner, World]:
        client = httpx.AsyncClient(trust_env=False)
        clients.append(client)
        runner, w = make_runner(world)
        a = OpenShellAdapter(make_ctx(tmp_path, runner, client, **kw))
        a.follow_interval = 0.01
        a.poll_interval = 0.01
        return a, runner, w

    yield _make
    for c in clients:
        await c.aclose()
