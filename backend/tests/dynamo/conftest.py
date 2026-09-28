from __future__ import annotations

from collections.abc import AsyncIterator
from pathlib import Path

import httpx
import pytest
import respx

from nemo_console.adapters.base import AdapterContext
from nemo_console.adapters.dynamo import DynamoAdapter

from .helpers import FIX, FakeRunner

BASE_URL = "http://127.0.0.1:8020"


@pytest.fixture
def runner() -> FakeRunner:
    return FakeRunner()


@pytest.fixture
def compose(tmp_path: Path) -> Path:
    p = tmp_path / "repo" / "docker-compose.yml"
    p.parent.mkdir()
    p.write_text((FIX / "base_compose.yml").read_text())
    return p


@pytest.fixture
async def make(runner: FakeRunner, compose: Path, tmp_path: Path) -> AsyncIterator[type]:
    clients: list[httpx.AsyncClient] = []

    def _make(**kw: float) -> DynamoAdapter:
        c = httpx.AsyncClient(trust_env=False)
        clients.append(c)
        state = tmp_path / "state"
        state.mkdir(exist_ok=True)
        ctx = AdapterContext(runner=runner, http=c, urls={"dynamo": BASE_URL}, paths={"dynamo_compose": compose},
                             state_dir=state)
        return DynamoAdapter(ctx, **{"poll_interval": 0.0, "health_timeout": 0.2, "follow_interval": 0.0, **kw})

    yield _make  # type: ignore[misc]
    for c in clients:
        await c.aclose()


@pytest.fixture
def mock() -> respx.MockRouter:
    with respx.mock(assert_all_called=False) as r:
        yield r
