"""status / metrics / events against captured real /metrics, Prometheus JSON and docker inspect."""
from __future__ import annotations

import json
from collections.abc import Callable

import httpx
import pytest
import respx

from nemo_console.adapters.dcgm import (
    DcgmAdapter,
    exporter_version,
    parse_exposition,
    short_model,
    version_tuple,
)

from .conftest import prom_handler, xid_metrics
from .helpers import CONTAINER, EXPORTER, PROM, FakeRunner, bad, fx, is_image_inspect, is_inspect, ok


def codes(status) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in status.problems}


def test_parse_exposition_real_scrape() -> None:
    s = parse_exposition(fx("metrics.txt"))
    assert len(s["DCGM_FI_DEV_GPU_TEMP"]) == 2
    labels, value = s["DCGM_FI_DEV_FB_USED"][0]
    assert labels["gpu"] in {"0", "1"} and value > 1000 and labels["modelName"].startswith("NVIDIA RTX PRO 6000")
    assert "DCGM_FI_DEV_XID_ERRORS" not in s          # absent while no XID happened (live observation)
    assert "DCGM_FI_PROF_GR_ENGINE_ACTIVE" in s


def test_parse_skips_garbage_and_nan() -> None:
    s = parse_exposition('# HELP x y\nfoo{a="b} 1\nbar NaN\nbaz{gpu="0"} 2.5e+3\nqux +Inf\n')
    assert "bar" not in s and "qux" not in s and s["baz"][0][1] == 2500.0


@pytest.mark.parametrize("image,ver", [
    ("nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4@sha256:148b", "4.8.4"),
    ("nvcr.io/nvidia/k8s/dcgm-exporter:4.5.2-4.8.1", "4.8.1"),
    ("nvcr.io/nvidia/k8s/dcgm-exporter:latest", None), (None, None)])
def test_exporter_version(image: str | None, ver: str | None) -> None:
    assert exporter_version(image) == ver


def test_versions_and_names() -> None:
    assert version_tuple("4.8.10") > version_tuple("4.8.3") > version_tuple("4.8.1")
    assert short_model("NVIDIA RTX PRO 6000 Blackwell Max-Q Workstation Edition") == \
        "RTX PRO 6000 Blackwell Max-Q Workstation"


async def test_status_healthy_fixed_exporter(adapter: DcgmAdapter) -> None:
    st = await adapter.status()
    assert st.system == "dcgm" and st.health == "up", st.problems
    assert st.version == "4.8.4" and st.problems == []
    names = {c.name for c in st.components}
    assert CONTAINER in names and "spark1 (dcgm-sparks)" in names and "local (dcgm)" in names
    cont = next(c for c in st.components if c.kind == "container")
    assert cont.state == "running" and cont.image == "nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4"
    assert "2 GPU(s) local" in st.summary and "6/6" in st.summary
    # nothing identifying leaks into the status
    assert "GPU-" not in st.model_dump_json() and "10.100" not in st.model_dump_json()


async def test_status_old_exporter_is_vulnerable_with_exact_fix(make_adapter: Callable[..., DcgmAdapter],
                                                                runner: FakeRunner, mock: respx.MockRouter) -> None:
    runner.rules.insert(0, (is_inspect, lambda _a: ok(
        "running|true|nvcr.io/nvidia/k8s/dcgm-exporter:4.5.2-4.8.1@sha256:aaaa|2026-10-01T00:00:00Z|0\n")))
    mock.get(f"{EXPORTER}/debug/pprof/").respond(200, text="Types of profiles available")   # what 4.8.1 did
    st = await make_adapter().status()
    assert st.health == "degraded" and st.version == "4.8.1"
    vuln = next(p for p in st.problems if p.code == "dcgm.exporter.vulnerable")
    assert vuln.severity == "error" and "DCGM_VERSION=4.6.1-4.8.4" in (vuln.fix or "") \
        and "observability/.env" in (vuln.fix or "") and "--profile gpu up -d dcgm-exporter" in (vuln.fix or "")
    exposed = next(p for p in st.problems if p.code == "dcgm.exporter.pprof_exposed")
    assert exposed.severity == "error"


async def test_pprof_exposed_even_when_version_unreadable(make_adapter: Callable[..., DcgmAdapter],
                                                          runner: FakeRunner, mock: respx.MockRouter) -> None:
    mock.get(f"{EXPORTER}/debug/pprof/").respond(200, text="x")
    st = await make_adapter().status()
    assert "dcgm.exporter.pprof_exposed" in codes(st) and "dcgm.exporter.vulnerable" not in codes(st)


async def test_exporter_down_is_data_not_exception(make_adapter: Callable[..., DcgmAdapter], runner: FakeRunner,
                                                   mock: respx.MockRouter) -> None:
    mock.get(f"{EXPORTER}/metrics").mock(side_effect=httpx.ConnectError("refused"))
    runner.rules.insert(0, (is_inspect, lambda _a: bad("Error: No such object: x")))
    a = make_adapter()
    st = await a.status()
    assert st.health == "down" and "dcgm.exporter.down" in codes(st)
    cont = next(c for c in st.components if c.kind == "container")
    assert cont.state == "absent"
    snap = await a.metrics()
    assert snap.available and all(p.key.startswith(("dcgm.fleet", "dcgm.host")) for p in snap.points)
    assert snap.error and "not answering" in snap.error


