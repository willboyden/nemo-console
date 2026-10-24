"""Offline tests for the guardrails adapter. Fixtures are captured from the live lab deployment (2026-10-24)."""
from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

import httpx
import pytest
from gr_helpers import BASE, CONTAINER, FakeRunner, fx, ok, standard_runner

from nemo_console.adapters.guardrails import PROBES

ID_RE = re.compile(r"[a-z0-9][a-z0-9_.\-]{0,63}")        # api/deps.py ID_RE
CFG = "lab-input-output--config.yml"


# ── identity / contract ─────────────────────────────────────────────────────────────────
def test_identity_and_action_dangers(make_adapter: Any) -> None:
    a, _ = make_adapter()
    assert (a.id, a.label) == ("guardrails", "NeMo Guardrails")
    dangers = {s.id: s.danger for s in a.actions()}
    assert dangers == {"up": "disruptive", "restart": "disruptive", "stop": "disruptive", "check": "disruptive"}
    assert a.playground_url() == f"{BASE}/v1/chat/completions"


# ── status ──────────────────────────────────────────────────────────────────────────────
async def test_status_up(make_adapter: Any) -> None:
    a, runner = make_adapter()
    st = await a.status()
    assert st.system == "guardrails" and st.health == "up" and st.version == "0.24.1"
    assert st.summary == "1 rails config(s) served"
    assert st.problems == []
    names = {c.name: c.state for c in st.components}
    assert names[CONTAINER] == "running" and names[f"{CONTAINER}-gw"] == "running"
    assert names[f"{CONTAINER}-router-broker"] == "running"
    assert runner.called(["docker", "network", "inspect", "--format", "{{.Internal}}", "ai-lab-guardrails"])


async def test_status_off_when_container_absent(make_adapter: Any, server: Any) -> None:
    server.get(f"{BASE}/v1/health").mock(side_effect=httpx.ConnectError("refused"))
    r = FakeRunner().on(["docker", "inspect"], ok(rc=1, stderr="Error: No such object"))
    a, _ = make_adapter(r)
    st = await a.status()
    assert st.health == "off" and st.version is None
    assert [p.code for p in st.problems] == ["guardrails.off"]
    assert "docker compose -f" in (st.problems[0].fix or "") and "up -d --no-build" in (st.problems[0].fix or "")


async def test_status_down_when_running_but_silent(make_adapter: Any, server: Any) -> None:
    server.get(f"{BASE}/v1/health").mock(side_effect=httpx.ConnectError("refused"))
    a, _ = make_adapter()
    st = await a.status()
    assert st.health == "down"
    assert "guardrails.down" in [p.code for p in st.problems]


async def test_status_flags_open_egress_and_published_port(make_adapter: Any) -> None:
    r = standard_runner(isolation='ai-lab-guardrails ai-lab |{"8000/tcp":[{"HostIp":"0.0.0.0","HostPort":"8000"}]}',
                        internal="false\n")
    a, _ = make_adapter(r)
    st = await a.status()
    codes = [p.code for p in st.problems]
    assert st.health == "degraded"
    assert codes.count("guardrails.egress.open") == 2 and "guardrails.port.published" in codes
    egress = next(p for p in st.problems if p.code == "guardrails.egress.open")
    assert egress.fix and egress.fix.startswith("docker network disconnect ")


async def test_status_wrapper_missing_is_a_warning_with_fix(make_adapter: Any) -> None:
    r = standard_runner()
    r.on(["guardrails-local", "--version"], ok(rc=127, stderr="not found"))
    a, _ = make_adapter(r)
    st = await a.status()
    assert st.health == "degraded" and st.version is None
    p = next(p for p in st.problems if p.code == "guardrails.wrapper.missing")
    assert p.fix and p.fix.endswith("   # builds the pinned image and links guardrails-local onto PATH")
    assert "install.sh" in p.fix


async def test_status_config_on_disk_not_served(make_adapter: Any, server: Any) -> None:
    server.get(f"{BASE}/v1/rails/configs").respond(200, json=[])
    a, _ = make_adapter()
    st = await a.status()
    codes = [p.code for p in st.problems]
    assert "guardrails.configs.none" in codes and "guardrails.configs.not_served" in codes


