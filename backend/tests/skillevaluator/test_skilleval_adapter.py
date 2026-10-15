from __future__ import annotations

import asyncio
import json
import os
import stat
from pathlib import Path

import httpx
import pytest
from skilleval_helpers import FakeRunner, ok, standard_runner

from nemo_console.adapters import skillevaluator as se
from nemo_console.adapters.skillevaluator import SkillEvaluatorAdapter, failed_checks, summarize_tier1
from nemo_console.services.runner import SubprocessRunner


def codes(st) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in st.problems}


# ── discovery and containment ──────────────────────────────────────────────────────────────────
async def test_skills_are_real_dirs_with_skill_md_only(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    assert a.skills() == ["robust-scripts", "serve-model"]        # no symlink, no dir without SKILL.md, no file


@pytest.mark.parametrize("name", ["../outside", "linked", "no-skill-md", "", "a/b", ".hidden", "x" * 200])
async def test_skill_path_refuses_anything_outside_the_list(make_adapter, name: str) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        a._skill_path(name)


async def test_evaluate_refuses_an_unknown_skill_without_running(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("evaluate", {"skill": "linked", "tiers": "static"})
    assert not res.ok and "unknown skill" in res.output
    assert not [c for c in run.calls if "tier1" in c]


# ── status ─────────────────────────────────────────────────────────────────────────────────────
async def test_status_prefers_the_wrapper_and_is_up(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    st = await a.status()
    assert st.system == "skillevaluator" and st.health == "up" and st.version == "0.5.0"
    assert st.components[0].name == "skillevaluator-local" and "own key file" in (st.components[0].detail or "")
    assert "2 skills" in st.summary and not st.problems
    assert run.calls[0] == ["skillevaluator-local", "--version"]


async def test_version_is_cached_between_polls(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    await a.status()
    await a.status()
    assert sum(1 for c in run.calls if c[-1] == "--version") == 1


async def test_plain_cli_without_wrapper_is_flagged_info(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(standard_runner("skillevaluator"))
    st = await a.status()
    assert st.health == "up" and "skillevaluator.no_wrapper" in codes(st)


async def test_not_installed_is_down_with_install_fix(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(FakeRunner())
    st = await a.status()
    assert st.health == "down" and "skillevaluator.not_installed" in codes(st)
    p = next(p for p in st.problems if p.code == "skillevaluator.not_installed")
    assert p.fix and "uv tool install" in p.fix


async def test_router_down_is_degraded(make_adapter, router) -> None:  # type: ignore[no-untyped-def]
    router.get("http://127.0.0.1:4000/health/liveliness").mock(side_effect=httpx.ConnectError("refused"))
    a, _ = make_adapter()
    st = await a.status()
    assert st.health == "degraded" and "skillevaluator.router_down" in codes(st)


async def test_no_skills_dir_is_a_warning_not_a_crash(tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    from skilleval_helpers import make_ctx
    async with httpx.AsyncClient(trust_env=False) as client:
        a = SkillEvaluatorAdapter(make_ctx(tmp_path, standard_runner(), client, skills=False))
        st = await a.status()
        assert "skillevaluator.no_skills" in codes(st) and a.skills() == []
        assert a.actions()[0].params[0].choices is None


# ── evaluate ───────────────────────────────────────────────────────────────────────────────────
async def test_static_tier_passes_no_llm_and_keeps_the_report(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("evaluate", {"skill": "robust-scripts", "tiers": "static"})
    t1 = next(c for c in run.calls if c[1] == "tier1")
    assert t1[:3] == ["skillevaluator-local", "tier1", str((tmp_path / "repo/.claude/skills/robust-scripts").resolve())]
    assert "--no-llm" in t1 and t1[t1.index("-r") + 1] == "json,markdown"
    run_dir = Path(t1[t1.index("-o") + 1])
    assert run_dir.parent == tmp_path / "state" / "skillevaluator" / "runs"
    assert run.cwds[-1] == run_dir                                   # the CLI's scratch files land in the run dir
    meta = json.loads((run_dir / "run.json").read_text())
    # exit code 1 from the CLI is NOT the verdict: the JSON overall_status is
    assert meta["verdict"] == "incomplete" and meta["model_calls"] is False
    assert meta["tier1"]["quality_score"] == 83.0 and meta["tier1"]["grade"] == "B"
    assert meta["tier1"]["incomplete"] == ["skillspector"]
    assert stat.S_IMODE(os.stat(run_dir / "run.json").st_mode) == 0o600
    assert stat.S_IMODE(os.stat(run_dir).st_mode) == 0o700
    assert res.ok and "robust-scripts · static · quality 83.0 B" in res.output and "INCOMPLETE" in res.output


async def test_tier1_runs_the_llm_judge(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("evaluate", {"skill": "robust-scripts", "tiers": "tier1"})
    t1 = next(c for c in run.calls if c[1] == "tier1")
    assert "--no-llm" not in t1 and not [c for c in run.calls if c[1] == "tier2"]
    assert "judge 83.6" in res.output


async def test_tier1_plus_2_runs_dedup_and_records_it(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("evaluate", {"skill": "serve-model", "tiers": "tier1+2"})
    t2 = next(c for c in run.calls if c[1] == "tier2")
    assert t2[2].endswith("/serve-model") and "-o" in t2
    ev = (await a.recent_events())[0]
    assert ev.data["tier2_status"] == "passed" and ev.data["verdict"] == "incomplete"
    assert "Tier 2 (deduplication): passed" in res.output


async def test_tier2_failure_downgrades_a_passing_verdict(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()

    def t1_pass(argv: list[str]):  # type: ignore[no-untyped-def]
        d = json.loads((Path(__file__).parent / "fixtures" / "tier1_incomplete.json").read_text())
        d.update(overall_status="passed", incomplete_scans=[])
        Path(argv[argv.index("-o") + 1], "skillevaluator-tier1.json").write_text(json.dumps(d))
        return ok()
    run.on(["skillevaluator-local", "tier1"], t1_pass)
    run.on(["skillevaluator-local", "tier2"], ok("", rc=1, stderr="Error: SKILL_EVAL_EMBEDDING_PROVIDER is required"))
    a, _ = make_adapter(run)
    await a.run_action("evaluate", {"skill": "serve-model", "tiers": "tier1+2"})
    ev = (await a.recent_events())[0]
    assert ev.data["tier2_status"] == "error" and ev.data["verdict"] == "error"


async def test_no_report_is_an_error_run_with_redacted_stderr(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on(["skillevaluator-local", "tier1"], ok("", rc=2, stderr="boom: Authorization: Bearer sk-abcdefghijklmnop"))
    a, _ = make_adapter(run)
    res = await a.run_action("evaluate", {"skill": "robust-scripts", "tiers": "tier1"})
    assert not res.ok and "did not produce a report" in res.output
    assert "sk-abcdefghijklmnop" not in res.output
    run_json = next((a._runs_dir).iterdir()) / "run.json"
    assert "sk-abcdefghijklmnop" not in run_json.read_text()


async def test_evaluate_all_static_runs_every_skill_without_llm(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("evaluate_all_static", {})
    t1 = [c for c in run.calls if c[1] == "tier1"]
    assert len(t1) == 2 and all("--no-llm" in c for c in t1)
    assert "robust-scripts" in res.output and "serve-model" in res.output and "83.0" in res.output


async def test_unknown_action_raises_keyerror(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        await a.run_action("tier3", {})


async def test_doctor_runs_the_launcher(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("doctor", {})
    assert res.ok and "openai-compatible" in res.output and ["skillevaluator-local", "doctor"] in run.calls


# ── stored runs: report, events, metrics, retention ────────────────────────────────────────────
async def test_report_action_newest_and_by_id(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    empty = await a.run_action("report", {})
    assert not empty.ok and "no stored runs" in empty.output
    await a.run_action("evaluate", {"skill": "robust-scripts", "tiers": "static"})
    newest = await a.run_action("report", {"run_id": ""})
    rid = (await a.recent_events())[0].data["run_id"]
    by_id = await a.run_action("report", {"run_id": rid})
    assert newest.ok and by_id.output == newest.output
    bad = await a.run_action("report", {"run_id": "../../etc/passwd"})
    assert not bad.ok and bad.output == "unknown run id"


async def test_events_and_metrics_follow_the_stored_runs(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    m0 = await a.metrics()
    assert {p.key: p.value for p in m0.points} == {"se.skills": 2.0, "se.runs": 0.0}
    await a.run_action("evaluate", {"skill": "robust-scripts", "tiers": "tier1"})
    evs = await a.recent_events()
    assert len(evs) == 1 and evs[0].kind == "skilleval.run" and evs[0].system == "skillevaluator"
    pts = {p.key: p.value for p in (await a.metrics()).points}
    assert pts["se.runs"] == 1.0 and pts["se.last_quality_score"] == 83.0 and pts["se.last_judge_score"] == 83.6
    assert pts["se.runs_by_verdict{verdict=incomplete}"] == 1.0
    st = await a.status()
    assert "last run: robust-scripts · tier1" in st.summary


async def test_follow_events_yields_only_new_runs(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    await a.run_action("evaluate", {"skill": "robust-scripts", "tiers": "static"})
    gen = a.follow_events()
    nxt = asyncio.ensure_future(gen.__anext__())
    await asyncio.sleep(0.05)
    assert not nxt.done()                                            # the old run is not replayed
    await a.run_action("evaluate", {"skill": "serve-model", "tiers": "static"})
    ev = await asyncio.wait_for(nxt, 2)
    assert ev.data["skill"] == "serve-model"
    await gen.aclose()


async def test_retention_keeps_the_newest_runs(make_adapter, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    monkeypatch.setattr(se, "KEEP_RUNS", 2)
    a, _ = make_adapter()
    for _ in range(3):
        await a.run_action("evaluate", {"skill": "robust-scripts", "tiers": "static"})
        await asyncio.sleep(0.002)
    assert len(list(a._runs_dir.iterdir())) == 2


# ── report parsing ─────────────────────────────────────────────────────────────────────────────
def test_summarize_never_invents_missing_fields() -> None:
    s = summarize_tier1({"overall_status": "passed"})
    assert s["status"] == "passed" and s["quality_score"] is None and s["rubric_score"] is None and s["incomplete"] == []
    assert summarize_tier1({"rubric_eval": {"execution_status": "failed", "overall_score": 50}})["rubric_score"] is None
    assert failed_checks({"results": [{"validator": "X", "findings": [{"severity": "low", "message": "m"}]}]}) == []
    assert failed_checks({"results": "garbage"}) == []


# ── runner: SKILL_EVAL_* reaches SkillEvaluator only ───────────────────────────────────────────
def test_runner_passes_skill_eval_env_only_to_skillevaluator(monkeypatch) -> None:  # type: ignore[no-untyped-def]
    monkeypatch.setenv("SKILL_EVAL_LLM_API_KEY", "secret-value")
    monkeypatch.setenv("SKILLEVAL_MODEL", "m")
    monkeypatch.setenv("OPENAI_API_KEY", "other-secret")
    r = SubprocessRunner()
    for b in ("skillevaluator", "skillevaluator-local"):
        env = r._env(b)
        assert env["SKILL_EVAL_LLM_API_KEY"] == "secret-value" and env["SKILLEVAL_MODEL"] == "m"
        assert "OPENAI_API_KEY" not in env
    for b in ("docker", "systemctl", "nemo-relay", ""):
        env = r._env(b)
        assert "SKILL_EVAL_LLM_API_KEY" not in env and "SKILLEVAL_MODEL" not in env
    # the allowlist still gates by bare name, never a path
    with pytest.raises(Exception, match="not allowed"):
        r.resolve("/usr/bin/skillevaluator")
