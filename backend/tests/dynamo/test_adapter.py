"""Adapter behaviour with a fake runner, respx and CAPTURED real fixtures (frontend/worker inspect, metrics, logs)."""
from __future__ import annotations

import copy
import json
import stat
from pathlib import Path
from typing import Any

import httpx
import respx

from nemo_console.adapters.base import CommandResult

from .conftest import BASE_URL
from .helpers import FakeRunner, fail, frontend_doc, fx, fxj, inspect_json, is_docker, ok, worker_doc

FE, WK, ENG = "ai-lab-dynamo-dynamo-frontend-1", "ai-lab-dynamo-dynamo-worker-1", "ai-lab-vllm-qwen38-vllm-1"
UUID = "GPU-00000000-0000-0000-0000-000000000001"


def engine_doc(running: bool = True, name: str = ENG, project: str = "ai-lab-vllm-qwen38", uuid: str = UUID) -> dict[str, Any]:
    """Synthesised from the REAL worker inspect shape (the live engine is stopped, so none could be captured)."""
    d = copy.deepcopy(worker_doc())
    d["Name"] = "/" + name
    d["State"].update(Running=running, Status="running" if running else "exited")
    lb = d["Config"]["Labels"]
    lb.update({"com.docker.compose.project": project, "com.docker.compose.service": "vllm",
               "com.docker.compose.project.working_dir": "/repo/inference/vllm-qwen38",
               "com.docker.compose.project.config_files": "/repo/inference/vllm-qwen38/docker-compose.yml"})
    d["HostConfig"]["DeviceRequests"][0]["DeviceIDs"] = [uuid]
    d["Config"]["Image"] = "vllm/vllm-openai:v0.27.1"
    return d


def wire(runner: FakeRunner, docs: list[dict[str, Any]], ps: str = "") -> None:
    def _inspect(argv: list[str]) -> CommandResult:
        want = set(argv[2:])
        return inspect_json(*[d for d in docs if d["Name"].lstrip("/") in want])
    runner.on(is_docker("inspect"), _inspect)
    runner.on(is_docker("ps"), ok(ps))


def wire_worker_exec(runner: FakeRunner, health: str | None = None) -> None:
    body = (health or fx("worker_health.json")).strip() + "\n---METRICS---\n" + fx("worker_metrics.prom")
    runner.on(is_docker("exec"), ok(body))


def http_live(mock: respx.MockRouter, models: bool = True) -> None:
    mock.get(f"{BASE_URL}/health").respond(200, text=fx("frontend_health.json"))
    mock.get(f"{BASE_URL}/v1/models").respond(200, json=fxj("frontend_models.json") if models else {"object": "list", "data": []})
    mock.get(f"{BASE_URL}/metrics").respond(200, text=fx("frontend_metrics.prom"))


def codes(st: Any) -> set[str]:
    return {p.code for p in st.problems}


# ── status ──────────────────────────────────────────────────────────────────────────────────────
async def test_status_up_real_fixtures(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [frontend_doc(), worker_doc(), engine_doc(running=False)])
    http_live(mock)
    st = await make().status()
    assert st.health == "up" and st.version == "1.5.0"
    assert "unsloth/Qwen3.8-27B-NVFP4" in st.summary
    assert [c.name for c in st.components] == [FE, WK] and all(c.state == "running" for c in st.components)
    # the engine is stopped while Dynamo holds the card -> named dark-alias problem
    p = next(p for p in st.problems if p.code == "dynamo.holds_card")
    assert "qwen3.8-27b-1m" in p.message and "Switchyard capable tier" in p.message and p.fix
    # the 27B is restored through use-model.sh (it frees the card flashnext TP2 holds), never a bare compose up
    assert p.fix.endswith("scripts/use-model.sh qwen3.8-27b-1m") and "docker compose up" not in p.fix
    assert "dynamo.structural_tag.off" not in codes(st)          # the live worker has the flag
    assert {e.role for e in st.endpoints} == {"api", "health", "metrics"}


async def test_status_loading_when_models_empty(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [frontend_doc(), worker_doc()])
    http_live(mock, models=False)
    st = await make().status()
    assert st.health == "degraded" and st.summary.startswith("loading")
    assert "loading" in (st.components[1].detail or "")


