from __future__ import annotations

from pathlib import Path

import httpx
import pytest
import respx

from nemo_console.adapters.base import AdapterContext
from nemo_console.adapters.switchyard import SwitchyardAdapter

from .helpers import FIX, INSPECT_OK, FakeRunner, fx, is_inspect, is_tail, ok

BASE = "http://127.0.0.1:4010"


@pytest.fixture
def runner() -> FakeRunner:
    r = FakeRunner()
    r.on(is_inspect, ok(INSPECT_OK))
    r.on(is_tail, ok(fx("routing_tail.jsonl")))
    return r


@pytest.fixture
def routes_dir(tmp_path: Path) -> Path:
    d = tmp_path / "switchyard"
    d.mkdir()
    (d / "routes.toml").write_text((FIX / "routes.good.toml").read_text())
    return d


@pytest.fixture
def mock() -> respx.MockRouter:
    with respx.mock(assert_all_called=False) as m:
        m.get(f"{BASE}/health").respond(200, text=fx("health.json"))
        m.get(f"{BASE}/v1/models").respond(200, text=fx("models.json"))
        m.get(f"{BASE}/v1/stats").respond(200, text=fx("stats.json"))
        m.get(f"{BASE}/metrics").respond(200, text=fx("metrics.txt"))
        yield m


@pytest.fixture
async def adapter(runner: FakeRunner, routes_dir: Path, mock: respx.MockRouter, tmp_path: Path):
    async with httpx.AsyncClient(trust_env=False) as http:
        ctx = AdapterContext(runner=runner, http=http, urls={"switchyard": BASE},
                             paths={"switchyard_routes": routes_dir / "routes.toml", "switchyard_dir": routes_dir},
                             secret_env={"switchyard": "SWITCHYARD_BEARER"}, state_dir=tmp_path)
        a = SwitchyardAdapter(ctx)
        a.poll_interval = 0.0
        a.poll_seconds = 0.2
        a.follow_interval = 0.01
        yield a
