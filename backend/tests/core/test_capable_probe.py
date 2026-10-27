"""Addendum 9.11: the opt-in active capable-tier probe (one 1-token completion per alias, in the background)."""
from __future__ import annotations

import asyncio
import json
import logging
import time
from pathlib import Path

import httpx
import pytest
import respx

from nemo_console.adapters.base import CommandResult
from nemo_console.config import Settings
from nemo_console.container import build_container
from nemo_console.main import create_app
from nemo_console.services.common import KNOWN
from nemo_console.services.engines import CapableProbe, ProbeResult, probe_problems

from .conftest import BASE, FAKE_SECRET, Env

URL = "http://127.0.0.1:4000/v1/chat/completions"
ALIAS = "qwen3.8-flash-next"
CONTAINER = "ai-lab-sglang-flashnext-flashnext-1"
ROUTES = """
[targets.lightning]
id = "nemotron-lightning"
llm_client = "litellm"

[targets.flash-next]
id = "qwen3.8-flash-next"
llm_client = "litellm"

[routes.auto-local]
id = "auto-local"
type = "stage_router"
capable_target = "flash-next"
efficient_target = "lightning"
"""
OK = {"choices": [{"index": 0, "message": {"role": "assistant", "content": "SECRET-ANSWER-TEXT"}}]}


def _probe(http: httpx.AsyncClient, *, env: dict[str, str] | None = None, interval: float = 300.0) -> CapableProbe:
    return CapableProbe(http, "http://127.0.0.1:4000", "LITELLM_API_KEY", interval_s=interval, timeout_s=2.0,
                        retry_delay_s=0.0, environ={"LITELLM_API_KEY": FAKE_SECRET} if env is None else env)


# ── one probe request ─────────────────────────────────────────────────────────────────────────────
@respx.mock
async def test_ok_sends_one_tiny_completion_with_the_named_credential() -> None:
    route = respx.post(URL).respond(200, json=OK)
    async with httpx.AsyncClient(trust_env=False) as http:
        res = await _probe(http).probe(ALIAS)
    assert res.verdict == "ok" and res.detail == "answered"
    req = route.calls.last.request
    body = json.loads(req.content)
    assert body == {"model": ALIAS, "messages": [{"role": "user", "content": "ping"}], "max_tokens": 1,
                    "temperature": 0, "stream": False, "cache": {"no-cache": True}}   # never a cached answer
    assert req.headers["authorization"] == f"Bearer {FAKE_SECRET}"
    assert FAKE_SECRET in KNOWN._values                       # registered with the redactor before use


@pytest.mark.parametrize("respond,verdict,detail", [
    ({"status_code": 503, "json": {"error": "x"}}, "broken", "router answered 503"),
    ({"status_code": 400, "json": {"error": "Invalid model name"}}, "broken", "router answered 400"),
    ({"status_code": 200, "json": {"choices": []}}, "broken", "no choices in the answer"),
    ({"status_code": 200, "text": "not json"}, "broken", "no choices in the answer"),
    ({"status_code": 401, "json": {"error": "no key"}}, "denied", "router answered 401"),
    ({"status_code": 403, "json": {"error": "not allowed"}}, "denied", "router answered 403"),
])
@respx.mock
async def test_verdicts(respond: dict[str, object], verdict: str, detail: str) -> None:
    respx.post(URL).respond(**respond)  # type: ignore[arg-type]
    async with httpx.AsyncClient(trust_env=False) as http:
        res = await _probe(http).probe(ALIAS)
    assert (res.verdict, res.detail) == (verdict, detail)


@respx.mock
async def test_timeout_and_unreachable_are_broken() -> None:
    respx.post(URL).mock(side_effect=httpx.ReadTimeout("slow"))
    async with httpx.AsyncClient(trust_env=False) as http:
        res = await _probe(http).probe(ALIAS)
    assert res.verdict == "broken" and res.detail == "no answer within 2 s"
    respx.post(URL).mock(side_effect=httpx.ConnectError("refused"))
    async with httpx.AsyncClient(trust_env=False) as http:
        res = await _probe(http).probe(ALIAS)
    assert res.verdict == "broken" and res.detail == "router unreachable (ConnectError)"


@respx.mock
async def test_without_the_credential_nothing_is_sent() -> None:
    route = respx.post(URL).respond(200, json=OK)
    async with httpx.AsyncClient(trust_env=False) as http:
        res = await _probe(http, env={}).probe(ALIAS)
    assert res.verdict == "unconfigured" and "LITELLM_API_KEY" in res.detail
    assert route.call_count == 0


@respx.mock
async def test_logs_carry_status_only(caplog: pytest.LogCaptureFixture) -> None:
    respx.post(URL).respond(200, json=OK)
    caplog.set_level(logging.INFO, logger="nemo_console.services.engines")
    async with httpx.AsyncClient(trust_env=False) as http:
        await _probe(http).probe(ALIAS)
    assert "capable probe qwen3.8-flash-next: ok" in caplog.text
    assert "SECRET-ANSWER-TEXT" not in caplog.text and "ping" not in caplog.text and FAKE_SECRET not in caplog.text


