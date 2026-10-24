"""AIPerf `profile` action: the ONE background run, verdicts, run dirs, report, preflight refusals, and the core's
confirm + param checks. Offline: fakes + the captured live fixtures only."""
from __future__ import annotations

import asyncio
import json
import os
import stat
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import httpx
import pytest
from ap_helpers import LAUNCHER, FakeRunner, fx, make_ctx, ok, standard_runner, writes_profile, writes_records

from nemo_console.adapters.aiperf import KEEP_RUNS, AiPerfAdapter, request_errors
from nemo_console.config import NO_USER_ENV, Settings
from nemo_console.container import build_container
from nemo_console.main import create_app

SMOKE = {"preset": "smoke-lightning"}


def codes(res) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in res.problems}


async def finish(a: AiPerfAdapter) -> None:
    assert a._live is not None
    await asyncio.wait_for(a._live.task, 5)


def run_json(a: AiPerfAdapter, run_id: str) -> dict[str, Any]:
    return json.loads((a._runs_dir / run_id / "run.json").read_text())


async def run_to_end(a: AiPerfAdapter, params: dict[str, object] | None = None) -> tuple[str, dict[str, Any]]:
    res = await a.run_action("profile", params or dict(SMOKE))
    assert res.ok, res.output
    rid = str(a._live_run_id())
    await finish(a)
    return rid, run_json(a, rid)