async def test_version_is_cached(make_adapter: Any) -> None:
    a, runner = make_adapter()
    await a.status()
    await a.status()
    assert len(runner.called(["guardrails-local", "--version"])) == 1


# ── metrics ─────────────────────────────────────────────────────────────────────────────
async def test_metrics_only_what_is_readable(make_adapter: Any) -> None:
    a, _ = make_adapter()
    m = await a.metrics()
    assert m.available and {p.key: p.value for p in m.points} == {"gr.up": 1.0, "gr.configs": 1.0}


async def test_metrics_unavailable_when_down(make_adapter: Any, server: Any) -> None:
    server.get(f"{BASE}/v1/health").mock(side_effect=httpx.ConnectError("refused"))
    a, _ = make_adapter()
    m = await a.metrics()
    assert not m.available and m.points == [] and m.error


# ── config files ────────────────────────────────────────────────────────────────────────
def test_config_ids_match_the_core_id_rules(make_adapter: Any) -> None:
    a, _ = make_adapter()
    specs = {s.id: s for s in a.config_files()}
    assert set(specs) == {CFG, "lab-input-output--prompts.yml", "lab-input-output--actions.py",
                          "lab-input-output--kb--faq.md"}
    assert all(ID_RE.fullmatch(i) for i in specs)
    assert specs[CFG].editable and specs[CFG].format == "yaml" and specs[CFG].validators
    assert specs["lab-input-output--kb--faq.md"].format == "text"
    py = specs["lab-input-output--actions.py"]
    assert not py.editable and py.why_readonly and "code" in py.why_readonly
    assert specs[CFG].display_path == "security/nemo-guardrails/config/lab-input-output/config.yml"


def test_symlinks_uppercase_hidden_and_foreign_dirs_are_not_offered(make_adapter: Any) -> None:
    a, _ = make_adapter()
    ids = {s.id for s in a.config_files()}
    assert not any("linked" in i or "notes" in i.lower() or "hidden" in i or "no-config" in i for i in ids)
    for bad in ("lab-input-output--linked.yml", "lab-input-output--Notes.md", "../x", "nope"):
        with pytest.raises(KeyError):
            a.config_path(bad)


async def test_read_config(make_adapter: Any) -> None:
    a, _ = make_adapter()
    cf = await a.read_config(CFG)
    assert cf.content == fx("config-tree/lab-input-output/config.yml")
    assert len(cf.sha256) == 64 and cf.spec.id == CFG
    with pytest.raises(KeyError):
        await a.read_config("nope--config.yml")


# ── validation ──────────────────────────────────────────────────────────────────────────
async def test_validate_good_candidate_loads_a_temp_copy(make_adapter: Any, tmp_path: Path) -> None:
    seen: dict[str, Any] = {}

    def respond(argv: list[str]) -> Any:
        d = Path(argv[2])
        seen["argv"] = argv
        seen["config"] = (d / "config.yml").read_text()
        seen["prompts"] = (d / "prompts.yml").exists()
        seen["name"] = d.name
        seen["mode"] = oct(d.parent.stat().st_mode & 0o777)
        return ok(fx("validate_ok.stdout"))

    r = standard_runner().on(["guardrails-local", "validate"], respond)
    a, _ = make_adapter(r)
    live = a.config_path(CFG)
    before = live.read_text()
    cand = before.replace("temperature: 0.2", "temperature: 0.7")
    res = await a.validate_config(CFG, cand)
    assert res.ok and res.authoritative and res.validator.startswith("guardrails-local validate")
    assert res.note and "ok: loaded" in res.note
    assert seen["argv"][:2] == ["guardrails-local", "validate"]
    assert seen["name"] == "lab-input-output" and seen["config"] == cand and seen["prompts"]
    assert seen["mode"] == "0o700"
    assert live.read_text() == before                               # the live file is never touched
    assert not list((tmp_path / "state" / "guardrails").glob("validate-*"))   # temp copy cleaned up


