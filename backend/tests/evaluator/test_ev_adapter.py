"""Observe + configure: status, metrics, events, suites.yaml validation (offline; fakes answer like the real wrapper)."""
from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

import pytest
from ev_helpers import LAUNCHER, FakeRunner, fx, ok, standard_runner

from nemo_console.adapters.evaluator import EvaluatorAdapter, validate_suites
from nemo_console.domain.models import SystemEvent

RUN = "20261024T072853-123-smoke-lightning"


def codes(st: Any) -> list[str]:
    return [p.code for p in st.problems]


def put_run(adapter: EvaluatorAdapter, run_id: str, **meta: Any) -> Path:
    d = adapter._runs_dir / run_id
    d.mkdir(parents=True, mode=0o700, exist_ok=True)
    base: dict[str, Any] = {"run_id": run_id, "suite": run_id.split("-", 2)[2], "model": "aitop/nemotron-lightning",
                            "ts": 1792841333.0, "verdict": "succeeded", "mode": "run"}
    (d / "run.json").write_text(json.dumps({**base, **meta}))
    return d


async def test_status_up(make_adapter: Any) -> None:
    a, _ = make_adapter()
    st = await a.status()
    assert st.system == "evaluator" and st.health == "up" and st.version == "0.3.0"
    assert st.problems == []
    assert "3 suites" in st.summary and "6/6 datasets cached" in st.summary
    assert st.components[0].name == LAUNCHER
    assert st.endpoints[0].auth == "bearer" and "sk-" not in st.model_dump_json()


async def test_status_not_installed_is_down_with_fix(make_adapter: Any, tmp_path: Path) -> None:
    repo = tmp_path / "labrepo"
    (repo / "clients" / "nemo-evaluator").mkdir(parents=True)
    (repo / "clients" / "nemo-evaluator" / "install.sh").write_text("#!/bin/sh\n")
    a, _ = make_adapter(FakeRunner(), repo=repo)
    st = await a.status()
    assert st.health == "down" and st.version is None
    p = next(p for p in st.problems if p.code == "ev.not_installed")
    assert p.severity == "error" and p.fix is not None and "clients/nemo-evaluator/install.sh" in p.fix
    assert "ev.dataset_missing" not in codes(st)           # nothing to say about datasets without the tool


async def test_status_no_key_warns_with_mint_fix(make_adapter: Any) -> None:
    a, _ = make_adapter(key_file=False)
    st = await a.status()
    p = next(p for p in st.problems if p.code == "ev.no_key")
    assert p.severity == "warn" and p.fix is not None and "mint-key.sh" in p.fix and st.health == "up"


async def test_status_router_down_degraded(make_adapter: Any, router: Any) -> None:
    router.get("http://127.0.0.1:4000/health/liveliness").respond(503)
    a, _ = make_adapter()
    st = await a.status()
    assert st.health == "degraded" and "ev.router_down" in codes(st)


async def test_status_missing_datasets_suggest_prefetch(make_adapter: Any) -> None:
    r = standard_runner().on([LAUNCHER, "cached"], ok(fx("cached_partial.json").replace('"gsm8k", "cached": true',
                                                                                        '"gsm8k", "cached": false')))
    a, _ = make_adapter(r)
    st = await a.status()
    p = next(p for p in st.problems if p.code == "ev.dataset_missing")
    assert p.fix is not None and p.fix.startswith("evaluator-local prefetch gsm8k")
    assert "mmlu" not in p.fix and "smoke-lightning" in p.message and p.severity == "warn"


async def test_status_cached_unreadable_is_not_a_false_alarm(make_adapter: Any) -> None:
    a, _ = make_adapter(standard_runner().on([LAUNCHER, "cached"], ok("not json")))
    st = await a.status()
    assert "ev.dataset_missing" not in codes(st) and st.health == "up"


async def test_status_invalid_suites_is_degraded(make_adapter: Any) -> None:
    a, _ = make_adapter(suites="version: 1\nsuites:\n  x:\n    model: m\n    benchmarks:\n      - bench: humaneval\n")
    st = await a.status()
    p = next(p for p in st.problems if p.code == "ev.suites_invalid")
    assert "humaneval" in p.message and st.health == "degraded"
    assert a.actions()[0].params[0].choices is None


