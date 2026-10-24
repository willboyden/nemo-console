"""actions: golden docker argv (constant templates), diag parsing, run dirs, no serials, failure as data."""
from __future__ import annotations

import asyncio
import json
import re
import stat
from pathlib import Path

import pytest

from nemo_console.adapters.dcgm import DIAG_CMD, DISCOVERY_CMD, DcgmAdapter, summarize_diag

from .conftest import diag_doc
from .helpers import CONTAINER, FakeRunner, bad, fx, is_restart, is_rm, is_run, ok

IMG = ("nvcr.io/nvidia/cloud-native/dcgm:4.6.1-1-ubuntu24.04"
       "@sha256:48464e50aa76671f5b204264cb4999739591112bf74330a727523f5655c6d7f2")


def golden(name: str, script: str) -> list[str]:
    return ["docker", "run", "--rm", "--pull", "never", "--name", name, "--gpus", "all", "--cap-drop", "ALL",
            "--cap-add", "SYS_ADMIN", "--network", "none", "--security-opt", "no-new-privileges", "--read-only",
            "--tmpfs", "/tmp", "--pids-limit", "256", "--memory", "2g",
            "--entrypoint", "/bin/sh", IMG, "-c", script]


async def prime(a: DcgmAdapter) -> None:
    await a.metrics()          # learns the GPUs for the enum


def test_constant_templates() -> None:
    assert DIAG_CMD[1] == "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi diag -r 1 -i {gpu} -j"
    assert DIAG_CMD[2] == "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi diag -r 2 -i {gpu} -j"
    assert DISCOVERY_CMD == "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi discovery -l"


async def test_action_specs(adapter: DcgmAdapter) -> None:
    await prime(adapter)
    specs = {s.id: s for s in adapter.actions()}
    assert set(specs) == {"diag", "diag_level2", "discovery", "restart_exporter"}
    assert specs["diag"].danger == "disruptive" and specs["discovery"].danger == "safe"
    assert specs["diag_level2"].danger == "disruptive" and specs["restart_exporter"].danger == "disruptive"
    g = specs["diag"].params[0]
    assert g.key == "gpu" and g.type == "enum" and g.choices == ["0", "1"]
    assert "Max-Q" in g.help and "GPU-" not in g.help
    assert specs["diag_level2"].params[0].choices == ["0", "1"]


async def test_gpu_param_before_discovery_is_a_bounded_int(adapter: DcgmAdapter) -> None:
    g = adapter.actions()[0].params[0]
    assert g.type == "int" and g.min == 0 and g.max == 15 and g.choices is None


async def test_diag_level1_golden_argv_run_dir_and_event(adapter: DcgmAdapter, runner: FakeRunner, tmp_path: Path) -> None:
    runner.on(is_run, ok(fx("diag_level1_gpu0.json")))          # REAL level-1 output, serial already redacted
    await prime(adapter)
    res = await adapter.run_action("diag", {"gpu": "0"})
    assert res.ok, res.output
    [argv] = runner.argvs("docker", "run")
    assert argv == golden(argv[6], "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi diag -r 1 -i 0 -j")
    assert argv[:6] == ["docker", "run", "--rm", "--pull", "never", "--name"]
    assert re.fullmatch(r"nemo-console-dcgm-\d{8}t\d{6}-\d{3}-diag1-gpu0", argv[6])
    assert "PASS" in res.output and "software" in res.output and "RTX PRO 6000 Blackwell Max-Q" in res.output
    [run] = (tmp_path / "state" / "dcgm" / "runs").iterdir()
    assert stat.S_IMODE(run.stat().st_mode) == 0o700
    assert stat.S_IMODE((run / "run.json").stat().st_mode) == 0o600
    meta = json.loads((run / "run.json").read_text())
    assert meta["verdict"] == "pass" and meta["gpu"] == 0 and meta["level"] == 1
    evs = await adapter.recent_events()
    d = [e for e in evs if e.kind == "dcgm.diag"]
    assert len(d) == 1 and d[0].data["verdict"] == "pass" and d[0].data["tests"] == {"software": "Pass"}


