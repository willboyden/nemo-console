"""Tier 3 (live agent evaluation): background start, one run at a time, completion from a CAPTURED result.json
(fixtures/tier3_result.json: SkillEvaluator's `calculator` sample run through the lab runner on 2026-10-17, trimmed to
the fields the console reads, container paths dropped), interrupted runs, the wrapper-only rule, and the core's
confirm + param checks."""
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
from skilleval_helpers import (
    T3_RUN,
    T3_SKILL,
    FakeRunner,
    fx,
    make_ctx,
    ok,
    standard_runner,
    tier3_stdout,
    writes_tier3,
)

from nemo_console.adapters import skillevaluator as se
from nemo_console.adapters.skillevaluator import SkillEvaluatorAdapter, last_json_object, summarize_tier3
from nemo_console.config import NO_USER_ENV, Settings
from nemo_console.container import build_container
from nemo_console.main import create_app

LIVE = {"skill": "robust-scripts", "attempts": 2}


def codes(res) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in res.problems}


def gated(responder, gate: asyncio.Event):  # type: ignore[no-untyped-def]
    """A tier3 responder that blocks until the test opens the gate (a live run in progress)."""
    async def respond(argv: list[str]):  # type: ignore[no-untyped-def]
        await gate.wait()
        return responder(argv)
    return respond


async def _finish(a: SkillEvaluatorAdapter) -> None:
    assert a._live is not None
    await asyncio.wait_for(a._live.task, 5)


def _run_json(a: SkillEvaluatorAdapter, run_id: str) -> dict[str, Any]:
    return json.loads((a._runs_dir / run_id / "run.json").read_text())


