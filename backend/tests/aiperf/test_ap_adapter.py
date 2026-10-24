"""AIPerf adapter: status, metrics, config validation (profiles.yaml schema), actions listing. Offline."""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import httpx
import pytest
from ap_helpers import LAUNCHER, FakeRunner, fx

from nemo_console.adapters.aiperf import AiPerfAdapter, parse_profiles, render_argv, summarize
from nemo_console.services.runner import AIPERF_PROFILE, GRACEFUL_BINARIES, max_timeout


def codes(st) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in st.problems}


def pts(snap, key: str) -> dict[str, float]:  # type: ignore[no-untyped-def]
    return {p.labels.get("q", p.labels.get("verdict", "")): p.value for p in snap.points if p.key == key}


def seed_run(a: AiPerfAdapter, run_id: str, **meta: Any) -> Path:
    d = a._runs_dir / run_id
    d.mkdir(parents=True)
    base: dict[str, Any] = {"run_id": run_id, "preset": run_id.split("-", 2)[2], "ts": 1_700_000_000.0,
                            "verdict": "succeeded", "model": "aitop/nemotron-lightning", "concurrency": 2}
    (d / "run.json").write_text(json.dumps({**base, **meta}))
    return d


# ── status ─────────────────────────────────────────────────────────────────────────────────────
async def test_status_up(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    st = await a.status()
    assert st.system == "aiperf" and st.health == "up" and st.version == "0.13.0" and st.problems == []
    assert st.summary.startswith("3 preset(s)")
    assert [e.name for e in st.endpoints] == ["router (load target)"] and st.endpoints[0].auth == "bearer"
    assert {c.name for c in st.components} >= {LAUNCHER, "router http://127.0.0.1:4000"}
    assert run.calls == [[LAUNCHER, "--version"]]
    await a.status()                                   # the version probe is TTL-cached
    assert run.calls.count([LAUNCHER, "--version"]) == 1


async def test_status_absent_launcher_is_down_with_a_fix(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(FakeRunner())                  # every argv -> rc 127
    st = await a.status()
    assert st.health == "down" and "ap.not_installed" in codes(st)
    p = next(p for p in st.problems if p.code == "ap.not_installed")
    assert p.severity == "error" and p.fix is not None and "clients/aiperf/install.sh" in p.fix
    assert not any(c.state == "running" and c.name == LAUNCHER for c in st.components)


async def test_status_install_fix_uses_the_repo_path_when_present(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    repo = tmp_path / "repo"
    (repo / "clients" / "aiperf").mkdir(parents=True)
    (repo / "clients" / "aiperf" / "install.sh").write_text("#!/bin/sh\n")
    a, _ = make_adapter(FakeRunner(), repo=repo)
    p = next(p for p in (await a.status()).problems if p.code == "ap.not_installed")
    assert p.fix is not None and p.fix.startswith(str(repo / "clients" / "aiperf" / "install.sh"))


async def test_status_router_down_degrades(make_adapter, router) -> None:  # type: ignore[no-untyped-def]
    router.get("http://127.0.0.1:4000/health/liveliness").mock(side_effect=httpx.ConnectError("refused"))
    a, _ = make_adapter()
    st = await a.status()
    assert st.health == "degraded" and "ap.router_down" in codes(st)
    assert any(c.kind == "http" and c.state == "stopped" for c in st.components)


async def test_status_missing_key_warns_with_mint_key_fix(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(key_file=False)
    st = await a.status()
    assert st.health == "degraded"
    p = next(p for p in st.problems if p.code == "ap.no_key")
    assert p.severity == "warn" and p.fix is not None and "mint-key.sh" in p.fix


async def test_status_never_reads_the_key_file(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    key = a.ctx.paths["aiperf_key_file"]
    key.chmod(0o000)                                   # unreadable: existence is all that is checked
    try:
        assert "ap.no_key" not in codes(await a.status())
    finally:
        key.chmod(0o600)


async def test_status_missing_tokenizer_cache_warns_with_the_prefetch_fix(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(tokenizer=False)
    st = await a.status()
    p = next(p for p in st.problems if p.code == "ap.no_tokenizer")
    assert st.health == "degraded" and p.fix is not None and "prefetch" in p.fix


async def test_status_profiles_missing_and_invalid(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    a.ctx.paths["aiperf_profiles"].unlink()
    assert "ap.no_profiles" in codes(await a.status())
    a.ctx.paths["aiperf_profiles"].write_text("version: 1\nprofiles:\n  smoke:\n    model: m\n")
    st = await a.status()
    p = next(p for p in st.problems if p.code == "ap.profiles_invalid")
    assert "'concurrency' is required" in p.message and "line" in p.message
    a.ctx.paths["aiperf_profiles"].write_text("profiles: [unclosed\n")
    assert "ap.profiles_invalid" in codes(await a.status())          # a broken file is data, not an exception


async def test_status_summary_shows_the_newest_run(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    seed_run(a, "20261024T100000-001-smoke-lightning", metrics={"ttft_p50_ms": 29.96, "output_tps": 1533.0})
    st = await a.status()
    assert "last run: smoke-lightning" in st.summary and "TTFT p50 30 ms" in st.summary and "1533 output tok/s" in st.summary


# ── metrics ────────────────────────────────────────────────────────────────────────────────────
async def test_metrics_without_runs_has_only_the_count(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    snap = await a.metrics()
    assert snap.available and [(p.key, p.value) for p in snap.points] == [("ap.runs", 0.0)]


async def test_metrics_from_the_captured_summary(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    doc = json.loads(fx("profile_export_aiperf.json"))
    m = summarize(doc)
    assert m["ttft_p50_ms"] == pytest.approx(29.9637365) and m["ttft_p99_ms"] == pytest.approx(40.695334159999994)
    assert m["itl_p50_ms"] == pytest.approx(0.7912008333333332) and m["output_tps"] == pytest.approx(1532.9910176367441)
    assert m["request_tps"] == pytest.approx(23.952984650574127) and m["requests"] == 8.0 and m["error_rate_pct"] == 0.0
    seed_run(a, "20261024T100000-001-smoke-lightning", metrics=m)
    seed_run(a, "20261024T110000-001-smoke-lightning", verdict="failed", error="boom")       # newer, but not usable
    snap = await a.metrics()
    assert pts(snap, "ap.ttft_ms") == {"p50": pytest.approx(29.9637365), "p99": pytest.approx(40.695334159999994)}
    assert pts(snap, "ap.itl_ms")["p50"] == pytest.approx(0.7912008333333332)
    assert pts(snap, "ap.output_tps") == {"": pytest.approx(1532.9910176367441)}
    assert pts(snap, "ap.request_tps") == {"": pytest.approx(23.952984650574127)}
    assert pts(snap, "ap.runs") == {"": 2.0}
    assert pts(snap, "ap.runs_by_verdict") == {"succeeded": 1.0, "failed": 1.0, "error": 0.0, "interrupted": 0.0}
    assert {p.unit for p in snap.points if p.key == "ap.output_tps"} == {"tok/s"}


async def test_metrics_tolerates_missing_keys(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    seed_run(a, "20261024T100000-001-smoke-lightning", metrics={"ttft_p50_ms": 12.0})   # no p99, itl, tps
    snap = await a.metrics()
    assert pts(snap, "ap.ttft_ms") == {"p50": 12.0}
    assert not [p for p in snap.points if p.key in ("ap.itl_ms", "ap.output_tps", "ap.request_tps")]
    assert summarize({}) == {} and summarize({"time_to_first_token": "garbage", "request_count": {"avg": True}}) == {}


async def test_summarize_never_fabricates_non_numbers() -> None:
    doc: dict[str, Any] = {"time_to_first_token": {"p50": "fast", "p99": None}, "output_token_throughput": {"avg": 5}}
    assert summarize(doc) == {"output_tps": 5.0}


# ── events ─────────────────────────────────────────────────────────────────────────────────────
async def test_events_are_metadata_only(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    seed_run(a, "20261024T100000-001-smoke-lightning", requests=8, isl=128, osl=64, duration_ms=900,
             metrics={"ttft_p50_ms": 30.0, "output_tps": 1500.0}, error="sk-abcdefghijklmnop0123 leaked?")
    [ev] = await a.recent_events()
    assert ev.kind == "aiperf.run" and ev.system == "aiperf" and ev.ts == 1_700_000_000.0
    assert ev.data["preset"] == "smoke-lightning" and ev.data["ttft_p50_ms"] == 30.0 and ev.data["requests"] == 8
    assert "error" not in ev.data and "metrics" not in ev.data and "sk-" not in json.dumps(ev.model_dump())


async def test_follow_events_announces_each_run_state_once(make_adapter) -> None:  # type: ignore[no-untyped-def]
    import asyncio
    a, _ = make_adapter()
    got: list[str] = []

    async def consume() -> None:
        async for ev in a.follow_events(since_ts=0.0):
            got.append(str(ev.data["verdict"]))

    task = asyncio.create_task(consume())
    await asyncio.sleep(0.05)
    d = seed_run(a, "20261024T100000-001-smoke-lightning", verdict="running", ts=9e9)
    await asyncio.sleep(0.05)
    (d / "run.json").write_text(json.dumps({"run_id": d.name, "preset": "smoke-lightning", "ts": 9e9, "verdict": "succeeded"}))
    await asyncio.sleep(0.05)
    task.cancel()
    assert got == ["interrupted", "succeeded"]          # a stored "running" with no live task reads as interrupted


async def test_a_run_left_running_by_a_dead_process_reads_as_interrupted(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    seed_run(a, "20261024T100000-001-smoke-lightning", verdict="running")
    [ev] = await a.recent_events()
    assert ev.data["verdict"] == "interrupted"


# ── config: profiles.yaml ──────────────────────────────────────────────────────────────────────
async def test_config_file_surface(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    [spec] = a.config_files()
    assert spec.id == "profiles" and spec.editable and spec.authoritative and spec.format == "yaml"
    assert a.config_path("profiles") == a.ctx.paths["aiperf_profiles"]
    with pytest.raises(KeyError):
        a.config_path("other")
    cf = await a.read_config("profiles")
    assert cf.content == fx("profiles.yaml") and len(cf.sha256) == 64
    res = await a.post_apply("profiles", restart=False)
    assert res.ok and res.post_check == "healthy" and not res.restarted
    with pytest.raises(KeyError):
        await a.post_apply("nope", restart=False)


async def test_config_path_refuses_a_symlink(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    real = tmp_path / "elsewhere.yaml"
    real.write_text("version: 1\n")
    p = a.ctx.paths["aiperf_profiles"]
    p.unlink()
    p.symlink_to(real)
    with pytest.raises(KeyError):
        a.config_path("profiles")
    assert next(x for x in a.actions() if x.id == "profile").params[0].choices is None


async def test_the_shipped_profiles_validate_clean() -> None:
    presets, diags = parse_profiles(fx("profiles.yaml"))
    assert diags == [] and sorted(presets) == ["latency-lightning", "smoke-lightning", "throughput-lightning"]
    s = presets["smoke-lightning"]
    assert (s.model, s.concurrency, s.requests, s.warmup_requests, s.isl, s.osl) == (
        "aitop/nemotron-lightning", 2, 8, 1, 128, 64)
    assert not any(p.model.lower().startswith("glm") for p in presets.values())   # the main model is never a preset


def errs(text: str) -> list[str]:
    return [d.message for d in parse_profiles(text)[1] if d.severity == "error"]


MINIMAL = "version: 1\nprofiles:\n  p1:\n    model: m\n    concurrency: 1\n    requests: 2\n    isl: 8\n    osl: 8\n"


@pytest.mark.parametrize(("text", "needle"), [
    ("profiles: [unclosed\n", "YAML syntax"),
    ("- a\n- b\n", "must be a mapping"),
    ("version: 2\nprofiles: {}\n", "'version' must be 1"),
    ("version: true\nprofiles: {}\n", "'version' must be 1"),
    ("version: 1\n", "at least one preset"),
    ("version: 1\nprofiles: {}\n", "at least one preset"),
    ("version: 1\nprofils: {}\nprofiles: {a: {}}\n", "unknown top-level key 'profils' (did you mean 'profiles'?)"),
    (MINIMAL.replace("p1:", "Bad_Name:"), "must match"),
    (MINIMAL.replace("p1:", "p1: 5\n  p2:").replace("    model: m", "    model: m"), "must be a mapping"),
    (MINIMAL.replace("concurrency: 1", "concurrency: 0"), "'concurrency' must be an integer from 1 to 64"),
    (MINIMAL.replace("concurrency: 1", "concurrency: 65"), "'concurrency' must be an integer from 1 to 64"),
    (MINIMAL.replace("concurrency: 1", "concurrency: true"), "'concurrency' must be an integer"),
    (MINIMAL.replace("concurrency: 1", "concurrency: '2'"), "'concurrency' must be an integer"),
    (MINIMAL.replace("concurrency: 1", "concurrency: 1.5"), "'concurrency' must be an integer"),
    (MINIMAL.replace("requests: 2", "requests: 2001"), "'requests' must be an integer from 1 to 2000"),
    (MINIMAL.replace("isl: 8", "isl: 40000"), "'isl' must be an integer from 1 to 32768"),
    (MINIMAL.replace("osl: 8", "osl: 5000"), "'osl' must be an integer from 1 to 4096"),
    (MINIMAL.replace("    osl: 8\n", ""), "'osl' is required"),
    (MINIMAL.replace("    model: m\n", ""), "'model' is required"),
    (MINIMAL.replace("model: m", "model: 'a b; rm -rf /'"), "'model' is required"),
    (MINIMAL + "    endpoint_type: embeddings\n", "'endpoint_type' must be one of chat, completions"),
    (MINIMAL + "    streaming: yes please\n", "'streaming' must be true or false"),
    (MINIMAL + "    concurency: 3\n", "unknown key 'concurency' (did you mean 'concurrency'?)"),
    (MINIMAL + "    warmup_requests: 101\n", "'warmup_requests' must be an integer from 0 to 100"),
    (MINIMAL + "    timeout_s: 0\n", "'timeout_s' must be an integer from 1 to 600"),
    (MINIMAL + "    seed: -1\n", "'seed' must be an integer from 0 to 2147483647"),
    (MINIMAL + "    description: |\n      two\n      lines\n", "'description' must be a single line"),
])
async def test_validator_rejects(text: str, needle: str) -> None:
    got = errs(text)
    assert any(needle in m for m in got), got


async def test_validator_warns_but_does_not_fail() -> None:
    presets, diags = parse_profiles(MINIMAL.replace("requests: 2", "requests: 1").replace("concurrency: 1", "concurrency: 4")
                                    + "    streaming: false\n")
    assert "p1" in presets and {d.severity for d in diags} == {"warn"}
    assert any("not measured" in d.message for d in diags) and any("fewer requests" in d.message for d in diags)


async def test_validator_reports_line_numbers() -> None:
    text = MINIMAL.replace("isl: 8", "isl: 0")
    [d] = [d for d in parse_profiles(text)[1] if d.severity == "error"]
    assert d.line == text.splitlines().index("    isl: 0") + 1
    [d2] = parse_profiles("profiles: [unclosed\n")[1]
    assert d2.line is not None


async def test_validator_drops_only_the_bad_preset() -> None:
    text = MINIMAL + "  p2:\n    model: m\n    concurrency: 0\n    requests: 1\n    isl: 1\n    osl: 1\n"
    presets, diags = parse_profiles(text)
    assert list(presets) == ["p1"] and len([d for d in diags if d.severity == "error"]) == 1


async def test_validator_caps_size_and_count() -> None:
    assert errs("x: " + "a" * 300_000)
    many = "version: 1\nprofiles:\n" + "".join(
        f"  p{i}:\n    model: m\n    concurrency: 1\n    requests: 1\n    isl: 1\n    osl: 1\n" for i in range(40))
    assert any("at most 32" in m for m in errs(many))


async def test_validate_config_is_authoritative(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    good = await a.validate_config("profiles", fx("profiles.yaml"))
    assert good.ok and good.authoritative and good.validator == "nemo-console aiperf profiles schema" and good.note
    bad = await a.validate_config("profiles", MINIMAL.replace("isl: 8", "isl: 0"))
    assert not bad.ok and bad.diagnostics[0].severity == "error"
    with pytest.raises(KeyError):
        await a.validate_config("other", "")


# ── actions listing + the runner contract ─────────────────────────────────────────────────────
async def test_actions_come_from_the_profiles_file(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    prof, rep = a.actions()
    assert (prof.id, prof.danger, prof.role) == ("profile", "disruptive", "admin")
    assert prof.params[0].type == "enum" and prof.params[0].choices == [
        "latency-lightning", "smoke-lightning", "throughput-lightning"]
    assert prof.params[0].default == "latency-lightning" and not prof.params[0].requires_restart
    assert (rep.id, rep.danger) == ("report", "safe") and rep.params[0].type == "string"
    a.ctx.paths["aiperf_profiles"].write_text(fx("profiles.yaml") + "  extra-one:\n    model: m\n    concurrency: 1\n"
                                              "    requests: 1\n    isl: 1\n    osl: 1\n")
    assert "extra-one" in a.actions()[0].params[0].choices                      # type: ignore[operator]


async def test_render_argv_golden_and_runner_contract() -> None:
    presets, _ = parse_profiles(fx("profiles.yaml"))
    art = Path("/state/aiperf/runs/R/artifacts")
    assert render_argv(LAUNCHER, presets["smoke-lightning"], art) == [
        "aiperf-local", "profile", "--model", "aitop/nemotron-lightning", "--endpoint-type", "chat", "--streaming",
        "--concurrency", "2", "--request-count", "8", "--prompt-input-tokens-mean", "128",
        "--prompt-output-tokens-mean", "64", "--warmup-request-count", "1", "--request-timeout-seconds", "120",
        "--random-seed", "42", "--export-level", "records", "--use-server-token-count",
        "--artifact-dir", "/state/aiperf/runs/R/artifacts"]
    assert render_argv(LAUNCHER, presets["latency-lightning"], art)[:7] == [
        "aiperf-local", "profile", "--model", "aitop/nemotron-lightning", "--endpoint-type", "chat", "--streaming"]
    from dataclasses import replace
    no_stream = render_argv(LAUNCHER, replace(presets["smoke-lightning"], streaming=False, warmup_requests=0), art)
    assert "--streaming" not in no_stream and "--warmup-request-count" not in no_stream
    for p in presets.values():
        argv = render_argv(LAUNCHER, p, art)
        assert max_timeout(argv) == AIPERF_PROFILE == 3600.0 and LAUNCHER in GRACEFUL_BINARIES
        assert max_timeout([LAUNCHER, "--version"]) == 600.0
        # nothing secret can be in this argv: no credential flag, no header, no URL (the wrapper owns all three)
        assert not {"--api-key", "-H", "--header", "--url", "-u", "--config"} & set(argv)
        assert not any(a.startswith("sk-") for a in argv)


async def test_the_adapter_is_registered_and_labelled(make_adapter) -> None:  # type: ignore[no-untyped-def]
    from nemo_console.adapters import build_adapter
    a, _ = make_adapter()
    built = build_adapter("aiperf", a.ctx)
    assert isinstance(built, AiPerfAdapter) and built.id == "aiperf" and built.label == "AIPerf"
