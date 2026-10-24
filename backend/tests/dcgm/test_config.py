"""counters.csv: read, validate (field/type check, honest authoritative=False), post_apply health + rollback signal."""
from __future__ import annotations

import itertools
from collections.abc import Callable
from pathlib import Path

import httpx
import pytest
import respx

from nemo_console.adapters.dcgm import KNOWN_FIELDS, DcgmAdapter, parse_counters

from .helpers import EXPORTER, FIX, FakeRunner, bad, fx, is_logs, is_restart, ok


def msgs(res) -> list[str]:  # type: ignore[no-untyped-def]
    return [f"{d.severity}:{d.line}:{d.message}" for d in res.diagnostics]


def test_known_fields_cover_the_live_counters_file() -> None:
    rows, diags = parse_counters(fx("counters.good.csv"))
    assert diags == [] and len(rows) > 20
    assert {f for _, f, _ in rows} <= set(KNOWN_FIELDS)
    assert KNOWN_FIELDS["DCGM_FI_DRIVER_VERSION"] == "l" and KNOWN_FIELDS["DCGM_FI_DEV_GPU_TEMP"] == "g"
    assert len(KNOWN_FIELDS) == 86


async def test_spec_and_read(adapter: DcgmAdapter, counters_file: Path) -> None:
    [spec] = adapter.config_files()
    assert spec.id == "counters" and spec.editable and spec.format == "text" and spec.authoritative is False
    assert spec.validators and "86" in spec.validators[0]
    cf = await adapter.read_config("counters")
    assert cf.content == counters_file.read_text() and len(cf.sha256) == 64 and cf.mtime > 0
    assert adapter.config_path("counters") == counters_file


async def test_config_path_rejects_unknown_symlink_and_missing(adapter: DcgmAdapter, counters_file: Path,
                                                               tmp_path: Path) -> None:
    for bad_id in ("routes", "../counters", "counters.csv", ""):
        with pytest.raises(KeyError):
            adapter.config_path(bad_id)
    link = tmp_path / "link.csv"
    link.symlink_to(counters_file)
    adapter.ctx.paths["dcgm_counters"] = link
    with pytest.raises(KeyError):
        adapter.config_path("counters")
    counters_file.unlink()
    adapter.ctx.paths["dcgm_counters"] = counters_file
    with pytest.raises(KeyError):
        adapter.config_path("counters")


async def test_validate_live_file_ok_not_authoritative(adapter: DcgmAdapter) -> None:
    res = await adapter.validate_config("counters", fx("counters.good.csv"))
    assert res.ok and res.authoritative is False and res.diagnostics == []
    assert res.note and "proxy" in res.note and "image" in res.note


async def test_validate_broken_candidate(adapter: DcgmAdapter) -> None:
    bad_csv = """# comment
DCGM_FI_DEV_GPU_TMP, gauge, typo of a real field
DCGM_FI_DEV_SM_CLOCK, histogram, wrong type
DCGM_FI_DEV_MEM_CLOCK, gauge
DCGM_FI_DEV_POWER_USAGE, gauge, ok
DCGM_FI_DEV_POWER_USAGE, gauge, duplicate
dcgm_gpu_temp, gauge, 1.x name
DCGM_FI_DEV_SERIAL, label, serial
DCGM_FI_DEV_QUITE_NEW_THING_XYZ, gauge, maybe a newer field
DCGM_FI_DEV_FB_USED, label, wrong kind
not a field, gauge, x
"""   # line numbers asserted below: typo=2 type=3 cols=4 dup=6 1.x=7 serial=8 new=9 kind=10 name=11
    res = await adapter.validate_config("counters", bad_csv)
    m = msgs(res)
    assert res.ok is False and res.authoritative is False
    assert any(x.startswith("error:2:") and "DCGM_FI_DEV_GPU_TEMP" in x for x in m)
    assert any(x.startswith("error:3:") and "gauge, counter, label" in x for x in m)
    assert any(x.startswith("error:4:") and "three" in x for x in m)
    assert any(x.startswith("error:6:") and "line 5" in x for x in m)
    assert any(x.startswith("error:7:") and "1.x" in x for x in m)
    assert any(x.startswith("warn:8:") and "serial" in x for x in m)
    assert any(x.startswith("warn:9:") for x in m)
    assert any(x.startswith("warn:10:") and "label" in x for x in m)
    assert any(x.startswith("error:11:") for x in m)


async def test_validate_needs_an_active_metric_and_caps_size(adapter: DcgmAdapter) -> None:
    res = await adapter.validate_config("counters", "# only comments\nDCGM_FI_DRIVER_VERSION, label, v\n")
    assert not res.ok and any("no active gauge/counter" in d.message for d in res.diagnostics)
    res = await adapter.validate_config("counters", "#" * (300 * 1024))
    assert not res.ok and "256 KiB" in res.diagnostics[0].message
    with pytest.raises(KeyError):
        await adapter.validate_config("other", "x")


async def test_validate_never_writes_the_live_file(adapter: DcgmAdapter, counters_file: Path) -> None:
    before = counters_file.read_bytes()
    await adapter.validate_config("counters", "garbage")
    assert counters_file.read_bytes() == before


