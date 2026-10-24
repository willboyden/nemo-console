from __future__ import annotations

import time

import httpx
import pytest

from nemo_console.domain.models import MetricPoint, SystemEvent
from nemo_console.services.metrics import MetricsPoller
from nemo_console.services.routing import summarize

from .conftest import Env


async def test_health_overview_systems_topology(env: Env) -> None:
    async with env.client() as c:
        h = (await c.get("/api/v1/health")).json()
        assert h["status"] == "ok" and h["trust"] == "loopback-admin" and set(h["systems"]) == {"switchyard", "relay", "dynamo", "skillevaluator", "datadesigner"}
        o = (await c.get("/api/v1/overview")).json()
        assert len(o["systems"]) == 5 and {n["id"] for n in o["topology"]["nodes"]} >= {"litellm", "switchyard", "relay"}
        assert len((await c.get("/api/v1/systems")).json()) == 5
        assert (await c.get("/api/v1/systems/dynamo/status")).json()["system"] == "dynamo"
        t = (await c.get("/api/v1/topology")).json()
        assert next(n for n in t["nodes"] if n["id"] == "litellm")["health"] == "up"
        # the datadesigner adapter's version probe may run first; a docker inspect (engine probes) must be among the calls
        assert env.runner.calls and any(c[:2] == ["docker", "inspect"] for c in env.runner.calls)


async def test_down_system_is_200_not_5xx(env: Env) -> None:
    env.adapters["dynamo"].health = "down"
    env.container.status.invalidate("dynamo")
    async with env.client() as c:
        r = await c.get("/api/v1/systems/dynamo/status")
        assert r.status_code == 200 and r.json()["health"] == "down"
        assert (await c.get("/api/v1/health")).json()["status"] == "degraded"


async def test_overall_health_ignores_systems_that_are_off(env: Env) -> None:
    """Addendum 9.4: an opt-in system that is off on purpose (Dynamo) must not keep /health `degraded` for good."""
    env.adapters["dynamo"].health = "off"
    env.container.status.invalidate("dynamo")
    async with env.client() as c:
        h = (await c.get("/api/v1/health")).json()
        assert h["systems"]["dynamo"] == "off" and h["status"] == "ok"
        env.adapters["relay"].health = "degraded"
        env.container.status.invalidate("relay")
        assert (await c.get("/api/v1/health")).json()["status"] == "degraded"     # off is ignored, nothing else is


async def test_topology_probe_failures_are_data(env: Env) -> None:
    env.mock.get("http://127.0.0.1:4000/health/liveliness").mock(side_effect=httpx.ConnectError("x"))
    async with env.client() as c:
        t = (await c.get("/api/v1/topology")).json()
    assert next(n for n in t["nodes"] if n["id"] == "litellm")["health"] == "down"


async def test_missing_adapter_module_is_unknown_with_problem(env: Env, monkeypatch: pytest.MonkeyPatch, tmp_path: object) -> None:
    from nemo_console import adapters
    from nemo_console.container import build_container
    from nemo_console.main import create_app
    monkeypatch.setitem(adapters._REGISTRY, "relay", ("nemo_console.adapters.does_not_exist", "X"))
    env.settings.state_dir = env.tmp / "state2"
    c = build_container(env.settings, adapters={"switchyard": env.adapters["switchyard"], "dynamo": env.adapters["dynamo"]},  # type: ignore[dict-item]
                        runner=env.runner, http=env.http)
    app = create_app(env.settings, c)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=("127.0.0.1", 1)), base_url="http://127.0.0.1:8793",
                                 headers={"X-Console": "1"}) as cl:
        st = (await cl.get("/api/v1/systems/relay/status")).json()
        assert st["health"] == "unknown" and "console.adapter.missing" in [p["code"] for p in st["problems"]]
        assert (await cl.get("/api/v1/systems/relay/metrics")).json()["available"] is False
        assert (await cl.get("/api/v1/systems/relay/events")).json() == []
        assert (await cl.get("/api/v1/systems/relay/config")).json() == []
        assert (await cl.get("/api/v1/systems/relay/actions")).json() == []
        assert (await cl.get("/api/v1/systems/relay/params")).json()["catalog"] == []
        assert (await cl.get("/api/v1/health")).json()["systems"]["relay"] == "unknown"
        assert (await cl.get("/api/v1/overview")).status_code == 200


async def test_adapter_exception_becomes_unknown_health(env: Env) -> None:
    env.adapters["relay"].status_error = ValueError("bad")
    env.container.status.invalidate("relay")
    async with env.client() as c:
        st = (await c.get("/api/v1/systems/relay/status")).json()
    assert st["health"] == "unknown"