async def test_status_down_when_nothing_exists(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [])
    st = await make().status()
    assert st.health == "down" and {"dynamo.frontend.down", "dynamo.worker.absent"} <= codes(st)
    assert all(c.state == "absent" for c in st.components)


async def test_status_frontend_up_worker_absent(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [frontend_doc()])
    http_live(mock, models=False)
    st = await make().status()
    assert st.health == "degraded" and "dynamo.worker.absent" in codes(st)


async def test_status_flags_missing_structural_tag_and_wrong_parser_dialect(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    w = worker_doc()
    cmd = [c for c in w["Config"]["Cmd"] if c != "--dyn-enable-structural-tag"] + ["--tool-call-parser", "hermes"]
    w["Config"]["Cmd"] = cmd
    wire(runner, [frontend_doc(), w, engine_doc(running=False)])
    http_live(mock)
    st = await make().status()
    assert {"dynamo.structural_tag.off", "dynamo.parser.dialect"} <= codes(st)
    assert all(p.fix for p in st.problems)


async def test_status_contended_card_and_kv_router(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    f = frontend_doc()
    f["Config"]["Cmd"] = [*f["Config"]["Cmd"], "--router-mode", "kv"]
    wire(runner, [f, worker_doc(), engine_doc(running=True)])
    http_live(mock)
    st = await make().status()
    assert {"dynamo.card.contended", "dynamo.router.kv"} <= codes(st)


async def test_status_never_raises(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    runner.on(is_docker("inspect"), lambda a: (_ for _ in ()).throw(RuntimeError("socket gone")))
    st = await make().status()
    assert st.health == "unknown" and st.problems[0].fix


# ── metrics ─────────────────────────────────────────────────────────────────────────────────────
def by_key(snap: Any) -> dict[str, list[Any]]:
    out: dict[str, list[Any]] = {}
    for p in snap.points:
        out.setdefault(p.key, []).append(p)
    return out


async def test_metrics_map_real_names(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [worker_doc()])
    wire_worker_exec(runner)
    http_live(mock)
    snap = await make().metrics()
    k = by_key(snap)
    assert snap.available and k["dyn.up"][0].value == 1 and k["dyn.worker_up"][0].value == 1
    assert k["dyn.models"][0].value == 1
    assert k["dyn.requests_total"][0].labels == {"status": "success"} and k["dyn.requests_total"][0].value == 1
    assert k["dyn.in_flight"][0].value == 0
    toks = {p.labels["kind"]: p.value for p in k["dyn.tokens"]}
    assert toks == {"prompt": 18.0, "completion": 40.0, "cached": 0.0}
    assert k["dyn.kv_cache_usage"][0].value == 0.0 and k["dyn.kv_cache_usage"][0].unit == "ratio"
    # lifetime quantile of the single 0.302 s TTFT sample lands inside its (0.22, 0.47] bucket
    p50 = next(p for p in k["dyn.ttft_ms"] if p.labels["q"] == "p50")
    assert 220 < p50.value <= 470
    itl = {p.labels["q"] for p in k["dyn.itl_ms"]}
    assert itl == {"p50", "p95"}
    assert abs(k["dyn.spec_accept_ratio"][0].value - 26 / 30) < 1e-9
    assert not any(x.key == "dyn.tokens" and x.labels["kind"] == "reasoning" for x in snap.points)   # never invented


async def test_metrics_tolerates_missing_worker_and_empty_frontend(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [])
    mock.get(f"{BASE_URL}/metrics").respond(200, text="# nothing\n")
    snap = await make().metrics()
    keys = {p.key for p in snap.points}
    assert snap.available and keys == {"dyn.up", "dyn.worker_up"}
    assert by_key(snap)["dyn.worker_up"][0].value == 0


async def test_metrics_frontend_down_is_data(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    mock.get(f"{BASE_URL}/metrics").mock(side_effect=httpx.ConnectError("refused"))
    snap = await make().metrics()
    assert snap.available is False and snap.points == [] and snap.error


async def test_worker_read_uses_exec_not_host_ip(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [worker_doc()])
    wire_worker_exec(runner)
    http_live(mock)
    await make().metrics()
    ex = next(a for a in runner.argvs() if a[:2] == ["docker", "exec"])
    assert ex[2] == WK and ex[3] == "python3" and "8081" in ex[5]


# ── events ──────────────────────────────────────────────────────────────────────────────────────
async def test_recent_events_redacted_bounded_and_denoised(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [frontend_doc(), worker_doc()])
    runner.on(lambda a: a[:2] == ["docker", "logs"] and a[-1] == FE, ok(fx("frontend_logs.txt")))
    extra = "2026-09-24T08:09:30.000000000Z INFO auth Authorization: Bearer abcdefghijkl token=hf_ABCDEFGHIJKLMNOP\n"
    runner.on(lambda a: a[:2] == ["docker", "logs"] and a[-1] == WK, ok(fx("worker_logs.txt") + extra))
    evs = await make().recent_events(limit=30)
    assert 0 < len(evs) <= 30 and all(e.kind == "dynamo.log" and e.system == "dynamo" for e in evs)
    assert [e.ts for e in evs] == sorted(e.ts for e in evs)
    blob = "\n".join(e.summary for e in evs)
    assert "abcdefghijkl" not in blob and "hf_ABCDEF" not in blob and "\x1b" not in blob
    assert "/root/" not in blob and "/home/" not in blob and "/usr/local" not in blob
    assert "http response sent" not in blob                     # 2 s health-poll spam dropped
    assert {e.data["component"] for e in evs} <= {"frontend", "worker"}
    tail = next(a for a in runner.argvs() if a[:2] == ["docker", "logs"])
    assert "--tail" in tail and int(tail[tail.index("--tail") + 1]) <= 1000


async def test_recent_events_refuses_unverified_container(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    imposter = worker_doc()
    imposter["Config"]["Labels"]["com.docker.compose.project"] = "somebody-else"
    wire(runner, [frontend_doc(), imposter])
    runner.on(lambda a: a[:2] == ["docker", "logs"], ok("2026-09-24T08:00:00Z hello\n"))
    evs = await make().recent_events(10)
    assert all(e.data["component"] == "frontend" for e in evs)


async def test_follow_events_yields_new_lines_and_terminates(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [worker_doc()])
    lines = ("2026-09-24T09:00:01.100000000Z INFO first\n2026-09-24T09:00:02.200000000Z WARN second\n")
    runner.on(lambda a: a[:2] == ["docker", "logs"], ok(lines))
    ad = make()
    got = []
    agen = ad.follow_events(since_ts=1_790_000_000.0)
    async for ev in agen:
        got.append(ev)
        if len(got) == 2:
            break
    await agen.aclose()
    assert [e.data["level"] for e in got] == ["INFO", "WARN"]
    log = next(a for a in runner.argvs() if a[:2] == ["docker", "logs"])
    assert "--since" in log


# ── config files + params ───────────────────────────────────────────────────────────────────────
async def test_config_files_and_readonly_compose(make, compose) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    specs = {s.id: s for s in ad.config_files()}
    assert specs["compose"].editable is False and "override" in (specs["compose"].why_readonly or "")
    assert specs["override"].editable is True
    assert ad.config_path("compose") == compose
    assert ad.config_path("override").name == "override.yml"
    import pytest
    with pytest.raises(KeyError):
        ad.config_path("../../etc/passwd")
    cf = await ad.read_config("compose")
    assert "dynamo.vllm" in cf.content and len(cf.sha256) == 64
    empty = await ad.read_config("override")
    assert empty.content == "" and empty.mtime == 0.0


async def test_validate_override(make, runner) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    runner.on(is_docker("compose"), ok())
    good = fx("golden_override.yml")
    r = await ad.validate_config("override", good)
    assert r.ok and r.authoritative
    assert not list((ad.ctx.state_dir / "dynamo" / "tmp").iterdir())        # candidate temp file cleaned up
    assert not (ad.ctx.state_dir / "dynamo" / "override.yml").exists()      # validate never writes the live file
    bad = "services:\n  dynamo-worker:\n    privileged: true\n"
    r2 = await ad.validate_config("override", bad)
    assert not r2.ok and "only `command`" in r2.diagnostics[0].message
    r3 = await ad.validate_config("override", "services:\n  evil:\n    command: [x]\n")
    assert not r3.ok
    flag = "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm, --trust-remote-code]\n"
    assert not (await ad.validate_config("override", flag)).ok
    secret = "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm, --model, sk-abcdefghijklmnop]\n"
    assert not (await ad.validate_config("override", secret)).ok
    nost = "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm, --max-num-seqs, '4']\n"
    r4 = await ad.validate_config("override", nost)
    assert r4.ok and any("structural" in d.message for d in r4.diagnostics if d.severity == "warn")
    assert not (await ad.validate_config("override", "a: [")).ok


async def test_validate_uses_compose_config_and_reports_failure(make, runner) -> None:  # type: ignore[no-untyped-def]
    runner.on(is_docker("compose"), fail("services.dynamo-worker.command must be a list"))
    r = await make().validate_config("override", fx("golden_override.yml"))
    assert not r.ok and "docker compose config" in r.diagnostics[-1].message
    argv = runner.argvs()[0]
    assert argv[:2] == ["docker", "compose"] and argv[-2:] == ["config", "-q"] and argv.count("-f") == 2


async def test_preview_is_pure_and_apply_writes_0600(make, runner, mock, compose) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    values = {"max_model_len": 131072, "gpu_memory_utilization": 0.6, "num_speculative_tokens": 3,
              "enable_prefix_caching": False, "router_mode": "kv", "reasoning_parser": "none"}
    preview = await ad.preview_params(values)
    assert preview == fx("golden_override.yml")
    assert not (ad.ctx.state_dir / "dynamo").exists()                       # preview touched nothing
    runner.on(is_docker("compose"), ok())
    wire(runner, [])                                                        # no worker: write only, no recreate
    res = await ad.apply_params(values)
    ov = ad.ctx.state_dir / "dynamo" / "override.yml"
    assert res.ok and res.post_check == "skipped" and not res.restarted and res.new_sha256
    assert ov.read_text() == preview and stat.S_IMODE(ov.stat().st_mode) == 0o600
    assert stat.S_IMODE(ov.parent.stat().st_mode) == 0o700
    assert ad.current_values()["router_mode"] == "kv"
    assert not any("up" in a for a in runner.argvs() if a[:2] == ["docker", "compose"])


async def test_apply_rejects_bad_values_and_rolls_back_bad_compose(make, runner) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    r = await ad.apply_params({"max_num_seqs": 0})
    assert not r.ok and not (ad.ctx.state_dir / "dynamo" / "override.yml").exists()
    runner.on(is_docker("compose"), fail("invalid"))
    r2 = await ad.apply_params({"max_num_seqs": 8})
    assert not r2.ok and "rejected" in r2.detail and not (ad.ctx.state_dir / "dynamo" / "override.yml").exists()


async def test_apply_recreates_worker_then_health_polls(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    old = worker_doc()
    new = copy.deepcopy(old)
    new["State"]["StartedAt"] = "2026-09-24T09:30:00.000000000Z"
    state = {"up_done": False}

    def _inspect(argv: list[str]) -> CommandResult:
        return inspect_json(new if state["up_done"] else old)

    def _compose(argv: list[str]) -> CommandResult:
        if "up" in argv:
            state["up_done"] = True
        return ok()

    runner.on(is_docker("inspect"), _inspect)
    runner.on(is_docker("compose"), _compose)
    http_live(mock)
    res = await ad.apply_params({"max_num_seqs": 16})
    assert res.ok and res.restarted and res.post_check == "healthy"
    up = next(a for a in runner.argvs() if "up" in a and a[:2] == ["docker", "compose"])
    assert up[-4:] == ["up", "-d", "dynamo-frontend", "dynamo-worker"] and "-v" not in up
    assert up.count("-f") == 1 or up.count("-f") == 2
    assert up[up.index("-f") + 1].endswith("docker-compose.yml")


async def test_apply_unhealthy_restores_previous_override(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    ad = make(health_timeout=0.05)
    ov = ad.ctx.state_dir / "dynamo" / "override.yml"
    ov.parent.mkdir(parents=True)
    ov.write_text("# previous\nservices: {}\n")
    old = worker_doc()
    runner.on(is_docker("inspect"), inspect_json(old))          # StartedAt never changes -> never "recreated"
    runner.on(is_docker("compose"), ok())
    http_live(mock)
    res = await ad.apply_params({"max_num_seqs": 16})
    assert not res.ok and res.post_check == "unhealthy" and res.rolled_back
    assert ov.read_text() == "# previous\nservices: {}\n"


async def test_post_apply(make, runner) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    assert (await ad.post_apply("compose", restart=True)).ok is False
    r = await ad.post_apply("override", restart=False)
    assert r.ok and r.post_check == "skipped"


# ── actions ─────────────────────────────────────────────────────────────────────────────────────
def compose_calls(runner: FakeRunner) -> list[list[str]]:
    return [a for a in runner.argvs() if a[:2] == ["docker", "compose"]]


async def test_action_catalog_matches_contract(make) -> None:  # type: ignore[no-untyped-def]
    acts = {a.id: a.danger for a in make().actions()}
    assert acts == {"preflight": "safe", "up": "disruptive", "down": "disruptive", "restart": "disruptive",
                    "up_mock": "safe"}


async def test_preflight_lists_sharers_by_uuid_and_ignores_other_card(make, runner) -> None:  # type: ignore[no-untyped-def]
    other_card = engine_doc(name="ai-lab-sglang-flashnext-flashnext-1", uuid="GPU-00000000-0000-0000-0000-000000000002")
    wire(runner, [worker_doc(), engine_doc(), other_card],
         ps=f"{WK}\n{FE}\n{ENG}\nai-lab-sglang-flashnext-flashnext-1\n")
    res = await make().run_action("preflight", {})
    assert res.ok and ENG in res.output and "flashnext" not in res.output
    assert "touches no other card" in res.output and "qwen3.8-27b-1m" in res.output
    assert not compose_calls(runner)                                        # safe: nothing was started/stopped


async def test_up_refused_while_engine_runs(make, runner) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [engine_doc()], ps=f"{ENG}\n")
    res = await make().run_action("up", {})
    assert not res.ok and "refused" in res.output and "stop_sharing_engine=true" in (res.problems[0].fix or "")
    assert not compose_calls(runner) and not any(a[:2] == ["docker", "stop"] for a in runner.argvs())


async def test_up_with_stop_sharing_engine_stops_exactly_that_container(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    eng = engine_doc()
    state = {"engine_down": False, "up": False}
    wk = worker_doc()

    def _inspect(argv: list[str]) -> CommandResult:
        docs = []
        if ENG in argv[2:]:
            docs.append(engine_doc(running=not state["engine_down"]))
        if WK in argv[2:] and state["up"]:
            docs.append(wk)
        return inspect_json(*docs)

    def _compose(argv: list[str]) -> CommandResult:
        if argv[-1] == "down":
            state["engine_down"] = True
        if "up" in argv:
            state["up"] = True
        return ok()

    runner.on(is_docker("inspect"), _inspect)
    runner.on(is_docker("ps"), ok(f"{ENG}\n"))
    runner.on(is_docker("compose"), _compose)
    http_live(mock)
    res = await ad.run_action("up", {"stop_sharing_engine": True})
    assert res.ok, res.output
    down = next(a for a in compose_calls(runner) if a[-1] == "down")
    assert down == ["docker", "compose", "-f", "/repo/inference/vllm-qwen38/docker-compose.yml", "down"]
    cwd = next(c for a, c in runner.calls if a == down)
    assert cwd == Path("/repo/inference/vllm-qwen38")
    assert not any(a[:2] == ["docker", "stop"] or "-v" in a or "rm" in a for a in runner.argvs())
    assert not any("use-model" in " ".join(a) for a in runner.argvs())
    order = [i for i, a in enumerate(runner.argvs()) if a[:2] == ["docker", "compose"]]
    assert runner.argvs()[order[0]][-1] == "down" and "up" in runner.argvs()[order[1]]     # stop first, then up
    assert eng["Name"]


async def test_stop_sharing_never_touches_other_or_unverified_containers(make, runner) -> None:  # type: ignore[no-untyped-def]
    comfy = engine_doc(name="comfyui-b-1", project="comfyui")
    wire(runner, [engine_doc(), comfy], ps=f"{ENG}\ncomfyui-b-1\n")
    res = await make().run_action("up", {"stop_sharing_engine": True})
    assert not res.ok and "comfyui-b-1" in res.output and not compose_calls(runner)


async def test_up_with_impostor_labels_is_refused(make, runner) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [engine_doc(project="evil")], ps=f"{ENG}\n")
    res = await make().run_action("up", {"stop_sharing_engine": True})
    assert not res.ok and res.problems[0].code == "dynamo.stop.unverified" and not any(
        a[-1] == "down" for a in compose_calls(runner))


async def test_up_refused_with_mocker_running(make, runner) -> None:  # type: ignore[no-untyped-def]
    m = copy.deepcopy(worker_doc())
    m["Name"] = "/ai-lab-dynamo-dynamo-mocker-1"
    wire(runner, [m])
    res = await make().run_action("up", {})
    assert not res.ok and "mocker" in res.output and not compose_calls(runner)


async def test_down_never_uses_volumes_flag_and_no_other_card(make, runner) -> None:  # type: ignore[no-untyped-def]
    runner.on(is_docker("compose"), ok("done"))
    res = await make().run_action("down", {})
    a = compose_calls(runner)[0]
    assert res.ok and a[-1] == "down" and "-v" not in a and "--volumes" not in a and "--profile" in a


async def test_up_mock_uses_profile_and_refuses_with_real_worker(make, runner) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [worker_doc()])
    res = await make().run_action("up_mock", {})
    assert not res.ok and not compose_calls(runner)
    runner.rules.clear()
    runner.calls.clear()
    wire(runner, [])
    runner.on(is_docker("compose"), ok())
    res2 = await make().run_action("up_mock", {})
    up = next(a for a in compose_calls(runner) if "up" in a)
    assert res2.ok and "--profile" in up and "mock" in up and up[-2:] == ["dynamo-frontend", "dynamo-mocker"]


async def test_restart_force_recreates_and_unknown_action_is_keyerror(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    import pytest
    ad = make()
    old = worker_doc()
    new = copy.deepcopy(old)
    new["State"]["StartedAt"] = "2026-09-24T09:31:00.000000000Z"
    st = {"up": False}
    runner.on(is_docker("inspect"), lambda a: inspect_json(new if st["up"] else old))
    runner.on(is_docker("compose"), lambda a: (st.__setitem__("up", True), ok())[1])
    http_live(mock)
    res = await ad.run_action("restart", {})
    assert res.ok and "--force-recreate" in compose_calls(runner)[0]
    with pytest.raises(KeyError):
        await ad.run_action("rm_rf", {})


# ── playground ──────────────────────────────────────────────────────────────────────────────────
async def test_playground(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    pg = ad.playground()
    assert pg is not None and pg.wire == "openai_chat" and pg.default_model == "unsloth/Qwen3.8-27B-NVFP4"
    wire(runner, [frontend_doc(), worker_doc()])
    http_live(mock)
    await ad.status()
    pg2 = ad.playground()
    assert pg2 is not None and pg2.models == ["unsloth/Qwen3.8-27B-NVFP4"]
    assert ad.playground_url() == f"{BASE_URL}/v1/chat/completions"


def test_json_fixtures_are_real_captures() -> None:
    assert json.loads(fx("frontend_health.json"))["status"] == "healthy"


async def test_sharers_ignores_container_that_stopped_after_ps(make, runner) -> None:  # type: ignore[no-untyped-def]
    wire(runner, [engine_doc(running=False)], ps=f"{ENG}\n")
    runner.on(is_docker("compose"), ok())
    res = await make().run_action("preflight", {})
    assert res.ok and "No other container is running" in res.output


async def test_preflight_machine_readable_tail(make, runner) -> None:  # type: ignore[no-untyped-def]
    # Dynamo holds the card, the sharing engine is stopped -> listed as stopped, aliases dark
    wire(runner, [worker_doc(), engine_doc(running=False)], ps=f"{WK}\n")
    out = (await make().run_action("preflight", {})).output.splitlines()
    assert out[-2:] == [f"shares_card: {ENG} (stopped)", "router_aliases_dark: qwen3.8-27b-1m, switchyard-capable-tier"]
    assert "No other container is running" in "\n".join(out)          # prose kept
    # engine running, Dynamo not the holder
    runner.rules.clear()
    wire(runner, [engine_doc()], ps=f"{ENG}\n")
    out = (await make().run_action("preflight", {})).output.splitlines()
    assert out[-2:] == [f"shares_card: {ENG} (running)", "router_aliases_dark: none"]
    # nothing at all
    runner.rules.clear()
    wire(runner, [worker_doc()], ps=f"{WK}\n")
    out = (await make().run_action("preflight", {})).output.splitlines()
    assert out[-2:] == ["shares_card: none", "router_aliases_dark: qwen3.8-27b-1m, switchyard-capable-tier"]
    runner.rules.clear()
    wire(runner, [], ps="")
    out = (await make().run_action("preflight", {})).output.splitlines()
    assert out[-2:] == ["shares_card: none", "router_aliases_dark: none"]


# ── hand-edited override: closed allowlist (audit payloads) ────────────────────────────────────
W = "python3, -m, dynamo.vllm"
MODEL = "/root/.cache/huggingface/hub/models--unsloth--Qwen3.8-27B-NVFP4/snapshots/16b6615af3548b88e2d8e382457bc705b00479cf"


def _errors(res: Any) -> str:
    return " | ".join(d.message for d in res.diagnostics if d.severity == "error")


async def test_override_rejects_single_dash_flag(make, runner) -> None:  # type: ignore[no-untyped-def]
    runner.on(is_docker("compose"), ok())
    doc = "services:\n  dynamo-worker:\n    command: [" + W + ", -cc, '{\"x\":1}']\n"
    r = await make().validate_config("override", doc)
    assert not r.ok and "-cc" in _errors(r)
    assert not compose_calls(runner)                        # refused before compose ever sees it


async def test_override_rejects_new_interpolation_in_argv(make, runner) -> None:  # type: ignore[no-untyped-def]
    runner.on(is_docker("compose"), ok())
    doc = f"services:\n  dynamo-worker:\n    command: [{W}, --model, '${{HF_TOKEN}}']\n"
    r = await make().validate_config("override", doc)
    assert not r.ok and "`$`" in _errors(r)


async def test_override_rejects_model_and_served_name_changes(make, runner) -> None:  # type: ignore[no-untyped-def]
    runner.on(is_docker("compose"), ok())
    r = await make().validate_config("override", f"services:\n  dynamo-worker:\n    command: [{W}, --served-model-name, /etc/passwd]\n")
    assert not r.ok and "--served-model-name must equal" in _errors(r)
    r2 = await make().validate_config("override", f"services:\n  dynamo-worker:\n    command: [{W}, --model, /etc/shadow]\n")
    assert not r2.ok and "--model must equal" in _errors(r2)
    same = f"services:\n  dynamo-worker:\n    command: [{W}, --model, {MODEL}, --served-model-name, unsloth/Qwen3.8-27B-NVFP4]\n"
    assert (await make().validate_config("override", same)).ok


async def test_override_env_interpolation_and_unknown_keys_rejected(make, runner) -> None:  # type: ignore[no-untyped-def]
    runner.on(is_docker("compose"), ok())
    doc = "services:\n  dynamo-frontend:\n    environment:\n      DYN_LOGGING_JSONL: \"${LITELLM_API_KEY}\"\n"
    r = await make().validate_config("override", doc)
    assert not r.ok and "`$`" in _errors(r)
    r2 = await make().validate_config("override", "services:\n  dynamo-frontend:\n    environment:\n      LD_PRELOAD: /x.so\n")
    assert not r2.ok and "not defined in the base compose" in _errors(r2)
    r3 = await make().validate_config("override", "services:\n  dynamo-worker:\n    environment:\n      DYN_DEFAULT_THINKING_MODE: enabled\n")
    assert not r3.ok                                        # key absent from the base worker env
    good = "services:\n  dynamo-frontend:\n    environment:\n      DYN_LOGGING_JSONL: 'true'\n"
    assert (await make().validate_config("override", good)).ok


async def test_override_fails_closed_without_base_compose(make, runner, compose) -> None:  # type: ignore[no-untyped-def]
    compose.unlink()
    r = await make().validate_config("override", f"services:\n  dynamo-worker:\n    command: [{W}, --model, x]\n")
    assert not r.ok


async def test_generated_override_still_passes_the_closed_allowlist(make, runner) -> None:  # type: ignore[no-untyped-def]
    runner.on(is_docker("compose"), ok())
    ad = make()
    text = await ad.preview_params({"max_model_len": 131072, "router_mode": "kv", "enable_prefix_caching": False,
                                    "reasoning_parser": "none", "num_speculative_tokens": 3})
    assert "${MAX_NUM_SEQS:-64}" in text                    # base interpolation carried verbatim is allowed
    r = await ad.validate_config("override", text)
    assert r.ok and r.authoritative, r.diagnostics


# ── second audit: duplicate flags, speculative-config, path/JSON values ─────────────────────────
SERVED = "unsloth/Qwen3.8-27B-NVFP4"


async def _verdict(make: Any, runner: FakeRunner, cmd: str) -> Any:
    runner.on(is_docker("compose"), ok())
    return await make().validate_config("override", f"services:\n  dynamo-worker:\n    command: [{cmd}]\n")


async def test_duplicate_model_flag_last_wins_is_rejected(make, runner) -> None:  # type: ignore[no-untyped-def]
    r = await _verdict(make, runner, f"{W}, --model, {MODEL}, --model, evil/repo")
    assert not r.ok and "more than once" in _errors(r)
    r2 = await _verdict(make, runner, f"{W}, --served-model-name, {SERVED}, --served-model-name, evil/x")
    assert not r2.ok and "more than once" in _errors(r2)


async def test_duplicate_detection_covers_equals_and_underscore_forms(make, runner) -> None:  # type: ignore[no-untyped-def]
    r = await _verdict(make, runner, f"{W}, --model, {MODEL}, --model=evil/repo")
    assert not r.ok and "more than once" in _errors(r)
    r2 = await _verdict(make, runner, f"{W}, --max-num-seqs, '4', --max_num_seqs, '8'")
    assert not r2.ok and "more than once" in _errors(r2)
    r3 = await _verdict(make, runner, f"{W}, --enable-prefix-caching, --no-enable-prefix-caching")
    assert not r3.ok and "more than once" in _errors(r3)
    r4 = await _verdict(make, runner, f"{W}, --model=evil/repo")             # `=` form must obey the model rule too
    assert not r4.ok and "--model must equal" in _errors(r4)


async def test_speculative_config_is_restricted_to_mtp(make, runner) -> None:  # type: ignore[no-untyped-def]
    evil = "'{\"method\":\"draft_model\",\"model\":\"evil/x\"}'"
    r = await _verdict(make, runner, f"{W}, --speculative-config, {evil}")
    assert not r.ok and "speculative-config" in _errors(r)
    for bad in ("'{\"method\":\"mtp\",\"num_speculative_tokens\":2,\"model\":\"evil/x\"}'",
                "'{\"method\":\"mtp\",\"num_speculative_tokens\":99}'", "'{\"method\":\"mtp\",\"num_speculative_tokens\":true}'",
                "'{\"method\":\"mtp\"}'", "'not json'", "'[1]'"):
        assert not (await _verdict(make, runner, f"{W}, --speculative-config, {bad}")).ok, bad
    good = "'{\"method\":\"mtp\",\"num_speculative_tokens\":3}'"
    assert (await _verdict(make, runner, f"{W}, --speculative-config, {good}")).ok


async def test_path_or_json_values_must_equal_base(make, runner) -> None:  # type: ignore[no-untyped-def]
    for cmd in (f"{W}, --dyn-tool-call-parser, /etc/passwd", f"{W}, --max-num-seqs, ../../x",
                f"{W}, --dyn-reasoning-parser, '{{\"a\":1}}'", f"{W}, --request-plane, /tmp/x",
                f"{W}, --discovery-backend=/tmp/x"):
        r = await _verdict(make, runner, cmd)
        assert not r.ok and "base compose" in _errors(r), cmd
    assert (await _verdict(make, runner, f"{W}, --request-plane, tcp, --max-num-seqs, '4'")).ok


async def test_stray_positional_tokens_rejected(make, runner) -> None:  # type: ignore[no-untyped-def]
    r = await _verdict(make, runner, f"{W}, stray")
    assert not r.ok
