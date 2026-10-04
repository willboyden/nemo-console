from __future__ import annotations

import pytest
from relay_helpers import FakeRunner, fx, ok, standard_runner

from nemo_console.adapters.base import CommandResult

RESTART = ["systemctl", "--user", "restart", "nemo-relay-gateway.service"]


def show(pid: int = 100, restarts: int = 0, active: str = "active", sub: str = "running") -> str:
    return (f"ActiveEnterTimestamp=@1790223677\nActiveState={active}\nSubState={sub}\nMainPID={pid}\nNRestarts={restarts}\n"
            "Result=success\nLoadState=loaded\nUnitFileState=enabled\n")


def test_actions_and_danger_levels(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    got = {x.id: x.danger for x in a.actions()}
    assert got == {"gateway_restart": "disruptive", "gateway_status": "safe", "doctor_offline": "safe",
                   "agents_list": "safe", "plugins_validate": "safe"}


async def test_post_apply_restarts_and_waits_for_settled_health(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    r = await a.post_apply("routes", restart=True)
    assert r.ok and r.restarted and r.post_check == "healthy"
    assert RESTART in run.calls
    assert sum(1 for c in run.calls if c[:4] == ["systemctl", "--user", "--timestamp=unix", "show"]) >= a.settle_checks


async def test_post_apply_without_restart_is_skipped(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    r = await a.post_apply("routes", restart=False)
    assert r.ok and not r.restarted and r.post_check == "skipped" and RESTART not in run.calls


async def test_post_apply_readonly_file_refused(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    r = await a.post_apply("plugins", restart=True)
    assert not r.ok and RESTART not in run.calls


async def test_post_apply_unhealthy_when_gateway_never_answers(make_adapter, router) -> None:  # type: ignore[no-untyped-def]
    import httpx

    router.get("http://127.0.0.1:4040/health").mock(side_effect=httpx.ConnectError("refused"))
    a, _ = make_adapter()
    a.restart_timeout = 0.05
    r = await a.post_apply("routes", restart=True)
    assert not r.ok and r.restarted and r.post_check == "unhealthy" and "not healthy" in r.detail


async def test_post_apply_detects_crash_loop_after_banner(make_adapter) -> None:  # type: ignore[no-untyped-def]
    n = {"i": 0}

    def flapping(argv: list[str]) -> CommandResult:
        n["i"] += 1
        return ok(show(pid=100 + n["i"], restarts=n["i"]))      # pid + NRestarts move every poll

    r = standard_runner()
    r.on(["systemctl", "--user", "--timestamp=unix", "show"], flapping)
    a, _ = make_adapter(r)
    a.restart_timeout = 0.05
    res = await a.post_apply("routes", restart=True)
    assert res.post_check == "unhealthy" and "crash loop" in res.detail


async def test_post_apply_restart_command_failure(make_adapter) -> None:  # type: ignore[no-untyped-def]
    r = standard_runner()
    r.on(RESTART, CommandResult(returncode=1, stdout="", stderr="Failed to restart"))
    a, _ = make_adapter(r)
    res = await a.post_apply("routes", restart=True)
    assert not res.ok and res.post_check == "unhealthy" and "Failed to restart" in res.detail


async def test_run_gateway_restart(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("gateway_restart", {})
    assert res.ok and RESTART in run.calls and res.problems == []


async def test_run_gateway_restart_unhealthy_reports_problem(make_adapter, router) -> None:  # type: ignore[no-untyped-def]
    import httpx

    router.get("http://127.0.0.1:4040/health").mock(side_effect=httpx.ConnectError("refused"))
    a, _ = make_adapter()
    a.restart_timeout = 0.05
    res = await a.run_action("gateway_restart", {})
    assert not res.ok and res.problems[0].code == "relay.gateway.restart_unhealthy" and res.problems[0].fix


async def test_safe_actions(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    st = await a.run_action("gateway_status", {})
    assert st.ok and "nemo-relay-gateway.service" in st.output
    ag = await a.run_action("agents_list", {})
    assert ag.ok and "claude: pass configured=True" in ag.output and "codex: info" in ag.output
    dr = await a.run_action("doctor_offline", {})
    assert dr.ok and "plugin host validation: pass" in dr.output and "nvidia.switchyard" in dr.output
    assert "CLI's own environment" in dr.output and "PASS while the gateway still fails" in dr.output
    pv = await a.run_action("plugins_validate", {})
    assert pv.ok and "nvidia.switchyard: valid=True" in pv.output and "attestation=signature_required" in pv.output
    assert "authoritative" in pv.output or "gateway still fails to start" in pv.output
    assert RESTART not in run.calls


async def test_gateway_status_inactive_rc3_is_still_an_answer(make_adapter) -> None:  # type: ignore[no-untyped-def]
    r = standard_runner()
    r.on(["systemctl", "--user", "status"], ok("Active: inactive (dead)\n", 3))
    a, _ = make_adapter(r)
    assert (await a.run_action("gateway_status", {})).ok


async def test_plugins_validate_failure_and_no_plugins(make_adapter) -> None:  # type: ignore[no-untyped-def]
    r = standard_runner()
    r.on(["nemo-relay", "plugins", "validate"], ok(fx("plugins_validate_notfound.json"), 2))
    a, _ = make_adapter(r)
    assert not (await a.run_action("plugins_validate", {})).ok
    a2, _ = make_adapter(FakeRunner())
    res = await a2.run_action("plugins_validate", {})
    assert not res.ok and "no registered plugins" in res.output


async def test_unknown_action_is_keyerror(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        await a.run_action("format_disk", {})


def test_playground(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    p = a.playground()
    assert p is not None and p.wire == "openai_chat"
    assert p.models == ["auto-local", "direct-lightning", "direct-27b"] and p.default_model == "auto-local"
    assert "401" in p.note and "env NAME" in p.note
    assert a.playground_url() == "http://127.0.0.1:4040/v1/chat/completions"


def test_playground_falls_back_when_routes_unreadable(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    a.config_path("routes").write_text("[[[ broken")
    p = a.playground()
    assert p is not None and p.models == ["auto-local", "direct-27b", "direct-lightning"]


async def test_plugin_ids_starting_with_dash_are_never_passed_and_dashdash_precedes_ids(make_adapter) -> None:  # type: ignore[no-untyped-def]
    import json

    rows = json.loads(fx("plugins_list.json"))
    rows["data"].append({**rows["data"][0], "id": "--help"})
    rows["data"].append({**rows["data"][0], "id": "-x"})
    r = standard_runner()
    r.on(["nemo-relay", "plugins", "list", "--json"], ok(json.dumps(rows)))
    a, run = make_adapter(r)
    await a.run_action("plugins_validate", {})
    validates = [c for c in run.calls if c[:3] == ["nemo-relay", "plugins", "validate"]]
    assert validates == [["nemo-relay", "plugins", "validate", "--json", "--", "nvidia.switchyard"]]