async def test_docker_missing_is_unknown_not_crash(make_adapter: Callable[..., DcgmAdapter]) -> None:
    st = await make_adapter(FakeRunner()).status()          # every command: rc 127
    assert next(c for c in st.components if c.kind == "container").state == "unknown"
    assert st.health in {"up", "degraded"}


async def test_a_down_spark_degrades_with_warn(make_adapter: Callable[..., DcgmAdapter], mock: respx.MockRouter) -> None:
    d = json.loads(fx("prom_up.json"))
    for r in d["data"]["result"]:
        if r["metric"].get("host") == "spark3":
            r["value"][1] = "0"
    mock.get(f"{PROM}/api/v1/query").mock(side_effect=prom_handler({"up{": json.dumps(d)}))
    st = await make_adapter().status()
    assert st.health == "degraded"
    p = next(p for p in st.problems if p.code == "dcgm.host.down")
    assert p.severity == "warn" and "spark3" in p.message and "spark1" not in p.message
    assert next(c for c in st.components if c.name.startswith("spark3")).state == "stopped"


async def test_prometheus_down_is_a_warning(make_adapter: Callable[..., DcgmAdapter], mock: respx.MockRouter) -> None:
    mock.get(f"{PROM}/api/v1/query").mock(side_effect=httpx.ConnectError("refused"))
    a = make_adapter()
    st = await a.status()
    assert st.health == "degraded" and "dcgm.prometheus.unreachable" in codes(st)
    snap = await a.metrics()
    assert snap.available and snap.error and "Prometheus" in snap.error
    assert not any(p.key.startswith("dcgm.fleet") for p in snap.points)


async def test_missing_job_target_is_reported(make_adapter: Callable[..., DcgmAdapter], mock: respx.MockRouter) -> None:
    d = json.loads(fx("prom_up.json"))
    d["data"]["result"] = [r for r in d["data"]["result"] if r["metric"]["job"] != "dcgm-aitop100"]
    mock.get(f"{PROM}/api/v1/query").mock(side_effect=prom_handler({"up{": json.dumps(d)}))
    st = await make_adapter().status()
    assert any("dcgm-aitop100 (no target)" in p.message for p in st.problems)


async def test_diag_image_missing_is_info(make_adapter: Callable[..., DcgmAdapter], runner: FakeRunner) -> None:
    runner.rules.insert(0, (is_image_inspect, lambda _a: bad("Error: No such image")))
    st = await make_adapter().status()
    p = next(p for p in st.problems if p.code == "dcgm.diag.image_missing")
    assert p.severity == "info" and (p.fix or "").startswith("docker pull nvcr.io/nvidia/cloud-native/dcgm:4.6.1-1")
    assert st.health == "up"


async def test_metrics_real_scrape(adapter: DcgmAdapter) -> None:
    snap = await adapter.metrics()
    assert snap.available and snap.system == "dcgm" and snap.error is None
    by = {(p.key, p.labels.get("gpu")): p for p in snap.points if p.key.startswith("dcgm.") and "gpu" in p.labels}
    t = by[("dcgm.gpu_temp_c", "0")]
    assert t.unit == "C" and 20 < t.value < 100 and t.labels["model"].startswith("RTX-PRO-6000-Blackwell-Max-Q")
    assert by[("dcgm.gpu_temp_c", "1")].labels["model"] == "RTX-PRO-6000-Blackwell-Workstation"
    raw = {lb["gpu"]: v for lb, v in parse_exposition(fx("metrics.txt"))["DCGM_FI_DEV_FB_USED"]}
    assert by[("dcgm.fb_used_b", "0")].value == raw["0"] * 1048576 and by[("dcgm.fb_used_b", "0")].unit == "B"
    assert by[("dcgm.gpu_util", "0")].unit == "ratio" and 0 <= by[("dcgm.gpu_util", "0")].value <= 1
    assert ("dcgm.pcie_replay_total", "0") in by and ("dcgm.sm_clock_mhz", "1") in by
    assert ("dcgm.tensor_active", "0") in by                                   # DCP family present in the scrape
    assert not any(k == "dcgm.xid_last" for k, _ in by)                         # never fabricated
    assert not any(k in {"dcgm.mem_temp_c"} for k, _ in by)
    for p in snap.points:                                                       # bounded, identifier-free labels
        assert set(p.labels) <= {"gpu", "model", "host", "job"}
        assert all(len(v) <= 40 for v in p.labels.values())
    blob = snap.model_dump_json()
    assert "GPU-" not in blob and "UUID" not in blob and "10.100" not in blob