async def test_metrics_scores_runs_and_verdicts(make_adapter: Any) -> None:
    a, _ = make_adapter()
    put_run(a, "20261024T080000-001-smoke-lightning", ts=1792844000.0,
            results=[{"bench": "gsm8k", "status": "ok", "score": 0.9, "samples": 10}])
    put_run(a, "20261024T070000-001-smoke-lightning", ts=1792840000.0,
            results=[{"bench": "gsm8k", "status": "ok", "score": 0.5, "samples": 10}])
    put_run(a, "20261024T060000-001-knowledge-lightning", verdict="failed",
            results=[{"bench": "mmlu", "status": "failed"}])
    put_run(a, "20261024T050000-001-knowledge-lightning", model="other/model",
            results=[{"bench": "mmlu", "status": "ok", "score": 0.48, "samples": 50}])
    (a._runs_dir / "20261024T040000-001-broken").mkdir()
    (a._runs_dir / "20261024T040000-001-broken" / "run.json").write_text("{not json")
    m = await a.metrics()
    assert m.available
    scores = {(p.labels["bench"], p.labels["model"]): p.value for p in m.points if p.key == "ev.score"}
    assert scores == {("gsm8k", "aitop/nemotron-lightning"): 0.9, ("mmlu", "other/model"): 0.48}   # newest only
    by = {p.labels["verdict"]: p.value for p in m.points if p.key == "ev.runs_by_verdict"}
    assert by == {"succeeded": 3.0, "failed": 1.0, "error": 0.0, "interrupted": 0.0}
    runs = next(p for p in m.points if p.key == "ev.runs")
    assert runs.value == 4.0                                  # the unreadable run.json is skipped, not fatal
    assert next(p for p in m.points if p.key == "ev.suites").value == 3.0


async def test_metrics_empty_has_no_fabricated_keys(make_adapter: Any) -> None:
    a, _ = make_adapter()
    m = await a.metrics()
    keys = {p.key for p in m.points}
    assert keys == {"ev.suites", "ev.runs"} and next(p for p in m.points if p.key == "ev.runs").value == 0.0


async def test_metrics_score_pairs_are_bounded(make_adapter: Any) -> None:
    a, _ = make_adapter()
    for i in range(60):
        put_run(a, f"20261024T0{i // 10}{i % 10}000-001-smoke-lightning", model=f"m{i}",
                results=[{"bench": "gsm8k", "score": 0.1, "status": "ok"}])
    m = await a.metrics()
    assert sum(1 for p in m.points if p.key == "ev.score") == 50


async def test_events_metadata_only_and_restart_reads_as_interrupted(make_adapter: Any) -> None:
    a, _ = make_adapter()
    put_run(a, RUN, results=[{"bench": "gsm8k", "status": "ok", "score": 1.0, "samples": 10, "metric": "pass@1"}],
            duration_ms=2100)
    put_run(a, "20261024T090000-001-knowledge-lightning", verdict="running")
    evs = await a.recent_events()
    assert [e.kind for e in evs] == ["evaluator.run"] * 2
    byrun = {e.data["run_id"]: e for e in evs}
    assert byrun[RUN].data["verdict"] == "succeeded" and byrun[RUN].data["benchmarks"][0]["score"] == 1.0
    assert "gsm8k 1.000" in byrun[RUN].summary
    assert byrun["20261024T090000-001-knowledge-lightning"].data["verdict"] == "interrupted"
    assert "prompt" not in json.dumps([e.model_dump() for e in evs])


async def test_follow_events_announces_new_runs(make_adapter: Any) -> None:
    a, _ = make_adapter()
    got: list[SystemEvent] = []

    async def consume() -> None:
        async for ev in a.follow_events(since_ts=None):
            got.append(ev)
            if len(got) == 1:
                return

    task = asyncio.create_task(consume())
    await asyncio.sleep(0.05)
    put_run(a, RUN, results=[])
    await asyncio.wait_for(task, 2)
    assert got[0].data["run_id"] == RUN


# ── configure ───────────────────────────────────────────────────────────────────────────────
async def test_config_files_and_read(make_adapter: Any) -> None:
    a, _ = make_adapter()
    [spec] = a.config_files()
    assert spec.id == "suites.yaml" and spec.editable and spec.format == "yaml" and spec.authoritative
    cf = await a.read_config("suites.yaml")
    assert cf.content == fx("suites_good.yaml") and len(cf.sha256) == 64
    with pytest.raises(KeyError):
        a.config_path("../../etc/passwd")
    with pytest.raises(KeyError):
        a.config_path("other.yaml")


