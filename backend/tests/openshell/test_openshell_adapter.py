"""OpenShell adapter: status, metrics, events, read-only trust files, actions. Offline: fakes answering with the
captured fixtures (see os_helpers.py); nothing starts a CLI, docker or a gateway."""
from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest
import yaml
from os_helpers import GW, LIVE_SANDBOX_LIST, FakeRunner, World, fx, make_runner, ok

from nemo_console.adapters import openshell as osh
from nemo_console.adapters.openshell import OpenShellAdapter, phase_bucket, strip_ansi
from nemo_console.services.runner import ALLOWED_BINARIES, SubprocessRunner


def codes(st) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in st.problems}


def point(snap, key: str, **labels: str) -> float | None:  # type: ignore[no-untyped-def]
    for p in snap.points:
        if p.key == key and p.labels == labels:
            return p.value
    return None


# ── runner policy / pure helpers ────────────────────────────────────────────────────────────────
def test_binaries_are_allowlisted() -> None:
    assert {"openshell", "docker"} <= ALLOWED_BINARIES
    with pytest.raises(Exception, match="not allowed"):
        SubprocessRunner([]).resolve("openshell-gateway")              # only the bare CLI is allowlisted
    try:
        SubprocessRunner([]).resolve("openshell")
    except Exception as e:  # noqa: BLE001 - allowlisted but not installed on this machine is fine
        assert "not allowed" not in str(e)


def test_strip_ansi_handles_the_real_status_output() -> None:
    raw = fx("status-connected.raw.txt")
    assert "\x1b" in raw
    clean = strip_ansi(raw)
    assert "\x1b" not in clean and "Status: Connected" in clean and "Version: 0.0.77" in clean


def test_phase_buckets_are_bounded() -> None:
    assert phase_bucket("Ready") == "Ready" and phase_bucket("provisioning") == "Provisioning"
    assert phase_bucket("SomethingNew") == "Other" and phase_bucket("") == "Other"