async def test_metrics_fleet_rollups(adapter: DcgmAdapter) -> None:
    snap = await adapter.metrics()
    fleet = {(p.key, p.labels["host"]): p.value for p in snap.points if p.key.startswith("dcgm.fleet")}
    assert fleet[("dcgm.fleet_gpus", "local")] == 2 and fleet[("dcgm.fleet_gpus", "spark1")] == 1
    assert fleet[("dcgm.fleet_temp_max_c", "aitop100")] == 28
    local = next(r for r in json.loads(fx("prom_power.json"))["data"]["result"] if "host" not in r["metric"])
    assert fleet[("dcgm.fleet_power_w", "local")] == pytest.approx(float(local["value"][1]))
    ups = [p for p in snap.points if p.key == "dcgm.host_up"]
    assert len(ups) == 6 and all(p.value == 1.0 for p in ups)


async def test_metrics_unavailable_when_nothing_answers(make_adapter: Callable[..., DcgmAdapter],
                                                        mock: respx.MockRouter) -> None:
    mock.get(f"{EXPORTER}/metrics").mock(side_effect=httpx.ConnectError("refused"))
    mock.get(f"{PROM}/api/v1/query").mock(side_effect=httpx.ConnectError("refused"))
    snap = await make_adapter().metrics()
    assert not snap.available and snap.points == [] and snap.error


async def test_metrics_garbage_prometheus_json_tolerated(make_adapter: Callable[..., DcgmAdapter],
                                                         mock: respx.MockRouter) -> None:
    mock.get(f"{PROM}/api/v1/query").respond(200, text='{"status":"success","data":{"result":[{"metric":{},"value":[1]}]}}')
    snap = await make_adapter().metrics()
    assert snap.available and any(p.key == "dcgm.gpu_temp_c" for p in snap.points)


async def test_xid_event_problem_and_dedupe(make_adapter: Callable[..., DcgmAdapter], mock: respx.MockRouter) -> None:
    a = make_adapter()
    assert await a.recent_events() == []
    mock.get(f"{EXPORTER}/metrics").respond(200, text=xid_metrics(79))
    st = await a.status()
    p = next(p for p in st.problems if p.code == "dcgm.xid.nonzero")
    assert p.severity == "warn" and "GPU 1: 79" in p.message and st.health == "degraded"
    snap = await a.metrics()                                    # same value again: no second event
    assert any(x.key == "dcgm.xid_last" and x.value == 79 for x in snap.points)
    evs = await a.recent_events()
    assert len(evs) == 1 and evs[0].kind == "dcgm.xid" and evs[0].data["xid"] == 79 and evs[0].data["gpu"] == "1"
    mock.get(f"{EXPORTER}/metrics").respond(200, text=xid_metrics(48))
    await a.metrics()
    assert [e.data["xid"] for e in await a.recent_events()] == [48, 79] or len(await a.recent_events()) == 2


async def test_xid_total_counter_increase(make_adapter: Callable[..., DcgmAdapter], mock: respx.MockRouter) -> None:
    line = lambda n: ('DCGM_EXP_XID_ERRORS_TOTAL{gpu="0",modelName="NVIDIA X"} ' + str(n) + "\n")
    a = make_adapter()
    mock.get(f"{EXPORTER}/metrics").respond(200, text=fx("metrics.txt") + line(0))
    await a.metrics()
    mock.get(f"{EXPORTER}/metrics").respond(200, text=fx("metrics.txt") + line(2))
    await a.metrics()
    evs = await a.recent_events()
    assert len(evs) == 1 and evs[0].data["new"] == 2 and "2 new XID" in evs[0].summary


async def test_follow_events_yields_new_xid_and_cancels(make_adapter: Callable[..., DcgmAdapter],
                                                        mock: respx.MockRouter) -> None:
    import asyncio
    a = make_adapter()
    got: list[str] = []

    async def consume() -> None:
        async for ev in a.follow_events():
            got.append(ev.kind)
            return

    task = asyncio.create_task(consume())
    await asyncio.sleep(0.1)                                    # first pass: baseline, nothing to yield
    assert not got
    mock.get(f"{EXPORTER}/metrics").respond(200, text=xid_metrics(61))
    await asyncio.wait_for(task, 3.0)
    assert got == ["dcgm.xid"]

    async def forever() -> None:
        async for _ in a.follow_events():
            pass
    t2 = asyncio.create_task(forever())
    await asyncio.sleep(0.05)
    t2.cancel()
    with pytest.raises(asyncio.CancelledError):
        await t2


async def test_every_metric_point_forms_a_history_key_the_api_accepts(adapter: DcgmAdapter) -> None:
    """Found against the real console 2026-10-24: a model label with spaces ('RTX PRO 6000 ...') made every per-GPU
    series un-chartable (the history endpoint answered 422). Each point's series key must match KEY_RE."""
    from nemo_console.metrics_keys import KEY_RE
    from nemo_console.services.metrics import series_name

    snap = await adapter.metrics()
    assert snap.points
    assert [series_name(p) for p in snap.points if not KEY_RE.fullmatch(series_name(p))] == []
    models = {p.labels["model"] for p in snap.points if "model" in p.labels}
    assert models and all(" " not in m for m in models)