async def test_serial_numbers_never_stored_or_shown(adapter: DcgmAdapter, runner: FakeRunner, tmp_path: Path) -> None:
    doc = json.loads(fx("diag_level1_gpu0.json"))
    doc["entity_groups"][0]["entities"][0]["serial_num"] = "1794225037164"
    runner.on(is_run, ok(json.dumps(doc)))
    await prime(adapter)
    res = await adapter.run_action("diag", {"gpu": 0})
    blob = res.output + "".join(p.read_text() for p in (tmp_path / "state").rglob("*.json"))
    blob += "".join(e.model_dump_json() for e in await adapter.recent_events())
    assert "1794225037164" not in blob and "serial" not in blob.lower()


async def test_level2_is_its_own_disruptive_action_with_longer_timeout(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    seen: list[float] = []

    async def run(argv: list[str], *, timeout: float = 15.0, max_bytes: int = 0, stdin: None = None, cwd: None = None):  # type: ignore[no-untyped-def]
        seen.append(timeout)
        return ok(diag_doc({"memory": "Pass", "diagnostic": "Warn"}, "ECC pending")) if argv[:2] == ["docker", "run"] \
            else ok("sha256:x\n")
    adapter.ctx.runner.run = run          # type: ignore[method-assign,assignment]
    await prime(adapter)
    res = await adapter.run_action("diag_level2", {"gpu": "1"})
    assert res.ok and "WARN" in res.output and "ECC pending" in res.output
    assert 600.0 in seen


async def test_failed_test_is_not_ok(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.on(is_run, ok(diag_doc({"memory": "Fail"}, "uncorrectable ECC on GPU 1")))
    await prime(adapter)
    res = await adapter.run_action("diag_level2", {"gpu": "1"})
    assert not res.ok and "FAIL" in res.output
    [e] = [e for e in await adapter.recent_events() if e.kind == "dcgm.diag"]
    assert e.data["verdict"] == "fail"


def test_summary_level_warnings_are_reported() -> None:
    """Real shape seen live under a read-only root: results Pass, test_summary Fail + warnings."""
    doc = {"DCGM Diagnostic": {"test_categories": [{"tests": [{"name": "software", "results": [{"status": "Pass"}],
           "test_summary": {"status": "Fail", "warnings": [{"warning": "No permission to create a file"}]}}]}]}}
    verdict, _, notes = summarize_diag(doc)
    assert verdict == "fail" and notes == ["No permission to create a file"]


def test_summarize_diag_handles_odd_shapes() -> None:
    assert summarize_diag({})[0] == "error"
    assert summarize_diag({"DCGM Diagnostic": {"test_categories": [{"tests": [{"name": "x", "results": [
        {"status": "Skip"}]}]}]}})[0] == "pass"
    assert summarize_diag({"DCGM Diagnostic": "nonsense"})[0] == "error"


@pytest.mark.parametrize("bad_gpu", ["2", "-1", "0; rm -rf /", "", None, True, 1.5, "01x", "99999"])
async def test_gpu_param_is_validated_before_anything_runs(adapter: DcgmAdapter, runner: FakeRunner, bad_gpu: object) -> None:
    await prime(adapter)
    res = await adapter.run_action("diag", {"gpu": bad_gpu})
    assert not res.ok and "gpu must be" in res.output
    assert not runner.argvs("docker", "run")


async def test_timeout_removes_the_container(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.on(is_run, bad(rc=-9, timed_out=True))
    runner.on(is_rm, ok())
    await prime(adapter)
    res = await adapter.run_action("diag", {"gpu": "0"})
    assert not res.ok and "timed out" in res.output
    [rm] = runner.argvs("docker", "rm", "-f")
    assert rm[3].startswith("nemo-console-dcgm-")


async def test_missing_image_is_an_actionable_problem(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.on(is_run, bad("Unable to find image 'x' locally", rc=125))
    await prime(adapter)
    res = await adapter.run_action("diag", {"gpu": "0"})
    assert not res.ok
    p = next(p for p in res.problems if p.code == "dcgm.diag.image_missing")
    assert p.fix == f"docker pull {IMG}"


async def test_no_docker_is_data(adapter: DcgmAdapter) -> None:
    a = adapter
    a.ctx.runner.rules.clear()                                        # type: ignore[attr-defined]
    res = await a.run_action("discovery", {})
    assert not res.ok and "failed" in res.output


async def test_only_one_diag_at_a_time(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    gate = asyncio.Event()
    base = runner.run

    async def slow(argv: list[str], **kw):  # type: ignore[no-untyped-def]
        if argv[:2] == ["docker", "run"]:
            await gate.wait()
        return await base(argv, **kw)
    runner.run = slow                    # type: ignore[method-assign]
    runner.on(is_run, ok(fx("diag_level1_gpu0.json")))
    await prime(adapter)
    first = asyncio.create_task(adapter.run_action("diag", {"gpu": "0"}))
    await asyncio.sleep(0.05)
    second = await adapter.run_action("diag", {"gpu": "1"})
    assert not second.ok and "another diagnostic" in second.output
    gate.set()
    assert (await first).ok


async def test_discovery_shortens_uuids(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.on(is_run, ok(fx("discovery.txt")))
    res = await adapter.run_action("discovery", {})
    assert res.ok and "2 GPUs found" in res.output and "Max-Q" in res.output
    assert "GPU-00000000-0000-0000-0000-000000000001" not in res.output and "GPU-00000000..." in res.output
    [argv] = runner.argvs("docker", "run")
    assert argv[-1] == "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi discovery -l" and argv[6].startswith("nemo-console-dcgm-disc-")


async def test_restart_exporter(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    res = await adapter.run_action("restart_exporter", {})
    assert res.ok and runner.argvs("docker", "restart") == [["docker", "restart", CONTAINER]]
    runner.rules.insert(0, (is_restart, lambda _a: bad("Error: No such container")))
    assert not (await adapter.run_action("restart_exporter", {})).ok


async def test_restart_refused_for_a_foreign_container(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.rules.insert(0, (lambda a: a[:2] == ["docker", "inspect"] and "compose.project" in " ".join(a),
                            lambda _a: ok("someone-else|dcgm-exporter\n")))
    res = await adapter.run_action("restart_exporter", {})
    assert not res.ok and "refusing to restart" in res.output and "someone-else" in res.output
    ap = await adapter.post_apply("counters", restart=True)
    assert ap.post_check == "unhealthy" and not ap.restarted and "refusing" in ap.detail
    runner.rules.insert(0, (lambda a: a[:2] == ["docker", "inspect"] and "compose.project" in " ".join(a),
                            lambda _a: ok("ai-lab-observability|prometheus\n")))
    assert not (await adapter.run_action("restart_exporter", {})).ok
    assert not runner.argvs("docker", "restart")


async def test_restart_refused_when_labels_unreadable(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.rules.insert(0, (lambda a: a[:2] == ["docker", "inspect"] and "compose.project" in " ".join(a),
                            lambda _a: bad("Error: No such object")))
    res = await adapter.run_action("restart_exporter", {})
    assert not res.ok and "cannot inspect" in res.output and not runner.argvs("docker", "restart")


async def test_unknown_action_is_keyerror(adapter: DcgmAdapter) -> None:
    with pytest.raises(KeyError):
        await adapter.run_action("rm_rf", {})


async def test_runs_are_pruned_and_ignored_when_foreign(adapter: DcgmAdapter, runner: FakeRunner, tmp_path: Path) -> None:
    runs = tmp_path / "state" / "dcgm" / "runs"
    runs.mkdir(parents=True)
    (runs / "not-a-run").mkdir()
    for i in range(105):
        d = runs / f"20260101T0000{i // 100}0-{i:03d}-diag1-gpu0"
        d.mkdir()
        (d / "run.json").write_text(json.dumps({"run_id": d.name, "ts": 1.0 + i, "gpu": 0, "level": 1,
                                                "verdict": "pass", "tests": []}))
    runner.on(is_run, ok(fx("diag_level1_gpu0.json")))
    await prime(adapter)
    await adapter.run_action("diag", {"gpu": 0})
    assert len([p for p in runs.iterdir() if p.name != "not-a-run"]) == 100


async def test_follow_yields_a_diag_run_once(adapter: DcgmAdapter, runner: FakeRunner) -> None:
    runner.on(is_run, ok(fx("diag_level1_gpu0.json")))
    await prime(adapter)
    got: list[str] = []

    async def consume() -> None:
        async for ev in adapter.follow_events():
            got.append(ev.kind)
            if len(got) >= 1:
                return
    t = asyncio.create_task(consume())
    await asyncio.sleep(0.1)
    await adapter.run_action("diag", {"gpu": 0})
    await asyncio.wait_for(t, 3.0)
    assert got == ["dcgm.diag"]