# ── scheduling: background, rate-limited, one at a time ───────────────────────────────────────────
@respx.mock
async def test_schedule_is_rate_limited_and_sequential() -> None:
    in_flight, peak = 0, 0

    async def answer(_req: httpx.Request) -> httpx.Response:
        nonlocal in_flight, peak
        in_flight += 1
        peak = max(peak, in_flight)
        await asyncio.sleep(0.01)
        in_flight -= 1
        return httpx.Response(200, json=OK)

    route = respx.post(URL).mock(side_effect=answer)
    async with httpx.AsyncClient(trust_env=False) as http:
        p = _probe(http)
        p.schedule(["a", "b", "c"])
        p.schedule(["a", "b", "c"])                      # a pass is running: ignored
        assert p._task is not None
        await p._task
        assert route.call_count == 3 and peak == 1      # one request per alias, never two at once
        p.schedule(["a", "b", "c"])                      # every result is fresh: nothing is due
        assert route.call_count == 3
        p._results["b"] = ProbeResult("ok", "answered", time.time() - 301)   # b's result is older than the interval
        p.schedule(["a", "b", "c"])
        assert p._task is not None
        await p._task
        assert route.call_count == 4
        await p.aclose()


def test_interval_below_five_minutes_is_refused() -> None:
    with pytest.raises(ValueError):
        Settings(capable_probe_interval_s=299.0)


# ── problems ──────────────────────────────────────────────────────────────────────────────────────
def test_probe_problems(tmp_path: Path) -> None:
    running = [(ALIAS, CONTAINER, ["auto-local"]), ("other", "c-2", ["r2", "r3"])]
    now = time.time()
    assert probe_problems({ALIAS: ProbeResult("ok", "answered", now)}, running, tmp_path) == []
    [p] = probe_problems({ALIAS: ProbeResult("broken", "router answered 503", now)}, running, tmp_path)
    assert p.severity == "warn" and p.code == "switchyard.tier.capable_broken"
    assert f"{ALIAS} runs (container {CONTAINER})" in p.message and "router answered 503" in p.message
    assert "Route auto-local cannot escalate" in p.message
    assert p.fix is not None and p.fix.startswith(f"docker logs --tail 80 {CONTAINER}")
    probs = probe_problems({ALIAS: ProbeResult("unconfigured", "env var X is not set", now),
                            "other": ProbeResult("unconfigured", "env var X is not set", now)}, running, tmp_path)
    assert [(p.severity, p.code) for p in probs] == [("info", "switchyard.tier.probe_unconfigured")]   # one, not two
    [d] = probe_problems({ALIAS: ProbeResult("denied", "router answered 401", now)}, running, tmp_path)
    assert d.code == "switchyard.tier.probe_denied" and d.severity == "info"
    assert probe_problems({"not-running": ProbeResult("broken", "x", now)}, running, tmp_path) == []


# ── end to end through the Switchyard status ──────────────────────────────────────────────────────
async def _switchyard(app: object) -> dict:  # type: ignore[type-arg]
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url=BASE,  # type: ignore[arg-type]
                                 headers={"X-Console": "1"}) as c:
        return (await c.get("/api/v1/systems/switchyard/status")).json()


async def test_off_by_default_sends_nothing(env: Env) -> None:
    env.cfg_file.write_text(ROUTES)
    route = env.mock.post(URL).respond(200, json=OK)
    env.container.status.invalidate("switchyard")
    st = await _switchyard(env.app)
    assert env.container.capable_probe is None and route.call_count == 0 and st["health"] == "up"


async def test_running_but_broken_engine_degrades_switchyard(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    env.cfg_file.write_text(ROUTES)
    monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
    env.runner.result = CommandResult(0, "running\n", "")
    route = env.mock.post(URL).respond(503, json={"error": "rank 1 died"})
    cfg = Settings(state_dir=env.tmp / "state2", frontend_dir=env.tmp / "front", background_tasks=False,
                   trust_loopback=True, capable_probe=True, capable_probe_retry_s=0.0)
    c = build_container(cfg, adapters=dict(env.adapters), runner=env.runner, http=env.http)  # type: ignore[arg-type]
    app = create_app(cfg, c)
    first = await _switchyard(app)                       # schedules the background pass; reports nothing yet
    assert "switchyard.tier.capable_broken" not in [p["code"] for p in first["problems"]]
    assert c.capable_probe is not None and c.capable_probe._task is not None
    await c.capable_probe._task
    c.status.invalidate("switchyard")
    st = await _switchyard(app)
    [p] = [p for p in st["problems"] if p["code"] == "switchyard.tier.capable_broken"]
    assert "router answered 503" in p["message"] and "rank 1 died" not in json.dumps(st)
    assert st["health"] == "degraded" and st["summary"].startswith("capable tier not answering")
    assert route.call_count == 2                         # one retry; the second status did not probe again
    assert json.loads(route.calls.last.request.content)["model"] == ALIAS
    await c.capable_probe.aclose()


@respx.mock
async def test_a_failure_is_retried_once_before_it_counts() -> None:
    route = respx.post(URL).mock(side_effect=[httpx.Response(503), httpx.Response(200, json=OK)])
    async with httpx.AsyncClient(trust_env=False) as http:
        p = _probe(http)
        p.schedule([ALIAS])
        assert p._task is not None
        await p._task
    assert route.call_count == 2 and p.results()[ALIAS].verdict == "ok"        # busy once, then answered: ok


@respx.mock
async def test_two_failures_are_broken_and_say_so() -> None:
    respx.post(URL).mock(side_effect=[httpx.Response(503), httpx.Response(502)])
    async with httpx.AsyncClient(trust_env=False) as http:
        p = _probe(http)
        p.schedule([ALIAS])
        assert p._task is not None
        await p._task
    res = p.results()[ALIAS]
    assert res.verdict == "broken" and res.detail == "router answered 502, twice 0 s apart"
