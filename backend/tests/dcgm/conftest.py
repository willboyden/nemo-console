from __future__ import annotations

import json
from collections.abc import AsyncIterator, Callable
from pathlib import Path

import httpx
import pytest
import respx

from nemo_console.adapters.base import AdapterContext
from nemo_console.adapters.dcgm import DcgmAdapter

from .helpers import (
    CONTAINER,
    EXPORTER,
    FIX,
    PROM,
    FakeRunner,
    fx,
    is_image_inspect,
    is_inspect,
    is_logs,
    is_restart,
    ok,
)

QUERY_FIXTURES = (("up{", "prom_up.json"), ("count by", "prom_count.json"), ("max by", "prom_tempmax.json"),
                  ("sum by", "prom_power.json"))


def prom_handler(overrides: dict[str, str] | None = None) -> Callable[[httpx.Request], httpx.Response]:
    over = overrides or {}

    def handler(request: httpx.Request) -> httpx.Response:
        q = request.url.params.get("query", "")
        for needle, name in QUERY_FIXTURES:
            if needle in q:
                return httpx.Response(200, text=over.get(needle, fx(name)))
        return httpx.Response(400, text="unknown query")
    return handler


@pytest.fixture
def runner() -> FakeRunner:
    r = FakeRunner()
    r.on(lambda a: is_inspect(a) and "com.docker.compose.project" in " ".join(a),
         ok("ai-lab-observability|dcgm-exporter\n"))
    r.on(is_inspect, ok(fx("inspect.txt")))
    r.on(is_image_inspect, ok("sha256:48464e50aa76\n"))
    r.on(is_restart, ok(CONTAINER + "\n"))
    r.on(is_logs, ok("time=2026-10-24T11:13:31.052Z level=INFO msg=\"Starting dcgm-exporter\" Version=4.8.4\n"))
    return r


@pytest.fixture
def mock() -> respx.MockRouter:
    with respx.mock(assert_all_called=False) as m:
        m.get(f"{EXPORTER}/metrics").respond(200, text=fx("metrics.txt"))
        m.get(f"{EXPORTER}/debug/pprof/").respond(404, text="404 page not found\n")
        m.get(f"{PROM}/api/v1/query").mock(side_effect=prom_handler())
        yield m


@pytest.fixture
def counters_file(tmp_path: Path) -> Path:
    d = tmp_path / "obs" / "dcgm"
    d.mkdir(parents=True)
    p = d / "counters.csv"
    p.write_text((FIX / "counters.good.csv").read_text())
    return p


@pytest.fixture
async def make_adapter(tmp_path: Path, runner: FakeRunner, mock: respx.MockRouter,
                       counters_file: Path) -> AsyncIterator[Callable[..., DcgmAdapter]]:
    clients: list[httpx.AsyncClient] = []

    def _make(run: FakeRunner | None = None) -> DcgmAdapter:
        client = httpx.AsyncClient(trust_env=False)
        clients.append(client)
        ctx = AdapterContext(runner=run or runner, http=client,
                             urls={"dcgm": EXPORTER, "prometheus": PROM},
                             paths={"dcgm_counters": counters_file, "observability_dir": counters_file.parent.parent,
                                    "observability_compose": counters_file.parent.parent / "docker-compose.yml"},
                             state_dir=tmp_path / "state")
        a = DcgmAdapter(ctx)
        a.follow_interval = 0.01
        a.poll_interval = 0.01
        a.settle = 0.0
        a.apply_timeout = 0.3
        return a
    yield _make
    for c in clients:
        await c.aclose()


@pytest.fixture
def adapter(make_adapter: Callable[..., DcgmAdapter]) -> DcgmAdapter:
    return make_adapter()


def xid_metrics(code: int = 79, extra: str = "") -> str:
    """SYNTHETIC: the real scrape plus an XID series on GPU 1 (the real exporter shows none while no XID occurred)."""
    line = ('DCGM_FI_DEV_XID_ERRORS{gpu="1",UUID="GPU-00000000-0000-0000-0000-000000000001",pci_bus_id="00000000:F1:00.0",'
            'device="nvidia1",modelName="NVIDIA RTX PRO 6000 Blackwell Workstation Edition",hostname="exporter",'
            f'DCGM_FI_DRIVER_VERSION="595.99.02"}} {code}\n')
    return fx("metrics.txt") + line + extra


def diag_doc(statuses: dict[str, str], warning: str | None = None) -> str:
    """SYNTHETIC level-2-shaped document built from the REAL level-1 fixture's structure."""
    d = json.loads(fx("diag_level1_gpu0.json"))
    tests = []
    for name, st in statuses.items():
        res: dict[str, object] = {"entity_group": "GPU", "entity_group_id": 1, "entity_id": 0, "status": st}
        if warning and st != "Pass":
            res["warnings"] = [{"warning": warning}]
        tests.append({"name": name, "results": [res], "test_summary": {"status": st}})
    d["DCGM Diagnostic"]["test_categories"] = [{"category": "Hardware", "tests": tests}]
    return json.dumps(d)