# ── status ──────────────────────────────────────────────────────────────────────────────────────
async def test_status_up_matches_the_live_shape(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter()
    st = await a.status()
    assert st.system == "openshell" and st.health == "up" and st.version == "0.0.77"
    assert "connected" in st.summary and "0 sandbox(es)" in st.summary and "CLI 0.0.71" in st.summary
    assert [c.name for c in st.components][:3] == ["openshell CLI", "ai-lab-dd-gateway", "ai-lab-dd-cli-fwd"]
    assert all(c.state == "running" for c in st.components)
    assert st.components[1].image == "ghcr.io/nvidia/openshell/gateway@sha256:ba6f6537544d"
    assert st.endpoints[0].url == "https://127.0.0.1:17670"
    assert codes(st) == {"openshell.version.skew"}                      # CLI 0.0.71 vs gateway 0.0.77, info only
    assert [p.severity for p in st.problems] == ["info"]
    # golden argv: pinned to the gateway registration, no shell, docker via the allowlisted binary
    assert ["openshell", "--version"] in run.calls
    assert [*GW, "status"] in run.calls
    assert ["docker", "network", "inspect", "openshell-docker", "--format", "{{.Internal}}"] in run.calls
    assert any(c[:3] == ["docker", "inspect", "--format"] and c[-2:] == ["ai-lab-dd-gateway", "ai-lab-dd-cli-fwd"]
               for c in run.calls)
    assert [*GW, "sandbox", "list", "--limit", "200", "-o", "json"] in run.calls


async def test_status_text_never_carries_ansi(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    st = await a.status()
    assert "\x1b" not in st.model_dump_json()


async def test_stopped_on_purpose_is_off_not_down(make_adapter) -> None:  # type: ignore[no-untyped-def]
    w = World(inspect_out=fx("docker-inspect-stopped.HAND-WRITTEN.txt"))
    a, run, _ = make_adapter(w)
    st = await a.status()
    assert st.health == "off"
    p = next(p for p in st.problems if p.code == "openshell.gateway.off")
    assert p.severity == "info" and "docker compose -f" in (p.fix or "") and "up -d" in (p.fix or "")
    assert "data-designer-local" in (p.fix or "") and "data-designer-local" in p.message
    assert run.count([*GW, "status"]) == 0                              # no CLI connect attempt to a stopped gateway
    assert run.count([*GW, "sandbox"]) == 0
    assert [c.state for c in st.components if c.kind == "container"] == ["stopped", "stopped"]


async def test_container_never_created_is_off(make_adapter) -> None:  # type: ignore[no-untyped-def]
    w = World(inspect_out="", inspect_rc=1, inspect_err="error: no such object: ai-lab-dd-gateway")
    a, _, _ = make_adapter(w)
    st = await a.status()
    assert st.health == "off" and [c.state for c in st.components if c.kind == "container"] == ["absent", "absent"]


async def test_crashed_gateway_is_down(make_adapter) -> None:  # type: ignore[no-untyped-def]
    out = fx("docker-inspect-stopped.HAND-WRITTEN.txt").replace("|exited|137|", "|exited|1|", 1)
    a, _, _ = make_adapter(World(inspect_out=out))
    st = await a.status()
    assert st.health == "down" and "openshell.gateway.failed" in codes(st)
    assert next(p for p in st.problems if p.code == "openshell.gateway.failed").fix.startswith("docker logs")  # type: ignore[union-attr]


async def test_docker_down_degrades_to_the_cli_view(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(inspect_out="", inspect_rc=1, inspect_err="Cannot connect to the Docker daemon"))
    st = await a.status()
    assert "openshell.docker.unavailable" in codes(st)
    assert st.health in {"up", "degraded", "unknown"} and st.health != "off"       # never claims "stopped on purpose"
    assert all(c.state == "unknown" for c in st.components if c.kind == "container")


async def test_cli_absent_is_down_with_a_fix(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(cli_rc=127, cli_version=""))
    st = await a.status()
    assert st.health == "down"
    p = next(p for p in st.problems if p.code == "openshell.cli.missing")
    assert p.severity == "error" and "0.0.71" in (p.fix or "")
    assert st.components[0].state == "absent"


async def test_non_internal_sandbox_network_is_an_error(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(internal="false"))
    st = await a.status()
    p = next(p for p in st.problems if p.code == "openshell.network.not_internal")
    assert p.severity == "error" and "--internal" in (p.fix or "") and "down -v" not in (p.fix or "")
    assert "172.16.0.0/24" in (p.fix or "")
    assert st.health == "degraded"


async def test_non_internal_network_is_flagged_even_when_gateway_is_off(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(internal="false", inspect_out=fx("docker-inspect-stopped.HAND-WRITTEN.txt")))
    st = await a.status()
    assert st.health == "off" and {"openshell.network.not_internal", "openshell.gateway.off"} <= codes(st)


async def test_missing_network_is_a_warning(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(internal="missing", inspect_out=fx("docker-inspect-stopped.HAND-WRITTEN.txt")))
    st = await a.status()
    p = next(p for p in st.problems if p.code == "openshell.network.missing")
    assert p.severity == "warn" and p.fix == "docker network create --internal --subnet 172.16.0.0/24 openshell-docker"


async def test_published_port_beyond_loopback_is_an_error(make_adapter) -> None:  # type: ignore[no-untyped-def]
    out = fx("docker-inspect-running.txt").replace('"HostIp":"127.0.0.1"', '"HostIp":"0.0.0.0"')
    a, _, _ = make_adapter(World(inspect_out=out))
    st = await a.status()
    p = next(p for p in st.problems if p.code == "openshell.port.exposed")
    assert p.severity == "error" and "0.0.0.0" in p.message and "127.0.0.1" in (p.fix or "")
    assert st.health == "degraded"


async def test_forwarder_down_degrades(make_adapter) -> None:  # type: ignore[no-untyped-def]
    lines = fx("docker-inspect-running.txt").splitlines()
    lines[1] = lines[1].replace("|running|0|", "|exited|0|")
    a, _, _ = make_adapter(World(inspect_out="\n".join(lines) + "\n"))
    st = await a.status()
    assert st.health == "degraded" and "openshell.forwarder.stopped" in codes(st)


async def test_unregistered_gateway(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(status_out=fx("status-unregistered.raw.txt")))
    st = await a.status()
    assert st.health == "degraded"                                       # `status` exits 0 here: only the line decides
    p = next(p for p in st.problems if p.code == "openshell.gateway.unregistered")
    assert p.fix == "openshell gateway add https://127.0.0.1:17670 --local --name openshell"


async def test_disconnected_gateway(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(status_out=fx("status-disconnected.HAND-WRITTEN.raw.txt")))
    st = await a.status()
    assert st.health == "degraded" and "openshell.gateway.disconnected" in codes(st)
    assert "0 sandbox" not in st.summary


async def test_no_version_skew_when_pair_matches(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(cli_version="openshell 0.0.77\n"))
    assert "openshell.version.skew" not in codes(await a.status())


async def test_version_probe_is_cached(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter()
    await a.status()
    await a.status()
    assert run.count(["openshell", "--version"]) == 1


async def test_status_flags_unpinned_gateway_config(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    assert not [p for p in (await a.status()).problems if p.code.startswith("openshell.config")]
    cfg = a.ctx.paths["openshell_gateway_config"]
    cfg.write_text(cfg.read_text().replace('image_pull_policy = "Never"', 'image_pull_policy = "Always"'))
    assert "openshell.config.pull_policy" in codes(await a.status())
    cfg.write_text("[openshell\n")
    assert "openshell.config.unreadable" in codes(await a.status())


async def test_status_survives_every_probe_failing(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    a.ctx.runner.rules.clear()                                             # type: ignore[attr-defined]  # all rc 127
    st = await a.status()
    assert st.health == "down" and "openshell.cli.missing" in codes(st)


# ── metrics ─────────────────────────────────────────────────────────────────────────────────────
async def test_metrics_empty_gateway(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    m = await a.metrics()
    assert m.available
    assert point(m, "os.gateway_up") == 1.0 and point(m, "os.sandboxes") == 0.0
    assert point(m, "os.network_internal") == 1.0 and point(m, "os.providers") == 1.0
    assert point(m, "os.gateway_restarts") == 0.0 and (point(m, "os.gateway_uptime_s") or 0) > 0
    assert {p.labels["phase"] for p in m.points if p.key == "os.sandboxes_by_phase"} == set(osh.PHASES)
    assert point(m, "os.sandbox_oldest_age_s") is None                   # nothing to age: omitted, not 0


async def test_metrics_counts_phases_and_buckets_unknown(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(sandboxes=fx("sandbox-list-phases.HAND-WRITTEN.json")))
    m = await a.metrics()
    assert point(m, "os.sandboxes") == 4.0
    for ph, n in {"Ready": 1, "Provisioning": 1, "Error": 1, "Deleting": 0, "Other": 1}.items():
        assert point(m, "os.sandboxes_by_phase", phase=ph) == float(n)
    assert (point(m, "os.sandbox_oldest_age_s") or 0) > 0


async def test_metrics_gateway_off_reads_zero_and_omits_the_rest(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter(World(inspect_out=fx("docker-inspect-stopped.HAND-WRITTEN.txt")))
    m = await a.metrics()
    assert m.available and point(m, "os.gateway_up") == 0.0
    assert point(m, "os.sandboxes") is None and point(m, "os.providers") is None and point(m, "os.gateway_uptime_s") is None
    assert run.count([*GW, "status"]) == 0


async def test_metrics_unavailable_without_the_cli(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(cli_rc=127, cli_version=""))
    m = await a.metrics()
    assert not m.available and m.points == [] and m.error


async def test_metrics_tolerates_a_failed_sandbox_list(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(sandbox_rc=1))
    m = await a.metrics()
    assert point(m, "os.gateway_up") == 1.0 and point(m, "os.sandboxes") is None


# ── events ──────────────────────────────────────────────────────────────────────────────────────
async def test_events_create_phase_delete_and_failed_poll_is_not_a_delete(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, w = make_adapter()
    assert await a.recent_events() == []                                  # baseline of an empty gateway
    live = json.loads(LIVE_SANDBOX_LIST)
    w.sandboxes = json.dumps([{**live[0], "phase": "Provisioning"}])
    a._snap.ts = 0.0
    await a.recent_events()
    w.sandboxes = LIVE_SANDBOX_LIST                                       # Provisioning -> Ready
    a._snap.ts = 0.0
    await a.recent_events()
    w.sandbox_rc = 1                                                      # gateway hiccup: NOT "deleted"
    a._snap.ts = 0.0
    await a.recent_events()
    w.sandbox_rc = 0
    w.sandboxes = "[]"
    a._snap.ts = 0.0
    evs = await a.recent_events()
    actions = [e.data["action"] for e in reversed(evs)]
    assert actions == ["created", "phase", "deleted"]
    assert all(e.kind == "openshell.sandbox" and e.system == "openshell" for e in evs)
    assert evs[0].data["name"] == "nc-adapter-probe" and "deleted" in evs[0].summary
    assert set(evs[0].data) == {"name", "id", "phase", "action", "previous_phase"}   # metadata only
    assert evs[1].data["previous_phase"] == "Provisioning" and evs[1].data["phase"] == "Ready"
    assert [e.ts for e in evs] == sorted((e.ts for e in evs), reverse=True)          # newest first


async def test_first_poll_reports_existing_sandboxes_as_present(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(sandboxes=LIVE_SANDBOX_LIST))
    evs = await a.recent_events()
    assert len(evs) == 1 and evs[0].data["action"] == "present"
    assert evs[0].ts == pytest.approx(1792841080.0, abs=2)               # created_at is UTC


async def test_event_memory_is_bounded(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, w = make_adapter()
    for i in range(osh.RING + 50):
        w.sandboxes = json.dumps([{"name": f"sb-{i}", "id": f"id-{i}", "phase": "Ready", "created_at": "2026-10-24 11:00:00"}])
        a._snap.ts = 0.0
        await a.recent_events()
        a._gw_cache = (0.0, False)
    assert len(a._ring) == osh.RING and len(a._known) <= osh.MAX_SANDBOXES
    assert len(await a.recent_events(limit=7)) == 7


async def test_follow_events_delivers_and_cancels_promptly(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, w = make_adapter()
    got: list[str] = []

    async def consume() -> None:
        async for ev in a.follow_events():
            got.append(ev.data["action"])

    task = asyncio.create_task(consume())
    await asyncio.sleep(0.05)
    w.sandboxes = LIVE_SANDBOX_LIST
    a._snap.ts = 0.0
    a._gw_cache = (0.0, False)
    for _ in range(100):
        if got:
            break
        await asyncio.sleep(0.02)
        a._snap.ts = 0.0
    assert got == ["created"]
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(task, 1.0)


async def test_follow_events_replays_since_ts(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(sandboxes=LIVE_SANDBOX_LIST))
    agen = a.follow_events(since_ts=0.0)
    ev = await asyncio.wait_for(anext(agen), 1.0)
    assert ev.data["name"] == "nc-adapter-probe"
    await agen.aclose()


# ── config files: read-only (ADR 0007) ──────────────────────────────────────────────────────────
def test_config_files_are_all_read_only_with_a_reason(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    specs = a.config_files()
    assert [s.id for s in specs] == ["gateway", "policy", "provider-profile", "compose"]
    assert [s.format for s in specs] == ["toml", "yaml", "yaml", "yaml"]
    for s in specs:
        assert s.editable is False and s.why_readonly and "ADR 0007" in s.why_readonly and s.authoritative is False


def test_no_config_files_without_paths(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(with_paths=False)
    assert a.config_files() == []
    with pytest.raises(KeyError):
        a.config_path("gateway")


async def test_read_config_returns_the_file_and_its_raw_hash(make_adapter) -> None:  # type: ignore[no-untyped-def]
    import hashlib
    a, _, _ = make_adapter()
    cf = await a.read_config("policy")
    raw = a.config_path("policy").read_bytes()
    assert cf.sha256 == hashlib.sha256(raw).hexdigest() and "network_policies" in cf.content
    assert "host_gateway_ip" in (await a.read_config("gateway")).content


async def test_read_config_scrubs_secret_looking_text(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    p = a.config_path("provider-profile")
    p.write_text(p.read_text() + "\n# api_key: sk-abcdefghijklmnopqrstuvwxyz0123456789\n")
    assert "sk-abcdefghijklmnopqrstuvwxyz0123456789" not in (await a.read_config("provider-profile")).content


async def test_unknown_config_id_is_a_key_error(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    for bad in ("nope", "../policy", ""):
        with pytest.raises(KeyError):
            a.config_path(bad)
        with pytest.raises(KeyError):
            await a.read_config(bad)
        with pytest.raises(KeyError):
            await a.validate_config(bad, "x")
        with pytest.raises(KeyError):
            await a.post_apply(bad, restart=False)


async def test_symlink_out_of_the_runner_dir_is_refused(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    outside = tmp_path / "outside.yaml"
    outside.write_text("secret: 1\n")
    pol = a.ctx.paths["openshell_policy"]
    pol.unlink()
    pol.symlink_to(outside)
    with pytest.raises(KeyError):
        a.config_path("policy")


async def test_missing_file_reads_as_file_not_found(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    a.config_path("compose").unlink()
    with pytest.raises(FileNotFoundError):
        await a.read_config("compose")


async def test_validate_is_a_parse_only_proxy_and_apply_refuses(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    good = await a.validate_config("gateway", "[openshell]\nversion = 1\n")
    assert good.ok and good.authoritative is False and "ADR 0007" in (good.note or "")
    bad = await a.validate_config("gateway", "[openshell\n")
    assert not bad.ok and bad.diagnostics[0].severity == "error"
    badyaml = await a.validate_config("policy", "a: [1, 2\n")
    assert not badyaml.ok
    res = await a.post_apply("policy", restart=True)
    assert res.ok is False and res.restarted is False


# ── actions ─────────────────────────────────────────────────────────────────────────────────────
def test_action_catalog(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    acts = {s.id: s for s in a.actions()}
    assert list(acts) == ["doctor", "list_sandboxes", "sandbox_get", "sandbox_logs", "gateway_logs", "prove_policy",
                          "gateway_up", "gateway_stop", "delete_sandbox"]
    assert {i for i, s in acts.items() if s.danger == "safe"} == {
        "doctor", "list_sandboxes", "sandbox_get", "sandbox_logs", "gateway_logs", "prove_policy"}
    assert acts["gateway_up"].danger == acts["gateway_stop"].danger == "disruptive"
    assert acts["delete_sandbox"].danger == "destructive"
    assert all(s.role == "admin" for s in acts.values())
    assert a.playground() is None


async def test_unknown_action_is_a_key_error(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter()
    with pytest.raises(KeyError):
        await a.run_action("sandbox_create", {})


async def test_doctor_and_list(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, w = make_adapter(World(sandboxes=LIVE_SANDBOX_LIST))
    r = await a.run_action("doctor", {})
    assert r.ok and "All checks passed." in r.output and ["openshell", "doctor", "check"] in run.calls
    r = await a.run_action("list_sandboxes", {})
    assert r.ok and "nc-adapter-probe" in r.output and "Ready" in r.output and "\x1b" not in r.output
    w.sandboxes = "[]"
    assert (await a.run_action("list_sandboxes", {})).output == "No sandboxes found."
    w.sandbox_rc = 1
    r = await a.run_action("list_sandboxes", {})
    assert not r.ok and r.problems[0].code == "openshell.gateway.unreachable" and "up -d" in (r.problems[0].fix or "")


async def test_actions_report_a_missing_cli(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _, _ = make_adapter(World(cli_rc=127, cli_version=""))
    r = await a.run_action("doctor", {})
    assert not r.ok and r.problems[0].code == "openshell.cli.missing"


async def test_delete_validates_the_name_against_the_live_list(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, w = make_adapter(World(sandboxes=LIVE_SANDBOX_LIST))
    run.on([*GW, "sandbox", "delete"], ok("Deleted sandbox nc-adapter-probe\n"))
    for bad in ("--all", "-x", "no-such", "", "a b", "../x", "x;rm", None, 7, "nc-adapter-probe\n--all"):
        r = await a.run_action("delete_sandbox", {"name": bad})
        assert not r.ok
    assert run.count([*GW, "sandbox", "delete"]) == 0                    # nothing reached the CLI
    r = await a.run_action("delete_sandbox", {"name": "nc-adapter-probe"})
    assert r.ok and [*GW, "sandbox", "delete", "nc-adapter-probe"] in run.calls
    assert run.count([*GW, "sandbox", "delete"]) == 1
    w.sandbox_rc = 1                                                      # cannot list -> cannot validate -> no delete
    r = await a.run_action("delete_sandbox", {"name": "nc-adapter-probe"})
    assert not r.ok and run.count([*GW, "sandbox", "delete"]) == 1


async def test_sandbox_get_and_logs(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter(World(sandboxes=LIVE_SANDBOX_LIST))
    run.on([*GW, "sandbox", "get"], ok(fx("sandbox-get.raw.txt")))
    run.on([*GW, "logs"], ok(fx("logs.txt")))
    r = await a.run_action("sandbox_get", {"name": "nc-adapter-probe"})
    assert r.ok and "Phase: Ready" in r.output and "\x1b" not in r.output
    r = await a.run_action("sandbox_logs", {"name": "nc-adapter-probe", "lines": 5})
    assert r.ok and [*GW, "logs", "nc-adapter-probe", "-n", "5"] in run.calls and "Landlock" in r.output
    for bad in (0, 501, True, "5", None):
        assert not (await a.run_action("sandbox_logs", {"name": "nc-adapter-probe", "lines": bad})).ok
    assert not (await a.run_action("sandbox_logs", {"name": "other", "lines": 5})).ok


async def test_gateway_logs_argv_and_redaction(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter()
    run.on(["docker", "logs"], ok("line\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz012345\n"))
    r = await a.run_action("gateway_logs", {"lines": 20})
    assert r.ok and ["docker", "logs", "--tail", "20", "ai-lab-dd-gateway"] in run.calls
    assert "abcdefghijklmnopqrstuvwxyz012345" not in r.output
    assert not (await a.run_action("gateway_logs", {"lines": 10_000})).ok


# prove_policy ---------------------------------------------------------------------------------
def _prove_runner(run: FakeRunner, out: str, rc: int, seen: dict[str, str]) -> None:
    def respond(argv: list[str]) -> object:
        cred = Path(argv[argv.index("--credentials") + 1])
        seen["descriptor"] = cred.read_text()
        seen["mode"] = oct(cred.stat().st_mode & 0o777)
        seen["path"] = str(cred)
        seen["argv"] = json.dumps(argv)
        return ok(out, rc=rc)
    run.on(["openshell", "policy", "prove"], respond)  # type: ignore[arg-type]


async def test_prove_derives_a_descriptor_without_any_key(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter()
    seen: dict[str, str] = {}
    _prove_runner(run, fx("prove-clean.raw.txt"), 0, seen)
    r = await a.run_action("prove_policy", {"scopes": "read, chat"})
    assert r.ok and r.problems == [] and "no findings" in r.output and "\x1b" not in r.output
    desc = yaml.safe_load(seen["descriptor"])
    assert desc == {"credentials": [{"name": "litellm_dd_key", "type": "bearer", "scopes": ["read", "chat"],
                                     "injected_via": "LITELLM_DD_KEY", "target_hosts": ["172.16.0.5"]}]}
    assert seen["mode"] == "0o600" and not Path(seen["path"]).exists()   # 0600, removed afterwards
    argv = json.loads(seen["argv"])
    assert argv[:5] == ["openshell", "policy", "prove", "--policy", str(a.ctx.paths["openshell_policy"])]
    assert argv[-1] == "--compact" and "-g" not in argv


async def test_prove_findings_are_a_result_not_a_failure(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter()
    _prove_runner(run, fx("prove-compact-findings.raw.txt"), 1, {})
    r = await a.run_action("prove_policy", {})
    assert r.ok and [p.code for p in r.problems] == ["openshell.policy.findings"]
    assert "8 finding" in r.problems[0].message and "credential_reach_expansion" in r.output


async def test_prove_error_output_is_a_failure(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter()
    _prove_runner(run, fx("prove-bad.raw.txt"), 1, {})
    r = await a.run_action("prove_policy", {"compact": False})
    assert not r.ok and "parsing credentials YAML" in r.output
    assert "--compact" not in run.calls[-1]


async def test_prove_rejects_bad_scopes_and_missing_inputs(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter()
    for bad in ("READ", "a;b", "x" * 40, ",".join(f"s{i}" for i in range(20)), "-x"):
        assert not (await a.run_action("prove_policy", {"scopes": bad})).ok
    assert run.count(["openshell", "policy", "prove"]) == 0
    a.ctx.paths["openshell_runner_dir"].joinpath("profile-lab-router.yaml").write_text("credentials: []\n")
    r = await a.run_action("prove_policy", {})
    assert not r.ok and "declares no credential" in r.output


# gateway lifecycle -----------------------------------------------------------------------------
def _lifecycle(run: FakeRunner, w: World) -> None:
    run.on(["docker", "compose"], ok("Container ai-lab-dd-gateway  Running\n"))
    run.on(["docker", "inspect", "ai-lab-litellm-litellm-1"], ok("ai-lab=172.21.0.17 openshell-docker=172.16.0.5 "))
    run.on(["docker", "network"], ok(""))
    run.on(["docker", "network", "inspect"], lambda a: ok(w.internal + "\n"))


async def test_gateway_up_mirrors_ensure_gateway(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, w = make_adapter()
    _lifecycle(run, w)
    r = await a.run_action("gateway_up", {})
    compose = str(a.ctx.paths["openshell_compose"])
    assert r.ok and "Connected" in r.output and "already pinned at 172.16.0.5" in r.output
    calls = [c for c in run.calls if c[0] == "docker" and c[1] in ("compose", "network") or c[:2] == ["docker", "inspect"]
             and c[2] == "ai-lab-litellm-litellm-1"]
    assert calls[0] == ["docker", "network", "inspect", "openshell-docker", "--format", "{{.Internal}}"]
    assert calls[1] == ["docker", "compose", "-f", compose, "up", "-d"]       # the exact data-designer-local argv
    assert not any("down" in c or "-v" in c for c in run.calls)


async def test_gateway_up_repins_the_router_when_missing(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, w = make_adapter()
    _lifecycle(run, w)
    run.on(["docker", "inspect", "ai-lab-litellm-litellm-1"], ok("ai-lab=172.21.0.17 "))
    r = await a.run_action("gateway_up", {})
    assert r.ok and "router pinned at 172.16.0.5" in r.output
    assert ["docker", "network", "disconnect", "openshell-docker", "ai-lab-litellm-litellm-1"] in run.calls
    assert ["docker", "network", "connect", "--ip", "172.16.0.5", "--alias", "litellm", "openshell-docker",
            "ai-lab-litellm-litellm-1"] in run.calls


async def test_gateway_up_refuses_without_an_internal_network(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, w = make_adapter(World(internal="false"))
    _lifecycle(run, w)
    r = await a.run_action("gateway_up", {})
    assert not r.ok and r.problems[0].code == "openshell.network.not_internal"
    assert run.count(["docker", "compose"]) == 0


async def test_gateway_up_reports_compose_failure_and_readiness_timeout(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, w = make_adapter()
    _lifecycle(run, w)
    run.on(["docker", "compose"], ok("", rc=1, stderr="boom"))
    r = await a.run_action("gateway_up", {})
    assert not r.ok and "gateway compose failed" in r.output
    run.on(["docker", "compose"], ok(""))
    w.status_out = fx("status-disconnected.HAND-WRITTEN.raw.txt")
    a.ready_timeout = 0.05
    r = await a.run_action("gateway_up", {})
    assert not r.ok and "did not become reachable" in r.output and r.problems[0].code == "openshell.gateway.disconnected"


async def test_gateway_stop_refuses_with_sandboxes_unless_forced(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, w = make_adapter(World(sandboxes=LIVE_SANDBOX_LIST))
    _lifecycle(run, w)
    r = await a.run_action("gateway_stop", {})
    assert not r.ok and r.problems[0].code == "openshell.stop.sandboxes_exist" and "nc-adapter-probe" in r.output
    assert run.count(["docker", "compose"]) == 0
    r = await a.run_action("gateway_stop", {"force": True})
    assert r.ok and ["docker", "compose", "-f", str(a.ctx.paths["openshell_compose"]), "stop"] in run.calls
    w.sandboxes = "[]"
    a._snap.ts = 0.0
    assert (await a.run_action("gateway_stop", {})).ok
    assert not any(c[:2] == ["docker", "compose"] and ("down" in c or "-v" in c) for c in run.calls)


async def test_runner_exceptions_are_data(make_adapter) -> None:  # type: ignore[no-untyped-def]
    class Boom:
        async def run(self, *a: object, **k: object) -> object:
            raise PermissionError("binary 'openshell' is not allowed")

    a, _, _ = make_adapter()
    a.ctx = type(a.ctx)(runner=Boom(), http=a.ctx.http, urls={}, paths=a.ctx.paths, state_dir=a.ctx.state_dir)  # type: ignore[arg-type]
    st = await a.status()
    assert st.health == "down"
    assert not (await a.metrics()).available
    assert not (await a.run_action("doctor", {})).ok


async def test_configured_container_names(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run, _ = make_adapter()
    a.containers = ("gw-x", "fwd-x")
    await a.status()
    assert any(c[-2:] == ["gw-x", "fwd-x"] for c in run.calls if c[:2] == ["docker", "inspect"])
    assert OpenShellAdapter.containers == ("ai-lab-dd-gateway", "ai-lab-dd-cli-fwd")      # Settings default


def test_runner_factory_roundtrip() -> None:
    r, _ = make_runner()
    assert isinstance(r, FakeRunner)


async def test_gateway_state_is_read_right_after_boot(make_adapter, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    """CI 2026-10-24: a cache stamped 0.0 looked fresh while time.monotonic() (seconds since boot) was small, so the
    events poll trusted the initial 'gateway not running' without asking docker. A never-filled cache must be stale."""
    import nemo_console.adapters.openshell as mod
    monkeypatch.setattr(mod.time, "monotonic", lambda: 5.0)
    a = make_adapter()
    a = a[0] if isinstance(a, tuple) else a
    assert await a._gateway_running() is True