async def test_validate_broken_yaml_reports_line(make_adapter: Any) -> None:
    r = standard_runner().on(["guardrails-local", "validate"], ok(rc=1, stderr=fx("validate_yaml.stderr")))
    a, _ = make_adapter(r)
    res = await a.validate_config(CFG, "models: [\n  - type: main")
    assert not res.ok and res.authoritative
    errs = [d for d in res.diagnostics if d.severity == "error"]
    assert errs and errs[0].line is not None and "YAML syntax" in errs[0].message
    assert len(errs) == 1                                           # the container's ParserError is the same finding


async def test_validate_unknown_flow_is_an_authoritative_error(make_adapter: Any) -> None:
    r = standard_runner().on(["guardrails-local", "validate"], ok(rc=1, stderr=fx("validate_flow.stderr")))
    a, _ = make_adapter(r)
    cand = "# nemo-console: allow-weaken-rails\n" + fx("config-tree/lab-input-output/config.yml").replace(
        "self check output", "self check nonsense")
    res = await a.validate_config(CFG, cand)
    assert not res.ok and res.authoritative
    assert "self check nonsense" in res.diagnostics[0].message and "does not exist" in res.diagnostics[0].message
    assert "  at llmrails.py" not in res.diagnostics[0].message


async def test_validate_missing_prompt(make_adapter: Any) -> None:
    r = standard_runner().on(["guardrails-local", "validate"], ok(rc=1, stderr=fx("validate_prompt.stderr")))
    a, _ = make_adapter(r)
    res = await a.validate_config("lab-input-output--prompts.yml", "prompts: []\n")
    assert not res.ok and "self_check_input" in res.diagnostics[0].message


async def test_validate_without_wrapper_is_not_authoritative(make_adapter: Any) -> None:
    r = standard_runner().on(["guardrails-local", "validate"], ok(rc=127, stderr="no such file"))
    a, _ = make_adapter(r)
    good = await a.validate_config(CFG, fx("config-tree/lab-input-output/config.yml").replace("0.2", "0.3"))
    assert good.ok and not good.authoritative and "install.sh" in (good.note or "")
    bad = await a.validate_config(CFG, "a: [\n")
    assert not bad.ok and not bad.authoritative


async def test_validate_refuses_python_and_oversize(make_adapter: Any) -> None:
    a, runner = make_adapter()
    py = await a.validate_config("lab-input-output--actions.py", "print(1)")
    assert not py.ok
    big = await a.validate_config(CFG, "a: " + "x" * 300_000)
    assert not big.ok and "exceeds" in big.diagnostics[0].message
    assert not runner.called(["guardrails-local", "validate"])


# ── apply ───────────────────────────────────────────────────────────────────────────────
async def test_post_apply_without_restart_is_skipped(make_adapter: Any) -> None:
    a, runner = make_adapter()
    r = await a.post_apply(CFG, restart=False)
    assert r.ok and r.post_check == "skipped" and not r.restarted
    assert not runner.called(["docker", "restart"])


async def test_post_apply_restarts_and_reloads(make_adapter: Any) -> None:
    r = standard_runner().on(["docker", "restart"], ok(CONTAINER))
    a, _ = make_adapter(r)
    res = await a.post_apply(CFG, restart=True)
    assert res.ok and res.restarted and res.post_check == "healthy" and "lab-input-output" in res.detail
    assert r.called(["docker", "restart"]) == [["docker", "restart", "--time", "10", CONTAINER]]
    live = a.config_path(CFG).parent
    assert r.called(["guardrails-local", "validate"]) == [["guardrails-local", "validate", str(live)]]


async def test_post_apply_unhealthy_when_config_not_served(make_adapter: Any, server: Any) -> None:
    server.get(f"{BASE}/v1/rails/configs").respond(200, json=[])
    r = standard_runner().on(["docker", "restart"], ok(CONTAINER))
    a, _ = make_adapter(r)
    res = await a.post_apply(CFG, restart=True)
    assert not res.ok and res.restarted and res.post_check == "unhealthy"
    assert "not in the served list" in res.detail