# ── start in the background, golden launch args ───────────────────────────────────────────────
async def test_profile_starts_in_the_background_with_golden_args(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()

    async def gated(argv: list[str]) -> Any:
        await gate.wait()
        return writes_profile()(argv)
    run.on([LAUNCHER, "profile"], gated)
    a, _ = make_adapter(run)
    res = await asyncio.wait_for(a.run_action("profile", dict(SMOKE)), 2)           # does not wait for the run
    assert res.ok and "Started load run" in res.output and "in the background" in res.output
    rid = a._live_run_id()
    assert rid is not None and rid.endswith("-smoke-lightning")
    run_dir = a._runs_dir / rid
    meta = run_json(a, rid)
    assert meta["verdict"] == "running" and meta["preset"] == "smoke-lightning" and meta["concurrency"] == 2
    assert stat.S_IMODE(run_dir.stat().st_mode) == 0o700 and stat.S_IMODE((run_dir / "run.json").stat().st_mode) == 0o600
    await asyncio.sleep(0.01)
    [call] = [c for c in run.calls if c[1:2] == ["profile"]]
    assert call == [
        "aiperf-local", "profile", "--model", "aitop/nemotron-lightning", "--endpoint-type", "chat", "--streaming",
        "--concurrency", "2", "--request-count", "8", "--prompt-input-tokens-mean", "128",
        "--prompt-output-tokens-mean", "64", "--warmup-request-count", "1", "--request-timeout-seconds", "120",
        "--random-seed", "42", "--export-level", "records", "--use-server-token-count",
        "--artifact-dir", str(run_dir / "artifacts")]
    assert run.cwds[run.calls.index(call)] == run_dir and run.timeouts[run.calls.index(call)] == 3600.0
    [ev] = await a.recent_events()
    assert ev.data["verdict"] == "running"
    gate.set()
    await finish(a)
    assert run_json(a, rid)["verdict"] == "succeeded"


async def test_one_run_at_a_time(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()

    async def gated(argv: list[str]) -> Any:
        await gate.wait()
        return writes_profile()(argv)
    run.on([LAUNCHER, "profile"], gated)
    a, _ = make_adapter(run)
    assert (await a.run_action("profile", dict(SMOKE))).ok
    second = await a.run_action("profile", {"preset": "latency-lightning"})
    assert not second.ok and "ap.profile.busy" in codes(second)
    assert len([c for c in run.calls if c[1:2] == ["profile"]]) <= 1
    st = await a.status()
    assert any(c.name == "profile smoke-lightning" and c.state == "running" for c in st.components)
    assert (await a.metrics()).points[1].key == "ap.live_running" and (await a.metrics()).points[1].value == 1.0
    gate.set()
    await finish(a)
    assert (await a.run_action("profile", dict(SMOKE))).ok        # free again
    await finish(a)


# ── verdicts ───────────────────────────────────────────────────────────────────────────────────
async def test_success_parses_the_captured_artifacts(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    _, meta = await run_to_end(a)
    assert meta["verdict"] == "succeeded" and meta["aiperf_version"] == "0.13.0" and meta["duration_ms"] >= 0
    assert meta["metrics"]["ttft_p50_ms"] == pytest.approx(29.9637365)
    assert meta["metrics"]["output_tps"] == pytest.approx(1532.9910176367441) and meta["metrics"]["requests"] == 8
    assert "stdout" not in meta and "api_key" not in json.dumps(meta)
    snap = await a.metrics()
    by_key = {(p.key, tuple(sorted(p.labels.items()))): p.value for p in snap.points}
    assert by_key[("ap.ttft_ms", (("q", "p99"),))] == pytest.approx(40.695334159999994)
    assert by_key[("ap.runs", ())] == 1.0 and by_key[("ap.runs_by_verdict", (("verdict", "succeeded"),))] == 1.0
    [ev] = await a.recent_events()
    assert ev.data["verdict"] == "succeeded" and ev.data["ttft_p50_ms"] == pytest.approx(29.9637365)
    assert "TTFT p50 30 ms" in ev.summary


async def test_failure_text_is_cleaned_of_boxes_and_ansi(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([LAUNCHER, "profile"], ok("", rc=1, stderr="\x1b[31m" + fx("all-failed.txt") + "\x1b[0m"))
    a, _ = make_adapter(run)
    rid, meta = await run_to_end(a)
    assert meta["verdict"] == "failed" and "All 1 inference request(s) failed" in meta["error"]
    assert "│" not in meta["error"] and "\x1b" not in meta["error"] and "metrics" not in meta
    assert (await a.metrics()).points[0].value == 1.0
    ok_, text = a._report_text(rid)
    assert ok_ and "AIPerf did not complete the run" in text and "All 1 inference request(s) failed" in text


async def test_failure_reports_the_routers_own_reason_per_request(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([LAUNCHER, "profile"], writes_records())
    a, _ = make_adapter(run)
    rid, meta = await run_to_end(a)
    assert meta["verdict"] == "failed" and meta["failed_requests"] == 2
    assert "2 request(s) failed; the first: HTTP 400 Bad Request: /chat/completions: Invalid model name" in meta["error"]
    assert "no-such-model-xyz" in meta["error"] and "{" not in meta["error"]       # the inner message, not the JSON blob
    assert "Invalid model name" in a._report_text(rid)[1]


async def test_report_mentions_partial_failures(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    d = a._runs_dir / "20261024T100000-001-smoke-lightning"
    d.mkdir(parents=True)
    (d / "run.json").write_text(json.dumps({"run_id": d.name, "preset": "smoke-lightning", "verdict": "succeeded",
                                            "failed_requests": 3, "first_error": "HTTP 429 Too Many Requests: slow down",
                                            "metrics": {"ttft_p50_ms": 1.0}}))
    assert "3 request(s) failed; the first: HTTP 429 Too Many Requests: slow down" in a._report_text(d.name)[1]


async def test_request_errors_reader_is_defensive(tmp_path: Path) -> None:
    assert request_errors(tmp_path / "absent.jsonl") == (0, "")
    p = tmp_path / "r.jsonl"
    p.write_text('not json\n[1]\n{"metadata": {}, "metrics": {}}\n{"error": {"code": 401, "type": "Unauthorized", '
                 '"message": "Bearer sk-abcdefghijklmnopqrstuv0123456789 rejected"}}\n{"error": "str"}\n')
    n, first = request_errors(p)
    assert n == 1 and first.startswith("HTTP 401 Unauthorized:") and "sk-abcdefghijklmnop" not in first
    link = tmp_path / "link.jsonl"
    link.symlink_to(p)
    assert request_errors(link) == (0, "")


async def test_timeout_is_a_failed_run(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([LAUNCHER, "profile"], lambda argv: __import__("nemo_console.adapters.base", fromlist=["x"]).CommandResult(
        returncode=-9, stdout="", stderr="", timed_out=True))
    a, _ = make_adapter(run)
    _, meta = await run_to_end(a)
    assert meta["verdict"] == "failed" and "timed out" in meta["error"]


async def test_exit_zero_without_a_summary_is_an_error(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([LAUNCHER, "profile"], ok("done\n"))
    a, _ = make_adapter(run)
    _, meta = await run_to_end(a)
    assert meta["verdict"] == "error" and "wrote no readable profile_export_aiperf.json" in meta["error"]


@pytest.mark.parametrize(("patch", "verdict"), [
    ({"was_cancelled": True}, "interrupted"),
    ({"is_complete": False}, "failed"),
    ({"request_count": {"unit": "requests", "avg": 0.0}}, "error"),
])
async def test_summary_flags_decide_the_verdict(make_adapter, patch: dict[str, Any], verdict: str) -> None:  # type: ignore[no-untyped-def]
    doc = {**json.loads(fx("profile_export_aiperf.json")), **patch}
    run = standard_runner()
    run.on([LAUNCHER, "profile"], writes_profile(summary=json.dumps(doc)))
    a, _ = make_adapter(run)
    _, meta = await run_to_end(a)
    assert meta["verdict"] == verdict


async def test_a_garbage_summary_is_an_error_not_a_crash(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([LAUNCHER, "profile"], writes_profile(summary="{not json"))
    a, _ = make_adapter(run)
    _, meta = await run_to_end(a)
    assert meta["verdict"] == "error"


async def test_secrets_in_runner_output_never_reach_run_json_or_the_report(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([LAUNCHER, "profile"], ok("", rc=1, stderr="Error: 401 Authorization: Bearer sk-abcdefghijklmnopqrstuv0123456789"))
    a, _ = make_adapter(run)
    rid, _ = await run_to_end(a)
    raw = (a._runs_dir / rid / "run.json").read_text()
    assert "sk-abcdefghijklmnop" not in raw and "sk-abcdefghijklmnop" not in a._report_text(rid)[1]


async def test_console_stop_marks_the_run_interrupted(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    started = asyncio.Event()

    async def forever(argv: list[str]) -> Any:
        started.set()
        await asyncio.sleep(3600)
    run.on([LAUNCHER, "profile"], forever)
    a, _ = make_adapter(run)
    res = await a.run_action("profile", dict(SMOKE))
    rid = str(a._live_run_id())
    assert res.ok
    await asyncio.wait_for(started.wait(), 2)
    assert a._live is not None
    a._live.task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await a._live.task
    meta = run_json(a, rid)
    assert meta["verdict"] == "interrupted" and meta["interrupted_by"] == "console stop"
    assert a._live_run_id() is None


# ── preflight refusals (nothing is launched) ──────────────────────────────────────────────────
@pytest.mark.parametrize(("kwargs", "code"), [
    ({"key_file": False}, "ap.no_key"),
    ({"tokenizer": False}, "ap.no_tokenizer"),
])
async def test_preflight_refusals_carry_a_fix(make_adapter, kwargs: dict[str, Any], code: str) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter(**kwargs)
    res = await a.run_action("profile", dict(SMOKE))
    assert not res.ok and codes(res) == {code} and res.problems[0].fix
    assert not [c for c in run.calls if c[1:2] == ["profile"]] and a._live is None
    assert not a._runs_dir.exists() or not list(a._runs_dir.iterdir())


async def test_preflight_router_down_and_launcher_absent(make_adapter, router) -> None:  # type: ignore[no-untyped-def]
    router.get("http://127.0.0.1:4000/health/liveliness").mock(side_effect=httpx.ConnectError("refused"))
    a, _ = make_adapter()
    res = await a.run_action("profile", dict(SMOKE))
    assert not res.ok and codes(res) == {"ap.router_down"}
    b, _ = make_adapter(FakeRunner())
    res2 = await b.run_action("profile", dict(SMOKE))
    assert not res2.ok and codes(res2) == {"ap.not_installed"}


@pytest.mark.parametrize("preset", ["", "nope", "../etc", "SMOKE-LIGHTNING", "smoke-lightning; rm -rf /"])
async def test_unknown_presets_are_refused(make_adapter, preset: str) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("profile", {"preset": preset})
    assert not res.ok and "unknown or invalid preset" in res.output
    assert not [c for c in run.calls if c[1:2] == ["profile"]]


async def test_a_preset_with_a_validation_error_cannot_run(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    a.ctx.paths["aiperf_profiles"].write_text(fx("profiles.yaml").replace("concurrency: 2", "concurrency: 999"))
    res = await a.run_action("profile", dict(SMOKE))
    assert not res.ok and "unknown or invalid preset" in res.output and "'concurrency' must be an integer" in res.output
    assert not [c for c in run.calls if c[1:2] == ["profile"]]


async def test_unknown_action_is_a_key_error(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        await a.run_action("nope", {})


# ── report ─────────────────────────────────────────────────────────────────────────────────────
async def test_report_of_a_successful_run(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    rid, _ = await run_to_end(a)
    res = await a.run_action("report", {"run_id": rid})
    assert res.ok
    out = res.output
    assert "smoke-lightning" in out and "concurrency 2" in out and "8 requests (+1 warmup)" in out
    assert "| Time to first output (ms) | 30.0 | 40.7 |" in out and "| Inter-token latency (ms) | 0.79 | 0.97 |" in out
    assert "Output throughput: 1533.0 tokens/s" in out and "error rate: 0.0 %" in out and "router hop" in out
    assert (await a.run_action("report", {})).output == out                # empty run id = the newest


async def test_report_edge_cases(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    assert (await a.run_action("report", {})) .ok is False
    assert (await a.run_action("report", {"run_id": "../../etc/passwd"})).output == "unknown run id"
    assert (await a.run_action("report", {"run_id": "20261024T100000-001-nope"})).output == "unknown run id"


# ── housekeeping ───────────────────────────────────────────────────────────────────────────────
async def test_only_the_newest_hundred_runs_are_kept(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    for i in range(KEEP_RUNS + 5):
        d = a._runs_dir / f"20260101T0000{i // 100:02d}-{i % 100:03d}-old"
        d.mkdir(parents=True)
        (d / "run.json").write_text(json.dumps({"run_id": d.name, "verdict": "succeeded", "ts": 1.0 + i}))
    await run_to_end(a)
    names = sorted(p.name for p in a._runs_dir.iterdir())
    assert len(names) == KEEP_RUNS and names[-1].endswith("smoke-lightning")
    assert not (a._runs_dir / "20260101T000000-000-old").exists()


async def test_stray_entries_in_the_runs_dir_are_ignored(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    a._runs_dir.mkdir(parents=True)
    (a._runs_dir / "not-a-run").mkdir()
    (a._runs_dir / "20261024T100000-001-x").mkdir()          # a run dir without run.json
    (a._runs_dir / "20261024T100000-002-y").mkdir()
    (a._runs_dir / "20261024T100000-002-y" / "run.json").write_text("[1, 2]")
    assert await a.recent_events() == [] and (await a.metrics()).points[0].value == 0.0


# ── the core: confirm required, params validated, admin only ──────────────────────────────────
@pytest.fixture
def isolated(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Iterator[None]:
    for k in list(os.environ):
        if k.startswith("NEMO_CONSOLE_"):
            monkeypatch.delenv(k)
    monkeypatch.setenv(NO_USER_ENV, "1")
    (tmp_path / "home").mkdir(exist_ok=True)
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    yield


async def test_core_requires_confirm_and_validates_the_preset(isolated, tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()

    async def gated(argv: list[str]) -> Any:
        await gate.wait()
        return writes_profile()(argv)
    run.on([LAUNCHER, "profile"], gated)
    async with httpx.AsyncClient(trust_env=False) as http:
        a = AiPerfAdapter(make_ctx(tmp_path, run, http))
        cfg = Settings(state_dir=tmp_path / "state", frontend_dir=tmp_path / "front", background_tasks=False,
                       trust_loopback=True)
        container = build_container(cfg, adapters={"aiperf": a}, runner=run, http=http)  # type: ignore[arg-type]
        app = create_app(cfg, container)
        url = "/api/v1/systems/aiperf/actions/profile"
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=("127.0.0.1", 5000)),
                                     base_url="http://127.0.0.1:8793", headers={"X-Console": "1"}) as c:
            spec = next(s for s in (await c.get("/api/v1/systems/aiperf/actions")).json() if s["id"] == "profile")
            assert spec["danger"] == "disruptive" and spec["role"] == "admin"
            assert spec["params"][0]["choices"] == ["latency-lightning", "smoke-lightning", "throughput-lightning"]
            r = await c.post(url, json={"params": dict(SMOKE)})
            assert r.status_code == 400 and r.json()["error"]["code"] == "confirm_required"
            for bad in ({"preset": "nope"}, {"preset": 3}, {"unknown": 1}):
                r = await c.post(url, json={"params": {**SMOKE, **bad}, "confirm": True})
                assert r.status_code == 422 and r.json()["error"]["code"] == "invalid_param", bad
            assert not [x for x in run.calls if x[1:2] == ["profile"]]
            r = await c.post(url, json={"params": dict(SMOKE), "confirm": True})
            assert r.status_code == 200 and r.json()["ok"] is True
            r2 = await c.post("/api/v1/systems/aiperf/actions/report", json={"params": {}})
            assert r2.status_code == 200                         # safe: no confirm needed
            st = (await c.get("/api/v1/systems/aiperf/status")).json()
            assert st["system"] == "aiperf" and st["health"] == "up"
        gate.set()
        await finish(a)
        container.store.close()


async def test_core_config_roundtrip_uses_our_validator(isolated, tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    async with httpx.AsyncClient(trust_env=False) as http:
        a = AiPerfAdapter(make_ctx(tmp_path, run, http))
        cfg = Settings(state_dir=tmp_path / "state", frontend_dir=tmp_path / "front", background_tasks=False,
                       trust_loopback=True)
        container = build_container(cfg, adapters={"aiperf": a}, runner=run, http=http)  # type: ignore[arg-type]
        app = create_app(cfg, container)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=("127.0.0.1", 5000)),
                                     base_url="http://127.0.0.1:8793", headers={"X-Console": "1"}) as c:
            files = (await c.get("/api/v1/systems/aiperf/config")).json()
            assert [f["id"] for f in files] == ["profiles"] and files[0]["editable"] is True
            cf = (await c.get("/api/v1/systems/aiperf/config/profiles")).json()
            assert cf["content"] == fx("profiles.yaml")
            bad = cf["content"].replace("requests: 8", "requests: 0")
            v = (await c.post("/api/v1/systems/aiperf/config/profiles/validate", json={"content": bad})).json()
            assert v["ok"] is False and v["authoritative"] is True and "'requests' must be an integer" in v["diagnostics"][0]["message"]
            r = await c.post("/api/v1/systems/aiperf/config/profiles/apply",
                             json={"content": bad, "base_sha256": cf["sha256"], "restart": False, "confirm": True})
            assert r.status_code >= 400 or r.json().get("ok") is False                # an invalid file is never written
            assert a.ctx.paths["aiperf_profiles"].read_text() == fx("profiles.yaml")
            good = cf["content"].replace("requests: 8", "requests: 6")
            r = await c.post("/api/v1/systems/aiperf/config/profiles/apply",
                             json={"content": good, "base_sha256": cf["sha256"], "restart": False, "confirm": True})
            assert r.status_code == 200 and r.json()["ok"] is True and r.json()["post_check"] == "healthy", r.text
            assert a.ctx.paths["aiperf_profiles"].read_text() == good
            assert (await a.run_action("profile", dict(SMOKE))).ok                       # the next run uses the new file
            await finish(a)
            assert "--request-count" in run.calls[-1] and run.calls[-1][run.calls[-1].index("--request-count") + 1] == "6"
        container.store.close()



async def test_run_json_redacts_errors_even_if_a_caller_forgot(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    d = tmp_path / "rd"
    d.mkdir()
    a._write_meta(d, {"run_id": "x", "error": "failed with Bearer sk-abcdefghijklmnopqrstuv0123456789"})
    assert "sk-abcdefghijklmnop" not in (d / "run.json").read_text()