async def test_metrics_live_and_history(env: Env) -> None:
    p = env.container.metrics
    base = time.time() // 60 * 60 - 120          # inside ONE past minute bucket, so the rollup average is deterministic
    for i in range(5):
        p.record("switchyard", base + i * 2, [MetricPoint(key="sy.tokens", value=1, labels={"kind": "prompt"}),
                                                  MetricPoint(key="sy.tokens", value=2, labels={"kind": "completion"}),
                                                  MetricPoint(key="sy.requests_total", value=i)])
    async with env.client() as c:
        live = (await c.get("/api/v1/systems/switchyard/metrics")).json()
        assert live["available"] and live["points"]
        h = (await c.get("/api/v1/systems/switchyard/metrics/history?window=15m&key=sy.requests_total")).json()
        assert [v for _, v in h["points"]] == [0, 1, 2, 3, 4]
        summed = (await c.get("/api/v1/systems/switchyard/metrics/history?key=sy.tokens")).json()["points"]
        assert [v for _, v in summed] == [3] * 5                                   # label variants summed
        one = (await c.get("/api/v1/systems/switchyard/metrics/history?key=sy.tokens{kind=completion}")).json()["points"]
        assert [v for _, v in one] == [2] * 5
        hr = (await c.get("/api/v1/systems/switchyard/metrics/history?window=1h&key=sy.requests_total")).json()
        assert hr["points"] and hr["points"][-1][1] == 2.0                          # 1-min rollup average of 0..4
        for bad in ("window=2h&key=sy.x", "key=Bad Key", "key=", "window=15m&key=" + "a" * 300):
            assert (await c.get(f"/api/v1/systems/switchyard/metrics/history?{bad}")).status_code == 422


async def test_ring_buffer_and_series_are_bounded(env: Env) -> None:
    p = MetricsPoller(env.container.status, env.container.store, interval=2.0, ring_seconds=10)
    for i in range(50):
        p.record("dynamo", 1000 + i, [MetricPoint(key="dyn.up", value=1)])
    assert len(p.ring[("dynamo", "dyn.up")]) == p.maxlen == 6
    for i in range(400):
        p.record("dynamo", 2000, [MetricPoint(key="dyn.m", value=1, labels={"id": str(i)})])
    assert sum(1 for k in p.ring if k[0] == "dynamo") <= 256


async def test_poller_tick_uses_adapters(env: Env) -> None:
    await env.container.metrics.tick()
    assert ("switchyard", "fake.requests_total") in env.container.metrics.ring


async def test_events_endpoint_limit_and_validation(env: Env) -> None:
    env.adapters["switchyard"].events = [SystemEvent(ts=float(i), system="switchyard", kind="k", summary="s") for i in range(10)]
    async with env.client() as c:
        assert len((await c.get("/api/v1/systems/switchyard/events?limit=3")).json()) == 3
        assert (await c.get("/api/v1/systems/switchyard/events?limit=0")).status_code == 422
        assert (await c.get("/api/v1/systems/switchyard/events?limit=99999")).status_code == 422


def _dec(route: str, model: str, tier: str, p: int, c: int, ts: float, **extra: object) -> SystemEvent:
    return SystemEvent(ts=ts, system="switchyard", kind="routing.decision", summary="x",
                       data={"route": route, "model": model, "tier": tier, "prompt_tokens": p, "completion_tokens": c, **extra})


async def test_routing_summary(env: Env) -> None:
    now = time.time()
    env.adapters["switchyard"].events = [
        _dec("auto-local", "qwen", "capable", 10, 5, now - 10), _dec("auto-local", "nemo", "efficient", 3, 1, now - 20),
        _dec("direct-27b", "qwen", "capable", 1, 1, now - 30, status=500),
        _dec("old", "qwen", "capable", 100, 100, now - 7200),
        SystemEvent(ts=now, system="switchyard", kind="other", summary="ignored")]
    async with env.client() as c:
        s = (await c.get("/api/v1/routing/summary?window=1h")).json()
        assert s["requests"] == 3 and s["errors"] == 1 and s["by_route"] == {"auto-local": 2, "direct-27b": 1}
        assert s["by_model"] == {"qwen": 2, "nemo": 1} and s["by_tier"] == {"capable": 2, "efficient": 1}
        assert s["tokens"] == {"prompt": 14, "completion": 7, "total": 21}
        assert (await c.get("/api/v1/routing/summary?window=24h")).json()["requests"] == 4
        assert (await c.get("/api/v1/routing/summary?window=9y")).status_code == 422
    assert summarize([], "1h")["requests"] == 0