async def test_post_apply_unhealthy_when_server_never_comes_back(make_adapter: Any, server: Any) -> None:
    server.get(f"{BASE}/v1/rails/configs").mock(side_effect=httpx.ConnectError("refused"))
    r = standard_runner().on(["docker", "restart"], ok(CONTAINER))
    a, _ = make_adapter(r)
    res = await a.post_apply(CFG, restart=True)
    assert not res.ok and res.post_check == "unhealthy"


async def test_post_apply_unhealthy_when_the_live_config_does_not_load(make_adapter: Any) -> None:
    r = (standard_runner().on(["docker", "restart"], ok(CONTAINER))
         .on(["guardrails-local", "validate"], ok(rc=1, stderr=fx("validate_flow.stderr"))))
    a, _ = make_adapter(r)
    res = await a.post_apply(CFG, restart=True)
    assert not res.ok and res.post_check == "unhealthy" and "does not load" in res.detail


async def test_post_apply_restart_failure(make_adapter: Any) -> None:
    r = standard_runner().on(["docker", "restart"], ok(rc=1, stderr="Error response from daemon"))
    a, _ = make_adapter(r)
    res = await a.post_apply(CFG, restart=True)
    assert not res.ok and not res.restarted and res.post_check == "unhealthy"


# ── actions ─────────────────────────────────────────────────────────────────────────────
async def test_up_restart_stop_argv(make_adapter: Any, tmp_path: Path) -> None:
    r = standard_runner()
    r.on(["docker", "compose"], ok("done"))
    r.on(["docker", "restart"], ok(CONTAINER))
    a, _ = make_adapter(r)
    compose = str(tmp_path / "repo" / "security" / "nemo-guardrails" / "docker-compose.yml")
    up = await a.run_action("up", {})
    assert up.ok and "served" in up.output
    await a.run_action("stop", {})
    rs = await a.run_action("restart", {})
    assert rs.ok
    assert r.called(["docker", "compose"]) == [
        ["docker", "compose", "-f", compose, "up", "-d", "--no-build"],
        ["docker", "compose", "-f", compose, "stop"]]
    assert r.called(["docker", "restart"]) == [["docker", "restart", "--time", "10", CONTAINER]]
    flat = [t for c in r.calls for t in c]
    assert "down" not in flat and "-v" not in flat and "--volumes" not in flat      # never down -v
    assert r.cwds[[c[:3] for c in r.calls].index(["docker", "compose", "-f"])] == Path(compose).parent


async def test_up_failure_points_at_install(make_adapter: Any) -> None:
    r = standard_runner().on(["docker", "compose"], ok(rc=1, stderr="No such image: ai-lab-nemo-guardrails:0.24.1"))
    a, _ = make_adapter(r)
    res = await a.run_action("up", {})
    assert not res.ok and res.problems[0].code == "guardrails.up.failed" and "install.sh" in (res.problems[0].fix or "")


async def test_unknown_action(make_adapter: Any) -> None:
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        await a.run_action("down", {})


def _checks_responder(server: Any, verdicts: dict[str, str]) -> list[dict[str, Any]]:
    bodies: list[dict[str, Any]] = []

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        bodies.append(body)
        text = body["messages"][0]["content"]
        name = next(n for n, p, _ in PROBES if p == text)
        status = verdicts[name]
        return httpx.Response(200, text=fx("checks-passed.json" if status == "passed" else "checks-blocked.json")
                              .replace('"passed"', f'"{status}"').replace('"blocked"', f'"{status}"'))

    server.post(f"{BASE}/v1/checks").mock(side_effect=handler)
    return bodies