# ── post_apply ──────────────────────────────────────────────────────────────────────────────
async def test_post_apply_healthy_no_restart(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    await adapter.validate_config("counters", fx("counters.good.csv"))      # freezes the baseline like a real apply
    res = await adapter.post_apply("counters", restart=False)
    assert res.ok and res.post_check == "healthy" and not res.restarted
    assert not runner.argvs("docker", "restart")


async def test_post_apply_restart_calls_docker_restart_and_polls(adapter: DcgmAdapter, runner: FakeRunner,
                                                                 mock: respx.MockRouter) -> None:
    await adapter.validate_config("counters", fx("counters.good.csv"))
    answers = itertools.chain([httpx.ConnectError("refused")] * 2, itertools.repeat(httpx.Response(200, text=fx("metrics.txt"))))
    mock.get(f"{EXPORTER}/metrics").mock(side_effect=lambda _r: next(answers))
    res = await adapter.post_apply("counters", restart=True)
    assert res.ok and res.restarted and res.post_check == "healthy"
    assert runner.argvs("docker", "restart") == [["docker", "restart", "ai-lab-observability-dcgm-exporter-1"]]


async def test_post_apply_unhealthy_when_metrics_stop_answering(adapter: DcgmAdapter, mock: respx.MockRouter) -> None:
    await adapter.validate_config("counters", fx("counters.good.csv"))
    mock.get(f"{EXPORTER}/metrics").respond(500, text="boom")
    res = await adapter.post_apply("counters", restart=False)
    assert not res.ok and res.post_check == "unhealthy" and "did not answer" in res.detail


async def test_post_apply_unhealthy_when_a_served_family_vanishes(adapter: DcgmAdapter, mock: respx.MockRouter,
                                                                  counters_file: Path) -> None:
    await adapter.validate_config("counters", fx("counters.good.csv"))
    # exporter now serves no temperature although the file still configures it
    text = "".join(ln for ln in fx("metrics.txt").splitlines(keepends=True) if "DCGM_FI_DEV_GPU_TEMP" not in ln)
    mock.get(f"{EXPORTER}/metrics").respond(200, text=text)
    res = await adapter.post_apply("counters", restart=False)
    assert res.post_check == "unhealthy" and "DCGM_FI_DEV_GPU_TEMP" in res.detail


async def test_intentionally_removed_family_is_not_required(adapter: DcgmAdapter, mock: respx.MockRouter,
                                                            counters_file: Path) -> None:
    await adapter.validate_config("counters", fx("counters.good.csv"))
    counters_file.write_text(counters_file.read_text().replace("DCGM_FI_DEV_GPU_TEMP,    gauge, GPU temperature (in C).",
                                                               "# DCGM_FI_DEV_GPU_TEMP, gauge, off"))
    text = "".join(ln for ln in fx("metrics.txt").splitlines(keepends=True) if "DCGM_FI_DEV_GPU_TEMP" not in ln)
    mock.get(f"{EXPORTER}/metrics").respond(200, text=text)
    res = await adapter.post_apply("counters", restart=False)
    assert res.ok and res.post_check == "healthy", res.detail


async def test_no_reload_detected_without_restart(adapter: DcgmAdapter, counters_file: Path) -> None:
    """The file drops a field but the exporter keeps serving it -> it did not re-read -> unhealthy (rollback)."""
    await adapter.validate_config("counters", fx("counters.good.csv"))
    counters_file.write_text(counters_file.read_text().replace("DCGM_FI_DEV_GPU_TEMP,    gauge, GPU temperature (in C).",
                                                               "# DCGM_FI_DEV_GPU_TEMP, gauge, off"))
    res = await adapter.post_apply("counters", restart=False)
    assert res.post_check == "unhealthy" and "did not re-read" in res.detail and "DCGM_FI_DEV_GPU_TEMP" in res.detail


async def test_logged_errors_make_it_unhealthy(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.rules.insert(0, (is_logs, lambda _a: ok(
        'time=2026-10-24T11:20:00.000Z level=ERROR msg="Failed to reload counters" error="bad field"\n')))
    await adapter.validate_config("counters", fx("counters.good.csv"))
    res = await adapter.post_apply("counters", restart=False)
    assert res.post_check == "unhealthy" and "Failed to reload" in res.detail


async def test_restart_failure_is_unhealthy(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.rules.insert(0, (is_restart, lambda _a: bad("Error response from daemon: no such container")))
    res = await adapter.post_apply("counters", restart=True)
    assert res.post_check == "unhealthy" and not res.restarted


async def test_prof_families_late_are_a_note_not_a_failure(make_adapter: Callable[..., DcgmAdapter],
                                                          mock: respx.MockRouter) -> None:
    a = make_adapter()
    await a.validate_config("counters", fx("counters.good.csv"))           # baseline includes DCGM_FI_PROF_*
    text = "".join(ln for ln in fx("metrics.txt").splitlines(keepends=True) if "DCGM_FI_PROF_" not in ln)
    mock.get(f"{EXPORTER}/metrics").respond(200, text=text)
    res = await a.post_apply("counters", restart=True)
    assert res.ok and "DCGM_FI_PROF_" in res.detail


async def test_baseline_is_frozen_between_validate_and_apply(adapter: DcgmAdapter, mock: respx.MockRouter) -> None:
    await adapter.validate_config("counters", fx("counters.good.csv"))
    mock.get(f"{EXPORTER}/metrics").respond(200, text="DCGM_FI_DEV_FB_USED{gpu=\"0\"} 1\n")   # broken exporter state
    await adapter.metrics()                                                  # a UI poll during the apply window
    assert "DCGM_FI_DEV_GPU_TEMP" in adapter._baseline                       # not shrunk to the broken state
    res = await adapter.post_apply("counters", restart=False)
    assert res.post_check == "unhealthy"


def test_fixture_dir_has_no_serial_or_real_uuid() -> None:
    import re
    for f in FIX.iterdir():
        t = f.read_text()
        assert not re.search(r"GPU-(?!0{8}-0000-0000-0000-00000000000[01])[0-9a-f]{8}-", t), f.name
        assert not re.search(r'"serial_num": "(?!REDACTED)', t), f.name