async def test_audit_is_append_only_and_records_mutations(env: Env) -> None:
    async with env.client() as c:
        await c.put("/api/v1/settings", json={"poll_interval_s": 5})
        await c.post("/api/v1/systems/dynamo/actions/reset", json={"confirm": False})
        rows = (await c.get("/api/v1/audit")).json()
        assert [r["status"] for r in rows[:2]] == [400, 200]
        assert rows[0]["system"] == "dynamo" and rows[0]["actor"] == "loopback"
        assert len((await c.get("/api/v1/audit?system=dynamo")).json()) == 1
        assert (await c.get("/api/v1/audit?system=zzz")).status_code == 422
        assert (await c.get("/api/v1/audit?limit=501")).status_code == 422
    import sqlite3
    with pytest.raises(sqlite3.DatabaseError):
        env.container.store.execute("DELETE FROM audit")
    with pytest.raises(sqlite3.DatabaseError):
        env.container.store.execute("UPDATE audit SET status=1")


async def test_settings_roundtrip_and_validation(env: Env) -> None:
    async with env.client() as c:
        v = (await c.get("/api/v1/settings")).json()
        assert v["poll_interval_s"] == 2.0 and v["urls"]["switchyard"].startswith("http://127.0.0.1")
        r = await c.put("/api/v1/settings", json={"poll_interval_s": 7, "urls": {"dynamo": "http://localhost:8021"},
                                                  "playground_key_env": {"switchyard": "MY_API_KEY"}})
        assert r.status_code == 200 and r.json()["restart_required"] is True
        v = (await c.get("/api/v1/settings")).json()
        assert v["poll_interval_s"] == 7 and v["urls"]["dynamo"] == "http://localhost:8021"
        assert v["playground_key_env"]["switchyard"] == "MY_API_KEY" and v["tokens"]["switchyard"] == "[unset]"
        for bad in ({"urls": {"dynamo": "http://evil.example:80"}}, {"urls": {"dynamo": "https://127.0.0.1"}},
                    {"urls": {"nope": "http://127.0.0.1:1"}}, {"poll_interval_s": 0.01}, {"playground_key_env": {"a": "lower"}},
                    {"urls": {"dynamo": "http://user:pw@127.0.0.1:1"}}, {"state_dir": "/tmp"}):
            assert (await c.put("/api/v1/settings", json=bad)).status_code == 422, bad
    assert env.container.metrics.interval == 7


async def test_console_metrics_endpoint(env: Env) -> None:
    async with env.client() as c:
        await c.get("/api/v1/systems")
        r = await c.get("/metrics")
    assert r.status_code == 200 and "nemo_console_http_requests_total" in r.text
    async with env.client(client_host="10.0.0.9") as c:
        assert (await c.get("/metrics")).status_code == 401


async def test_health_reports_caller_role(env: Env) -> None:
    _, vk = env.container.auth.mint("viewer")
    _, ak = env.container.auth.mint("admin")
    async with env.client() as c:
        assert (await c.get("/api/v1/health")).json()["role"] == "admin"          # loopback trust
    async with env.client(client_host="10.0.0.9", token=vk) as c:
        assert (await c.get("/api/v1/health")).json()["role"] == "viewer"
    async with env.client(client_host="10.0.0.9", token=ak) as c:
        assert (await c.get("/api/v1/health")).json()["role"] == "admin"


async def test_real_adapters_with_no_paths_give_problems_not_500(env: Env) -> None:
    """An empty ctx.paths (missing keys) must degrade to Problems / 404s, never a 500."""
    from nemo_console.adapters import build_adapter
    from nemo_console.adapters.base import AdapterContext
    from nemo_console.container import build_container
    from nemo_console.main import create_app
    ctx = AdapterContext(runner=env.runner, http=env.http, urls={}, paths={}, state_dir=env.tmp / "st3")
    real = {sid: build_adapter(sid, ctx) for sid in ("switchyard", "relay", "dynamo", "skillevaluator")}
    env.settings.state_dir = env.tmp / "state3"
    app = create_app(env.settings, build_container(env.settings, adapters=real, runner=env.runner, http=env.http))  # type: ignore[arg-type]
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=("127.0.0.1", 1)), base_url="http://127.0.0.1:8793",
                                 headers={"X-Console": "1"}) as cl:
        for sid in real:
            for path in ("status", "metrics", "events", "config", "actions", "params", "playground"):
                r = await cl.get(f"/api/v1/systems/{sid}/{path}")
                assert r.status_code < 500, (sid, path, r.status_code, r.text[:200])
            st = (await cl.get(f"/api/v1/systems/{sid}/status")).json()
            assert st["health"] in ("down", "unknown", "degraded", "up", "off")
            for f in (await cl.get(f"/api/v1/systems/{sid}/config")).json():
                r = await cl.get(f"/api/v1/systems/{sid}/config/{f['id']}")
                assert r.status_code < 500, (sid, f["id"], r.text[:200])
        assert (await cl.get("/api/v1/overview")).status_code == 200