async def test_check_all_as_expected(make_adapter: Any, tmp_path: Path, server: Any) -> None:
    bodies = _checks_responder(server, {"benign": "passed", "jailbreak": "blocked", "harmful": "blocked"})
    a, _ = make_adapter()
    res = await a.run_action("check", {})
    assert res.ok and res.problems == []
    assert len(bodies) == 3
    assert bodies[0] == {"model": "aitop/nemotron-lightning",
                         "messages": [{"role": "user", "content": "What is the capital of France?"}],
                         "guardrails": {"config_id": "lab-input-output"}}
    assert "benign     allowed" in res.output and "jailbreak  blocked" in res.output
    ev = (await a.recent_events())[0]
    assert ev.kind == "guardrails.check" and ev.system == "guardrails" and ev.data["ok"] is True
    assert "What is the capital" not in json.dumps(ev.model_dump())          # metadata only, never prompt text
    mode = (tmp_path / "state" / "guardrails" / "checks.jsonl").stat().st_mode & 0o777
    assert mode == 0o600


async def test_check_reports_a_jailbreak_that_got_through(make_adapter: Any, server: Any) -> None:
    _checks_responder(server, {"benign": "passed", "jailbreak": "passed", "harmful": "blocked"})
    a, _ = make_adapter()
    res = await a.run_action("check", {})
    assert not res.ok
    assert [p.code for p in res.problems] == ["guardrails.check.unexpected"]
    assert "jailbreak" in res.problems[0].message and "prompts.yml" in (res.problems[0].fix or "")


async def test_check_stops_on_server_error(make_adapter: Any, server: Any) -> None:
    server.post(f"{BASE}/v1/checks").respond(400, text=fx("chat-badconfig.json"))
    a, _ = make_adapter()
    res = await a.run_action("check", {"config": "lab-input-output"})
    assert not res.ok and res.problems[0].code == "guardrails.check.failed"
    assert len([c for c in server.calls if c.request.method == "POST"]) == 1


async def test_check_param_choice_is_validated_against_disk(make_adapter: Any, server: Any) -> None:
    bodies = _checks_responder(server, {"benign": "passed", "jailbreak": "blocked", "harmful": "blocked"})
    a, _ = make_adapter()
    spec = next(s for s in a.actions() if s.id == "check")
    assert spec.params[0].choices == ["lab-input-output"] and spec.params[0].default == "lab-input-output"
    await a.run_action("check", {"config": "../../etc"})              # unknown -> falls back to the default config
    assert bodies[0]["guardrails"] == {"config_id": "lab-input-output"}


# ── events ──────────────────────────────────────────────────────────────────────────────
async def test_events_empty_then_followed(make_adapter: Any, server: Any) -> None:
    a, _ = make_adapter()
    assert await a.recent_events() == []
    _checks_responder(server, {"benign": "passed", "jailbreak": "blocked", "harmful": "blocked"})
    it = a.follow_events(since_ts=0.0)
    await a.run_action("check", {})
    ev = await it.__anext__()
    assert ev.kind == "guardrails.check"
    await it.aclose()                                                   # type: ignore[attr-defined]


# ── playground ──────────────────────────────────────────────────────────────────────────
def test_playground_spec(make_adapter: Any) -> None:
    a, _ = make_adapter()
    pg = a.playground()
    assert pg is not None and pg.wire == "openai_chat"
    assert pg.models == ["aitop/nemotron-lightning"] and pg.default_model == "aitop/nemotron-lightning"
    assert "lab-input-output" in pg.note


def test_playground_body_shape_matches_the_captured_response(make_adapter: Any) -> None:
    """The core posts {model, messages, stream, max_tokens}; the real server answered such a body (fixtures)."""
    body = json.loads(fx("chat-benign.json"))
    assert body["choices"][0]["message"]["content"] == "Paris" and body["model"] == "aitop/nemotron-lightning"
    blocked = json.loads(fx("chat-jailbreak.json"))
    assert blocked["choices"][0]["message"]["content"] == "I'm sorry, I can't respond to that."


# ── M3: weakening the rails must be deliberate ──────────────────────────────────────────
MARKER = "# nemo-console: allow-weaken-rails"


async def test_removing_an_output_flow_is_refused(make_adapter: Any) -> None:
    a, _ = make_adapter()
    cand = fx("config-tree/lab-input-output/config.yml").replace("      - self check output\n", "")
    res = await a.validate_config(CFG, cand)
    assert not res.ok
    msg = next(d.message for d in res.diagnostics if d.severity == "error")
    assert "output: self check output" in msg and MARKER in msg


