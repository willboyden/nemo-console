from __future__ import annotations

from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

import httpx
import pytest
import respx
from relay_helpers import FakeRunner, make_ctx, standard_runner

from nemo_console.adapters.relay import RelayAdapter


@pytest.fixture
def router() -> Any:
    with respx.mock(assert_all_called=False) as r:
        r.get("http://127.0.0.1:4040/health", name="gateway_health").respond(404)
        r.get("http://127.0.0.1:4000/health/liveliness", name="upstream_liveliness").respond(200, json="I'm alive!")
        # Kept mocked so a regression that polls it again shows up as a call count, not a respx error.
        r.get("http://127.0.0.1:4040/v1/models", name="gateway_models").respond(
            401, json={"error": {"message": "Authentication Error, No api key passed in."}})
        yield r


@pytest.fixture
async def make_adapter(tmp_path: Path, router: Any) -> AsyncIterator[Callable[..., tuple[RelayAdapter, FakeRunner]]]:
    clients: list[httpx.AsyncClient] = []

    def _make(runner: FakeRunner | None = None, **kw: Any) -> tuple[RelayAdapter, FakeRunner]:
        client = httpx.AsyncClient(trust_env=False)
        clients.append(client)
        run = runner or standard_runner()
        a = RelayAdapter(make_ctx(tmp_path, run, client, **kw))
        a.poll_interval = 0
        a.follow_interval = 0.01
        return a, run

    yield _make
    for c in clients:
        await c.aclose()
