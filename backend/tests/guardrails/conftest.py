from __future__ import annotations

from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

import httpx
import pytest
import respx
from gr_helpers import BASE, FakeRunner, fx, make_ctx, standard_runner

from nemo_console.adapters.guardrails import GuardrailsAdapter


@pytest.fixture
def server() -> Any:
    """The server's HTTP surface, answering with captured real bodies."""
    with respx.mock(assert_all_called=False) as r:
        r.get(f"{BASE}/v1/health", name="health").respond(200, text=fx("health.json"))
        r.get(f"{BASE}/v1/rails/configs", name="configs").respond(200, text=fx("configs.json"))
        yield r


@pytest.fixture
async def make_adapter(tmp_path: Path, server: Any) -> AsyncIterator[Callable[..., tuple[GuardrailsAdapter, FakeRunner]]]:
    clients: list[httpx.AsyncClient] = []

    def _make(runner: FakeRunner | None = None) -> tuple[GuardrailsAdapter, FakeRunner]:
        client = httpx.AsyncClient(trust_env=False)
        clients.append(client)
        run = runner or standard_runner()
        a = GuardrailsAdapter(make_ctx(tmp_path, run, client))
        a.follow_interval = 0.01
        a.poll_seconds = 0.3
        a.poll_interval = 0.02
        return a, run

    yield _make
    for c in clients:
        await c.aclose()
