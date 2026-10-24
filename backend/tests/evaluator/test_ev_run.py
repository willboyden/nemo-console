"""The `run` and `report` actions end to end with golden argv (offline fakes)."""
from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

from ev_helpers import LAUNCHER, ok, standard_runner, writes_summary

from nemo_console.adapters.base import CommandResult


async def finish(a: Any) -> None:
    assert a._live is not None
    await asyncio.wait_for(a._live.task, 5)


def only_run_dir(a: Any) -> Path:
    return max(a._runs_dir.iterdir(), key=lambda p: p.name)       # the newest


async def test_action_specs(make_adapter: Any) -> None:
    a, _ = make_adapter()
    acts = {x.id: x for x in a.actions()}
    assert set(acts) == {"run", "report"}
    assert acts["run"].danger == "disruptive" and acts["report"].danger == "safe"
    p = acts["run"].params[0]
    assert p.type == "enum" and p.choices == ["smoke-lightning", "knowledge-lightning", "baseline-lightning"]
    assert p.default == "smoke-lightning"


async def test_run_end_to_end_golden_argv(make_adapter: Any) -> None:
    a, runner = make_adapter()
    res = await a.run_action("run", {"suite": "smoke-lightning"})
    assert res.ok and "Started evaluation" in res.output
    run_dir = only_run_dir(a)
    assert json.loads((run_dir / "run.json").read_text())["verdict"] == "running"
    await finish(a)
    run_calls = [c for c in runner.calls if c[:2] == [LAUNCHER, "run"]]
    assert run_calls == [[LAUNCHER, "run", "--suite", "smoke-lightning", "--output-dir", str(run_dir / "out")]]
    assert all("sk-" not in " ".join(c) and "key" not in " ".join(c).lower() for c in runner.calls)
    assert runner.timeouts[runner.calls.index(run_calls[0])] == 4800.0
    meta = json.loads((run_dir / "run.json").read_text())
    assert meta["verdict"] == "succeeded" and meta["model"] == "aitop/nemotron-lightning"
    assert meta["results"][0]["bench"] == "gsm8k" and meta["results"][0]["score"] == 1.0
    assert meta["results"][0]["samples"] == 10 and "SECRET" not in json.dumps(meta)
    assert oct((run_dir / "run.json").stat().st_mode & 0o777) == "0o600"
    m = await a.metrics()
    sc = next(p for p in m.points if p.key == "ev.score")
    assert sc.labels == {"bench": "gsm8k", "model": "aitop/nemotron-lightning"} and sc.value == 1.0
    [ev] = await a.recent_events()
    assert ev.data["verdict"] == "succeeded" and "SECRET" not in ev.model_dump_json()


async def test_second_run_is_refused_while_one_is_live(make_adapter: Any) -> None:
    gate = asyncio.Event()

    async def slow(argv: list[str]) -> CommandResult:
        await gate.wait()
        return writes_summary()(argv)

    a, _ = make_adapter(standard_runner().on([LAUNCHER, "run"], slow))
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok
    second = await a.run_action("run", {"suite": "knowledge-lightning"})
    assert not second.ok and second.problems[0].code == "ev.run.busy"
    st = await a.status()
    assert any(c.name == "run smoke-lightning" for c in st.components)
    gate.set()
    await finish(a)
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok       # free again
    await finish(a)


async def test_run_refuses_unprefetched_datasets_with_the_prefetch_command(make_adapter: Any) -> None:
    cached = {"cache_dir": "/x", "benchmarks": [{"bench": "gsm8k", "cached": False}, {"bench": "mmlu", "cached": True}]}
    a, runner = make_adapter(standard_runner().on([LAUNCHER, "cached"], ok(json.dumps(cached))))
    res = await a.run_action("run", {"suite": "baseline-lightning"})
    assert not res.ok and res.problems[0].code == "ev.dataset_missing"
    assert res.problems[0].fix == "evaluator-local prefetch gsm8k"
    assert not any(c[:2] == [LAUNCHER, "run"] for c in runner.calls) and not a._runs_dir.exists()


async def test_run_refuses_unknown_suite_missing_key_and_missing_tool(make_adapter: Any) -> None:
    a, _ = make_adapter()
    assert "unknown suite" in (await a.run_action("run", {"suite": "nope"})).output
    assert "unknown suite" in (await a.run_action("run", {})).output
    b, _ = make_adapter(key_file=False)
    r = await b.run_action("run", {"suite": "smoke-lightning"})
    assert not r.ok and r.problems[0].code == "ev.no_key" and r.problems[0].fix is not None
    from ev_helpers import FakeRunner
    c, _ = make_adapter(FakeRunner())
    r = await c.run_action("run", {"suite": "smoke-lightning"})
    assert not r.ok and r.problems[0].code == "ev.not_installed"


