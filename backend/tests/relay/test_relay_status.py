from __future__ import annotations

import httpx
from relay_helpers import FakeRunner, fx, ok, standard_runner

WEAK = fx("plugins.toml").replace('attestation = "signature_required"', 'attestation = "integrity_only"')
NO_EXPORTERS = (
    fx("plugins.toml").replace("[components.config.atof]\nenabled = true", "[components.config.atof]\nenabled = false")
    .replace("[components.config.atif]\nenabled = true", "[components.config.atif]\nenabled = false")
    .replace("[components.config.opentelemetry]\nenabled = true", "[components.config.opentelemetry]\nenabled = false")
)


def codes(st) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in st.problems}


async def test_status_healthy_treats_401_as_up(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    st = await a.status()
    assert st.health == "up" and st.system == "relay" and st.version == "0.9.2"
    assert [c.state for c in st.components] == ["running", "running"]
    assert "401" in (st.components[1].detail or "") and "UP" in (st.components[1].detail or "")
    assert st.endpoints[0].url == "http://127.0.0.1:4040"
    assert not [p for p in st.problems if p.severity == "error"]
    assert "relay.config.newer_than_gateway" not in codes(st)
    assert ["systemctl", "--user", "--timestamp=unix", "show", "nemo-relay-gateway.service", "-p",
            "ActiveEnterTimestamp,ActiveState,SubState,MainPID,NRestarts,Result,LoadState,UnitFileState"] in run.calls


async def test_status_flags_weak_attestation_with_fix(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(plugins_toml=WEAK)
    st = await a.status()
    p = next(p for p in st.problems if p.code == "relay.plugin.attestation_weak")
    assert p.severity == "error" and "signature_required" in (p.fix or "") and "human decision" in (p.fix or "")


async def test_status_flags_exporters_off_and_missing_pii(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(plugins_toml=NO_EXPORTERS)
    st = await a.status()
    assert "relay.exporters.off" in codes(st)
    no_pii = fx("plugins.toml").replace('kind = "pii_redaction"\nenabled = true', 'kind = "pii_redaction"\nenabled = false')
    a2, _ = make_adapter(plugins_toml=no_pii)
    assert "relay.pii_redaction.off" in codes(await a2.status())


async def test_status_gateway_down_is_data_not_exception(make_adapter, router) -> None:  # type: ignore[no-untyped-def]
    router.get("http://127.0.0.1:4040/v1/models").mock(side_effect=httpx.ConnectError("refused"))
    inactive = "ActiveState=inactive\nSubState=dead\nLoadState=loaded\nMainPID=0\nNRestarts=0\nResult=success\nUnitFileState=enabled\n"
    a, _ = make_adapter(standard_runner(show=inactive))
    st = await a.status()
    assert st.health == "down"
    p = next(p for p in st.problems if p.code == "relay.gateway.down")
    assert p.fix == "systemctl --user restart nemo-relay-gateway.service"
    m = await a.metrics()
    assert m.available is False and m.points == [] and m.error


async def test_status_active_but_silent_is_degraded_and_5xx_is_degraded(make_adapter, router) -> None:  # type: ignore[no-untyped-def]
    router.get("http://127.0.0.1:4040/v1/models").mock(side_effect=httpx.ReadTimeout("slow"))
    a, _ = make_adapter()
    assert (await a.status()).health == "degraded"
    router.get("http://127.0.0.1:4040/v1/models").respond(502)
    st = await a.status()
    assert st.health == "degraded" and "relay.gateway.upstream_error" in codes(st)


async def test_status_survives_every_command_failing(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(FakeRunner())          # every argv -> rc 127
    st = await a.status()
    assert st.health == "up"                   # HTTP still answers
    assert st.version is None
    assert "relay.plugins.list_unavailable" in codes(st)


async def test_status_plugin_disabled_and_config_newer_than_gateway(make_adapter) -> None:  # type: ignore[no-untyped-def]
    disabled = fx("plugins_list.json").replace('"enabled": true', '"enabled": false')
    r = standard_runner()
    r.on(["nemo-relay", "plugins", "list", "--json"], ok(disabled))
    a, _ = make_adapter(r)
    assert "relay.plugin.disabled" in codes(await a.status())
    # gateway started long before the (fresh tmp) plugins.toml was written
    old = fx("systemctl_show_active.txt").replace("@1790223677", "@1000")
    a2, _ = make_adapter(standard_runner(show=old))
    assert "relay.config.newer_than_gateway" in codes(await a2.status())
    m = await a2.metrics()
    assert next(p for p in m.points if p.key == "relay.plugin_loaded").value == 0.0


async def test_metrics_canonical_keys_and_values(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    m = await a.metrics()
    assert m.available and m.system == "relay"
    got = {p.key: p.value for p in m.points}
    assert got["relay.gateway_up"] == 1.0
    assert got["relay.plugin_loaded"] == 1.0 or got["relay.plugin_loaded"] == 0.0
    assert got["relay.atof_files"] == 2.0
    assert got["relay.atof_bytes"] == float(len(fx("atof_sample.jsonl").encode()))
    assert got["relay.atif_trajectories"] == 2.0          # notes.txt is not a trajectory
    assert got["relay.exporters_enabled"] == 3.0
    assert set(got) <= {"relay.gateway_up", "relay.plugin_loaded", "relay.atof_files", "relay.atof_bytes",
                        "relay.atif_trajectories", "relay.exporters_enabled"}


async def test_metrics_omit_what_cannot_be_read(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter(FakeRunner())
    a.ctx.paths["relay_atof_dir"] = a.ctx.paths["relay_atof_dir"] / "gone"
    keys = {p.key for p in (await a.metrics()).points}
    assert "relay.atof_files" not in keys and "relay.plugin_loaded" not in keys
    assert "relay.gateway_up" in keys and "relay.exporters_enabled" in keys
