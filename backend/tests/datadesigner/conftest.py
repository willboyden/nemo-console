from __future__ import annotations

from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

import httpx
import pytest
import respx
from dd_helpers import FakeRunner, make_ctx, standard_runner

from nemo_console.adapters.datadesigner import DataDesignerAdapter


@pytest.fixture
def router() -> Any:
    with respx.mock(assert_all_called=False) as r:
        r.get("http://127.0.0.1:4000/health/liveliness", name="liveliness").respond(200, json="I'm alive!")
        yield r


@pytest.fixture
async def make_adapter(tmp_path: Path, router: Any
                       ) -> AsyncIterator[Callable[..., tuple[DataDesignerAdapter, FakeRunner]]]:
    clients: list[httpx.AsyncClient] = []

    def _make(runner: FakeRunner | None = None, **kw: Any) -> tuple[DataDesignerAdapter, FakeRunner]:
        client = httpx.AsyncClient(trust_env=False)
        clients.append(client)
        run = runner or standard_runner()
        a = DataDesignerAdapter(make_ctx(tmp_path, run, client, **kw))
        a.follow_interval = 0.01
        return a, run

    yield _make
    for c in clients:
        await c.aclose()