async def test_failed_run_keeps_the_wrappers_error(make_adapter: Any) -> None:
    r = standard_runner().on([LAUNCHER, "run"], writes_summary("summary_failed.json", rc=1))
    a, _ = make_adapter(r)
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok
    await finish(a)
    meta = json.loads((only_run_dir(a) / "run.json").read_text())
    assert meta["verdict"] == "failed" and "OfflineModeIsEnabled" in meta["error"]
    st = await a.status()
    assert "ev.last_run_failed" in [p.code for p in st.problems]
    m = await a.metrics()
    assert not [p for p in m.points if p.key == "ev.score"]                   # a failed bench has no score


async def test_wrapper_exit_zero_without_summary_is_error_not_success(make_adapter: Any) -> None:
    a, _ = make_adapter(standard_runner().on([LAUNCHER, "run"], ok("done")))
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok
    await finish(a)
    assert json.loads((only_run_dir(a) / "run.json").read_text())["verdict"] == "error"


async def test_timeout_and_runner_exception_are_data(make_adapter: Any) -> None:
    a, _ = make_adapter(standard_runner().on([LAUNCHER, "run"], CommandResult(-9, "", "", timed_out=True)))
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok
    await finish(a)
    meta = json.loads((only_run_dir(a) / "run.json").read_text())
    assert meta["verdict"] == "failed" and meta["error"] == "timed out"

    def boom(argv: list[str]) -> CommandResult:
        raise RuntimeError("spawn failed")

    b, _ = make_adapter(standard_runner().on([LAUNCHER, "run"], boom))
    assert (await b.run_action("run", {"suite": "smoke-lightning"})).ok
    await finish(b)
    assert json.loads((only_run_dir(b) / "run.json").read_text())["verdict"] == "failed"


async def test_cancel_marks_the_run_interrupted(make_adapter: Any) -> None:
    started = asyncio.Event()

    async def hang(argv: list[str]) -> CommandResult:
        started.set()
        await asyncio.sleep(60)
        return ok()

    a, _ = make_adapter(standard_runner().on([LAUNCHER, "run"], hang))
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok
    await started.wait()
    assert a._live is not None
    a._live.task.cancel()
    await asyncio.gather(a._live.task, return_exceptions=True)
    meta = json.loads((only_run_dir(a) / "run.json").read_text())
    assert meta["verdict"] == "interrupted" and meta["interrupted_by"] == "console stop"


async def test_interrupted_summary_from_the_wrapper(make_adapter: Any, tmp_path: Path) -> None:
    def interrupted(argv: list[str]) -> CommandResult:
        res = writes_summary()(argv)
        out = Path(argv[argv.index("--output-dir") + 1]) / "summary.json"
        d = json.loads(out.read_text())
        d["state"] = "interrupted"
        out.write_text(json.dumps(d))
        return CommandResult(130, res.stdout, "")

    a, _ = make_adapter(standard_runner().on([LAUNCHER, "run"], interrupted))
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok
    await finish(a)
    assert json.loads((only_run_dir(a) / "run.json").read_text())["verdict"] == "interrupted"


async def test_report_action(make_adapter: Any) -> None:
    a, _ = make_adapter()
    assert not (await a.run_action("report", {})).ok                           # no runs yet
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok
    await finish(a)
    rid = only_run_dir(a).name
    for params in ({}, {"run_id": rid}):
        r = await a.run_action("report", params)
        assert r.ok and "verdict: succeeded" in r.output and "| gsm8k | ok | pass@1 | 1.0000 |" in r.output
        assert "SECRET" not in r.output and "sk-" not in r.output
    bad = await a.run_action("report", {"run_id": "../../etc"})
    assert not bad.ok and "run id" in bad.output
    assert not (await a.run_action("report", {"run_id": "20200101T000000-000-ghost"})).ok


async def test_unknown_action_is_keyerror(make_adapter: Any) -> None:
    import pytest
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        await a.run_action("prefetch", {})            # prefetch (internet) is deliberately not an action


async def test_prune_keeps_the_newest_hundred(make_adapter: Any) -> None:
    a, _ = make_adapter()
    for i in range(105):
        d = a._runs_dir / f"2026010{i // 100}T0{(i % 100) // 10}{i % 10}000-001-smoke-lightning"
        d.mkdir(parents=True, mode=0o700)
        (d / "run.json").write_text(json.dumps({"verdict": "succeeded", "run_id": d.name}))
    assert (await a.run_action("run", {"suite": "smoke-lightning"})).ok
    await finish(a)
    assert len(list(a._runs_dir.iterdir())) == 100