# ── start in the background ────────────────────────────────────────────────────────────────────
async def test_live_run_starts_in_the_background_and_returns_at_once(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()
    run.on(["skillevaluator-local", "tier3"], gated(writes_tier3(), gate))
    a, _ = make_adapter(run)
    res = await asyncio.wait_for(a.run_action("evaluate_live", dict(LIVE)), 2)    # does not wait for the run
    assert res.ok and "Started live evaluation" in res.output and "in the background" in res.output
    rid = a._live_run_id()
    assert rid is not None and rid.endswith("-robust-scripts") and rid in res.output
    run_dir = a._runs_dir / rid
    meta = _run_json(a, rid)
    assert meta["verdict"] == "running" and meta["tiers"] == "tier3" and meta["model_calls"] is True
    assert meta["live"] == {"agent": "opencode", "agent_model": "nemotron-lightning",
                            "judge_model": "nemotron-lightning-judge", "attempts": 2, "dataset": "auto",
                            "skip_baseline": False}
    assert stat.S_IMODE(os.stat(run_dir).st_mode) == 0o700
    assert stat.S_IMODE(os.stat(run_dir / "run.json").st_mode) == 0o600
    await asyncio.sleep(0.02)
    t3 = [c for c in run.calls if c[1:2] == ["tier3"]]
    skill = str((tmp_path / "repo/.claude/skills/robust-scripts").resolve())
    assert t3 == [["skillevaluator-local", "tier3", skill, "--n-attempts", "2", "--agent-model", "opencode=nemotron-lightning",
                   "--judge-model", "nemotron-lightning-judge", "--dataset", "auto", "--results-dir", str(run_dir),
                   "--quiet"]]
    assert run.cwds[-1] == run_dir
    # while it runs: status, events and the report say so
    st = await a.status()
    assert "live: robust-scripts running" in st.summary
    assert any(c.name == "tier3 robust-scripts" and c.state == "running" for c in st.components)
    ev = (await a.recent_events())[0]
    assert ev.data["verdict"] == "running" and "running" in ev.summary
    rep = await a.run_action("report", {"run_id": rid})
    assert rep.ok and "Status: running" in rep.output and "fills in when the run ends" in rep.output
    pts = {p.key: p.value for p in (await a.metrics()).points}
    assert pts["se.live_running"] == 1.0
    gate.set()
    await _finish(a)
    assert _run_json(a, rid)["verdict"] == "succeeded" and a._live_run_id() is None


async def test_one_live_run_at_a_time(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()
    run.on(["skillevaluator-local", "tier3"], gated(writes_tier3(), gate))
    a, _ = make_adapter(run)
    first = await a.run_action("evaluate_live", dict(LIVE))
    second = await a.run_action("evaluate_live", {"skill": "serve-model"})
    assert first.ok and not second.ok and codes(second) == {"skillevaluator.live.busy"}
    assert "already running" in second.output and str(a._live_run_id()) in second.output
    await asyncio.sleep(0.02)
    assert len([c for c in run.calls if c[1:2] == ["tier3"]]) == 1
    assert len(list(a._runs_dir.iterdir())) == 1                     # the refused request made no run directory
    gate.set()
    await _finish(a)
    third = await a.run_action("evaluate_live", {"skill": "serve-model"})   # free again once the first one ended
    assert third.ok
    await _finish(a)


async def test_static_runs_keep_working_while_a_live_run_is_in_progress(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()
    run.on(["skillevaluator-local", "tier3"], gated(writes_tier3(), gate))
    a, _ = make_adapter(run)
    await a.run_action("evaluate_live", dict(LIVE))
    res = await a.run_action("evaluate", {"skill": "serve-model", "tiers": "static"})
    assert res.ok and "serve-model · static" in res.output
    gate.set()
    await _finish(a)


# ── completion ─────────────────────────────────────────────────────────────────────────────────
async def test_completion_records_verdict_lift_attempts_and_feedback(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    res = await a.run_action("evaluate_live", {**LIVE, "agent_model": "my-agent", "judge_model": "my-judge",
                                               "dataset": "generate", "skip_baseline": True})
    assert res.ok
    rid = str(a._live_run_id())
    await _finish(a)
    meta = _run_json(a, rid)
    assert meta["verdict"] == "succeeded" and meta["container_exit"] == 0 and meta["job"] == "20261017-051633-16fc9065"
    assert meta["result"] == f"{T3_SKILL}/{T3_RUN}/result.json"
    t3 = meta["tier3"]
    assert t3["scored_attempts"] == 4 and t3["expected_attempts"] == 4 and t3["duration_seconds"] == 16.571
    agent = t3["agents"]["opencode"]
    assert agent["model"] == "openai/nemotron-lightning" and agent["skill_lift"] == 0.1597
    assert agent["with_skill"] == 0.7153 and agent["without_skill"] == 0.5555
    assert agent["metrics"]["skill_execution"] == {"with_skill": 0.625, "without_skill": 0.0, "delta": 0.625}
    assert set(agent["metrics"]) == {"security", "skill_execution", "skill_efficiency", "accuracy", "goal_accuracy",
                                     "behavior_check"}
    assert [c["title"] for c in meta["conclusions"]][:2] == ["Best performing agent", "Weakest dimension"]
    assert len(meta["recommendations"]) == 3 and all(len(r["message"]) <= 400 for r in meta["recommendations"])
    assert meta["live"]["dataset"] == "generate" and meta["live"]["skip_baseline"] is True
    # the event is metadata only: lift numbers yes, feedback text no
    ev = (await a.recent_events())[0]
    assert ev.data["tier3"]["agents"]["opencode"]["skill_lift"] == 0.1597
    assert "lift +0.160 (opencode)" in ev.summary and "succeeded (4/4 attempts scored)" in ev.summary
    assert "Best performing agent" not in json.dumps(ev.data)
    rep = await a.run_action("report", {"run_id": rid})
    assert rep.ok and "Status: succeeded" in rep.output and "Skill Lift" in rep.output
    assert "+0.160" in rep.output and "skill_execution" in rep.output and "Conclusions:" in rep.output
    assert "Recommendations:" in rep.output and "report.html" in rep.output
    assert "judge my-judge" in rep.output and "on my-agent" in rep.output
    pts = {p.key: p.value for p in (await a.metrics()).points}
    assert pts["se.last_skill_lift"] == 0.1597 and pts["se.runs_by_verdict{verdict=succeeded}"] == 1.0
    assert pts["se.live_running"] == 0.0


async def test_failed_execution_status_is_the_verdict(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    d = json.loads(fx("tier3_result.json"))
    d["execution_status"] = "failed"
    run = standard_runner()

    def respond(argv: list[str]):  # type: ignore[no-untyped-def]
        out = Path(argv[argv.index("--results-dir") + 1]) / T3_SKILL / T3_RUN
        out.mkdir(parents=True)
        (out / "result.json").write_text(json.dumps(d))
        return ok(tier3_stdout("failed"), rc=1)
    run.on(["skillevaluator-local", "tier3"], respond)
    a, _ = make_adapter(run)
    await a.run_action("evaluate_live", dict(LIVE))
    rid = str(a._live_run_id())
    await _finish(a)
    assert _run_json(a, rid)["verdict"] == "failed"


async def test_no_result_is_an_error_run_with_the_launcher_reason_redacted(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on(["skillevaluator-local", "tier3"], ok("", rc=1, stderr="launch: NOT READY: broker down; Bearer sk-abcdefghijklmnop"))
    a, _ = make_adapter(run)
    await a.run_action("evaluate_live", dict(LIVE))
    rid = str(a._live_run_id())
    await _finish(a)
    meta = _run_json(a, rid)
    assert meta["verdict"] == "error" and "NOT READY" in meta["error"]
    assert "sk-abcdefghijklmnop" not in (a._runs_dir / rid / "run.json").read_text()
    rep = await a.run_action("report", {"run_id": rid})
    assert "No Tier 3 result was written" in rep.output and "sk-abcdefghijklmnop" not in rep.output
    # the launcher's own tier3 error (from its summary) wins over stderr noise
    run.on(["skillevaluator-local", "tier3"], ok(tier3_stdout(error="dataset=existing but the skill has no evals/evals.json"),
                                                  rc=1, stderr="launch: job x"))
    await a.run_action("evaluate_live", {**LIVE, "dataset": "existing"})
    rid2 = str(a._live_run_id())
    await _finish(a)
    assert _run_json(a, rid2)["error"] == "dataset=existing but the skill has no evals/evals.json"


async def test_a_symlinked_result_is_never_read(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    secret = tmp_path / "private.json"
    secret.write_text(fx("tier3_result.json"))
    run = standard_runner()

    def respond(argv: list[str]):  # type: ignore[no-untyped-def]
        out = Path(argv[argv.index("--results-dir") + 1]) / T3_SKILL / T3_RUN
        out.mkdir(parents=True)
        (out / "result.json").symlink_to(secret)                    # planted by a hostile skill's run
        return ok(tier3_stdout(), rc=0)
    run.on(["skillevaluator-local", "tier3"], respond)
    a, _ = make_adapter(run)
    await a.run_action("evaluate_live", dict(LIVE))
    rid = str(a._live_run_id())
    await _finish(a)
    assert _run_json(a, rid)["verdict"] == "error"


# ── interrupted ────────────────────────────────────────────────────────────────────────────────
async def test_a_running_run_left_by_a_previous_process_reads_as_interrupted(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()
    run.on(["skillevaluator-local", "tier3"], gated(writes_tier3(), gate))
    old, _ = make_adapter(run)
    await old.run_action("evaluate_live", dict(LIVE))
    rid = str(old._live_run_id())
    # a new console process over the same state dir: run.json still says running, but no task owns it
    new, _ = make_adapter(standard_runner())
    assert _run_json(new, rid)["verdict"] == "running"
    ev = (await new.recent_events())[0]
    assert ev.data["verdict"] == "interrupted" and "interrupted" in ev.summary
    rep = await new.run_action("report", {"run_id": rid})
    assert "Interrupted: the console restarted" in rep.output
    assert (await new.run_action("evaluate_live", dict(LIVE))).ok              # not "busy": that run is not ours
    await _finish(new)
    pts = {p.key: p.value for p in (await new.metrics()).points}
    assert pts["se.runs_by_verdict{verdict=interrupted}"] == 1.0
    # the old process's launcher finished on its own after all: the report shows the result that landed
    gate.set()
    await _finish(old)
    old._live = None
    late, _ = make_adapter(standard_runner())
    (late._runs_dir / rid / "run.json").write_text(json.dumps({**_run_json(late, rid), "verdict": "running"}))
    rep = await late.run_action("report", {"run_id": rid})
    assert "Interrupted" in rep.output and "result.json landed after that" in rep.output and "+0.160" in rep.output


async def test_console_stop_cancels_the_run_and_marks_it_interrupted(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()
    run.on(["skillevaluator-local", "tier3"], gated(writes_tier3(), gate))
    a, _ = make_adapter(run)
    await a.run_action("evaluate_live", dict(LIVE))
    rid = str(a._live_run_id())
    assert a._live is not None
    await asyncio.sleep(0.02)                                          # the launcher is running (blocked on the gate)
    a._live.task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await a._live.task
    meta = _run_json(a, rid)
    assert meta["verdict"] == "interrupted" and meta["interrupted_by"] == "console stop"
    rep = await a.run_action("report", {"run_id": rid})
    assert "the console stopped while it ran" in rep.output


# ── refusals and validation ────────────────────────────────────────────────────────────────────
async def test_plain_skillevaluator_is_refused_for_tier3(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter(standard_runner("skillevaluator"))
    res = await a.run_action("evaluate_live", dict(LIVE))
    assert not res.ok and codes(res) == {"skillevaluator.live.needs_wrapper"}
    [p] = res.problems
    assert p.fix and p.fix.startswith("install the lab wrapper: clients/skillevaluator/install.sh")
    assert "sandboxed runner" in res.output
    assert not [c for c in run.calls if "tier3" in c] and not a._runs_dir.exists()
    # with the lab repo configured, the fix is that repo's installer, by path
    repo = tmp_path / "lab"
    (repo / "clients" / "skillevaluator").mkdir(parents=True)
    (repo / "clients" / "skillevaluator" / "install.sh").write_text("#!/bin/sh\n")
    b, _ = make_adapter(standard_runner("skillevaluator"), repo=repo)
    [q] = (await b.run_action("evaluate_live", dict(LIVE))).problems
    assert q.fix == f"install the lab wrapper: {repo}/clients/skillevaluator/install.sh   # links skillevaluator-local into ~/.local/bin"


async def test_not_installed_and_bad_values_are_refused_without_running(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(FakeRunner())
    res = await a.run_action("evaluate_live", dict(LIVE))
    assert not res.ok and codes(res) == {"skillevaluator.not_installed"}
    b, run2 = make_adapter()
    for params, why in (({"skill": "linked"}, "unknown skill"), ({**LIVE, "attempts": 0}, "attempts"),
                        ({**LIVE, "attempts": True}, "attempts"), ({**LIVE, "agent_model": "a b"}, "agent_model"),
                        ({**LIVE, "judge_model": "--version"}, "judge_model"), ({**LIVE, "dataset": "all"}, "dataset"),
                        ({**LIVE, "skip_baseline": "yes"}, "skip_baseline")):
        r = await b.run_action("evaluate_live", params)
        assert not r.ok and why in r.output, params
    assert not [c for c in run2.calls if "tier3" in c] and b._live is None


def test_summarize_tier3_never_invents_and_caps() -> None:
    s = summarize_tier3({"execution_status": "succeeded"})
    assert s["agents"] == {} and s["scored_attempts"] is None and s["conclusions"] == []
    hostile = {"execution_status": "<script>", "run_id": "../../x",
               "agents": {"bad name;": {}, "opencode": {"model": "m; rm -rf /", "lift": {"overall": {"delta": "1"},
                                                                                         "Bad-Key": {"delta": 1}}}},
               "tier3_feedback": {"conclusions": [{"severity": "pass", "title": "t", "message": "x" * 5000}] * 20,
                                  "recommendations": ["plain string", 7]}}
    h = summarize_tier3(hostile)
    assert h["execution_status"] is None and h["run_id"] is None and list(h["agents"]) == ["opencode"]
    assert h["agents"]["opencode"]["model"] is None and h["agents"]["opencode"]["skill_lift"] is None
    assert h["agents"]["opencode"]["metrics"] == {}
    assert len(h["conclusions"]) == se.MAX_FEEDBACK and len(h["conclusions"][0]["message"]) == 400
    assert h["recommendations"] == [{"severity": "info", "title": "", "message": "plain string"}]
    assert se.tier3_verdict(h) == "error"
    assert last_json_object('progress\nline\n{\n  "job": "x",\n  "summary": {\n    "a": 1\n  }\n}') == {"job": "x",
                                                                                                       "summary": {"a": 1}}
    assert last_json_object("") is None and last_json_object("no json here") is None
    assert last_json_object(tier3_stdout())["container_exit"] == 0  # type: ignore[index]


# ── through the core: confirm required, params validated against the ActionSpec ────────────────
@pytest.fixture
def isolated(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Iterator[None]:
    for k in list(os.environ):
        if k.startswith("NEMO_CONSOLE_"):
            monkeypatch.delenv(k)
    monkeypatch.setenv(NO_USER_ENV, "1")
    (tmp_path / "home").mkdir()
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    yield


async def test_core_requires_confirm_and_validates_live_params(isolated, tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    async with httpx.AsyncClient(trust_env=False) as http:
        a = SkillEvaluatorAdapter(make_ctx(tmp_path, run, http))
        cfg = Settings(state_dir=tmp_path / "state", frontend_dir=tmp_path / "front", background_tasks=False,
                       trust_loopback=True)
        container = build_container(cfg, adapters={"skillevaluator": a}, runner=run, http=http)  # type: ignore[arg-type]
        app = create_app(cfg, container)
        url = "/api/v1/systems/skillevaluator/actions/evaluate_live"
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=("127.0.0.1", 5000)),
                                     base_url="http://127.0.0.1:8793", headers={"X-Console": "1"}) as c:
            spec = next(s for s in (await c.get("/api/v1/systems/skillevaluator/actions")).json() if s["id"] == "evaluate_live")
            assert spec["danger"] == "disruptive" and spec["role"] == "admin"
            assert {p["key"]: p["default"] for p in spec["params"]} == {
                "skill": "robust-scripts", "attempts": 1, "agent_model": "nemotron-lightning",
                "judge_model": "nemotron-lightning-judge", "dataset": "auto", "skip_baseline": False}
            r = await c.post(url, json={"params": dict(LIVE)})
            assert r.status_code == 400 and r.json()["error"]["code"] == "confirm_required"
            for bad in ({"attempts": 6}, {"attempts": "2"}, {"dataset": "all"}, {"skip_baseline": "true"},
                        {"skill": "linked"}, {"agent_model": "x\ny"}, {"n_concurrent": 4}):
                r = await c.post(url, json={"params": {**LIVE, **bad}, "confirm": True})
                assert r.status_code == 422 and r.json()["error"]["code"] == "invalid_param", bad
            assert not [x for x in run.calls if "tier3" in x]
            r = await c.post(url, json={"params": dict(LIVE), "confirm": True})
            assert r.status_code == 200 and r.json()["ok"] is True
        await _finish(a)
        container.store.close()