async def test_config_path_refuses_symlink(make_adapter: Any, tmp_path: Path) -> None:
    a, _ = make_adapter()
    p = a._suites_path
    assert p is not None
    real = tmp_path / "elsewhere.yaml"
    real.write_text("x: 1\n")
    p.unlink()
    p.symlink_to(real)
    with pytest.raises(KeyError):
        a.config_path("suites.yaml")


async def test_validate_good_candidate(make_adapter: Any) -> None:
    a, run = make_adapter()
    v = await a.validate_config("suites.yaml", fx("suites_good.yaml"))
    assert v.ok and v.authoritative and [d for d in v.diagnostics if d.severity == "error"] == []
    assert any(c[:3] == [LAUNCHER, "suites", "--file"] for c in run.calls)
    assert v.note is not None and "agrees" in v.note
    assert list((a.ctx.state_dir / "evaluator" / "tmp").iterdir()) == []      # temp candidate deleted


@pytest.mark.parametrize(("bad", "needle"), [
    ("- not a mapping\n", "top level"),
    ("version: 2\nsuites: {a: {model: m, benchmarks: [{bench: gsm8k, max_problems: 1}]}}\n", "`version` must be 1"),
    ("version: 1\nsuites: {}\n", "non-empty mapping"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: humaneval, max_problems: 5}\n",
     "Docker sandbox"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: simpleqa, max_problems: 5}\n",
     "LLM judge"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: nope, max_problems: 5}\n",
     "unknown benchmark"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k}\n", "`max_problems` is required"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 99999}\n",
     "`max_problems`"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: true}\n",
     "`max_problems`"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5, url: x}\n",
     "unknown key 'url'"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    extra: 1\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
     "unknown key 'extra'"),
    ("version: 1\nsuites:\n  A_B:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
     "name must match"),
    ("version: 1\nsuites:\n  a:\n    model: 'bad model;rm'\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
     "`model`"),
    (("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n"
      "      - {bench: gsm8k, max_problems: 5}\n"), "listed twice"),
    ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5000, repeats: 8}\n",
     "sums to"),
    ("version: 1\nsuites: [unclosed\n", "YAML error"),
])
async def test_validate_broken_candidates(make_adapter: Any, bad: str, needle: str) -> None:
    a, run = make_adapter()
    v = await a.validate_config("suites.yaml", bad)
    assert not v.ok and v.authoritative
    assert any(needle in d.message for d in v.diagnostics), [d.message for d in v.diagnostics]
    assert not any(c[:2] == [LAUNCHER, "suites"] for c in run.calls)       # the schema stops it before the wrapper


async def test_validate_reports_line_numbers(make_adapter: Any) -> None:
    a, _ = make_adapter()
    bad = ("version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n"
           "      - bench: humaneval\n        max_problems: 5\n")
    v = await a.validate_config("suites.yaml", bad)
    assert [d.line for d in v.diagnostics] == [6]


async def test_validate_warns_about_unprefetched_datasets_but_is_ok(make_adapter: Any) -> None:
    a, _ = make_adapter()
    await a.status()                                   # primes the cached-datasets view
    cand = "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n"
    a._cached_cache = (a._cached_cache[0], {"gsm8k": False})
    v = await a.validate_config("suites.yaml", cand)
    assert v.ok and any(d.severity == "warn" and "prefetch gsm8k" in d.message for d in v.diagnostics)


async def test_validate_wrapper_disagreement_is_an_error(make_adapter: Any) -> None:
    r = standard_runner().on([LAUNCHER, "suites"], ok("", rc=1, stderr="evaluator-local: invalid suites file:\n  boom"))
    a, _ = make_adapter(r)
    v = await a.validate_config("suites.yaml", fx("suites_good.yaml"))
    assert not v.ok and any("boom" in d.message for d in v.diagnostics)


async def test_validate_without_tool_uses_schema_only(make_adapter: Any) -> None:
    a, _ = make_adapter(FakeRunner())
    v = await a.validate_config("suites.yaml", fx("suites_good.yaml"))
    assert v.ok and v.note is not None and "agrees" not in v.note


async def test_post_apply_is_healthy_and_never_restarts(make_adapter: Any) -> None:
    a, run = make_adapter()
    r = await a.post_apply("suites.yaml", restart=True)
    assert r.ok and r.post_check == "healthy" and not r.restarted and run.calls == []


def test_validate_suites_accepts_the_shipped_file() -> None:
    import yaml
    assert validate_suites(yaml.safe_load(fx("suites_good.yaml"))) == []


def test_standard_runner_fixture_is_consistent() -> None:
    assert isinstance(standard_runner(), FakeRunner)