async def test_removing_the_whole_rails_section_is_refused(make_adapter: Any) -> None:
    a, _ = make_adapter()
    live = fx("config-tree/lab-input-output/config.yml")
    cand = live[: live.index("rails:")]
    res = await a.validate_config(CFG, cand)
    assert not res.ok
    msg = next(d.message for d in res.diagnostics if d.severity == "error")
    assert "input: self check input" in msg and "output: self check output" in msg


async def test_the_marker_line_allows_the_weakening(make_adapter: Any) -> None:
    a, _ = make_adapter()
    cand = MARKER + "\n" + fx("config-tree/lab-input-output/config.yml").replace("      - self check output\n", "")
    res = await a.validate_config(CFG, cand)
    assert res.ok and res.authoritative
    sloppy = fx("config-tree/lab-input-output/config.yml").replace("      - self check output\n", "") + "# not " + MARKER + " really\n"
    assert not (await a.validate_config(CFG, sloppy)).ok          # only the exact line counts


async def test_edits_that_keep_or_add_rails_need_no_marker(make_adapter: Any) -> None:
    a, _ = make_adapter()
    live = fx("config-tree/lab-input-output/config.yml")
    assert (await a.validate_config(CFG, live.replace("temperature: 0.2", "temperature: 0.9"))).ok
    assert (await a.validate_config(CFG, live.replace("      - self check output\n",
                                                      "      - self check output\n      - extra rail\n"))).ok


async def test_weakening_is_refused_even_without_the_wrapper(make_adapter: Any) -> None:
    r = standard_runner().on(["guardrails-local", "validate"], ok(rc=127, stderr="no such file"))
    a, _ = make_adapter(r)
    cand = fx("config-tree/lab-input-output/config.yml").replace("      - self check input\n", "")
    res = await a.validate_config(CFG, cand)
    assert not res.ok and not res.authoritative


def test_spec_description_names_the_marker(make_adapter: Any) -> None:
    a, _ = make_adapter()
    specs = {s.id: s for s in a.config_files()}
    assert MARKER in specs[CFG].description and MARKER not in specs["lab-input-output--prompts.yml"].description


# ── L1: the validation copy never follows symlinks and is capped ────────────────────────
async def test_validation_copy_skips_symlinks_and_caps(make_adapter: Any) -> None:
    seen: dict[str, Any] = {}

    def respond(argv: list[str]) -> Any:
        d = Path(argv[2])
        seen["files"] = sorted(str(p.relative_to(d)) for p in d.rglob("*") if p.is_file())
        seen["links"] = [p.name for p in d.rglob("*") if p.is_symlink()]
        return ok(fx("validate_ok.stdout"))

    a, _ = make_adapter(standard_runner().on(["guardrails-local", "validate"], respond))
    res = await a.validate_config(CFG, fx("config-tree/lab-input-output/config.yml"))
    assert res.ok and seen["links"] == [] and "linked.yml" not in seen["files"]
    assert {"config.yml", "prompts.yml", "kb/faq.md"} <= set(seen["files"])
    big = a.config_path(CFG).parent / "big.txt"
    big.write_bytes(b"x" * (3 * 1024 * 1024))
    capped = await a.validate_config(CFG, fx("config-tree/lab-input-output/config.yml"))
    assert not capped.ok and "exceeds" in capped.diagnostics[0].message


async def test_isolation_is_checked_right_after_boot(make_adapter: Any, monkeypatch: pytest.MonkeyPatch) -> None:
    """CI 2026-10-24: time.monotonic() counts from boot, so a cache stamped 0.0 looked fresh on a runner booted under
    60 s ago and the isolation check was silently skipped. A never-filled cache must be stale at any clock value."""
    import nemo_console.adapters.guardrails as mod
    monkeypatch.setattr(mod.time, "monotonic", lambda: 5.0)
    a, runner = make_adapter()
    await a.status()
    assert runner.called(["docker", "network", "inspect", "--format", "{{.Internal}}", "ai-lab-guardrails"])
