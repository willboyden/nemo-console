"""The real CommandRunner against tiny fake executables in a temp dir (no docker needed)."""
from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from nemo_console.services.runner import RunnerError, SubprocessRunner


def _bin(d: Path, name: str, script: str) -> None:
    (d / name).write_text("#!/bin/sh\n" + script)
    (d / name).chmod(0o755)


@pytest.fixture
def bindir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    import os
    d = tmp_path / "bin"
    d.mkdir()
    monkeypatch.setenv("PATH", f"{d}{os.pathsep}{os.environ['PATH']}")   # fakes shadow any real docker
    return d


async def test_runs_allowlisted_binary_with_argv_no_shell(bindir: Path) -> None:
    _bin(bindir, "docker", 'printf "%s|" "$@"\n')
    marker = bindir / "pwned"
    r = await SubprocessRunner([str(bindir)]).run(["docker", "ps", f"; touch {marker}", "$(touch x)", "a b"])
    assert r.returncode == 0 and r.stdout == f"ps|; touch {marker}|$(touch x)|a b|"
    assert not marker.exists()


async def test_binary_allowlist(bindir: Path) -> None:
    _bin(bindir, "sh2", "echo hi\n")
    run = SubprocessRunner([str(bindir)])
    for argv in (["sh", "-c", "id"], ["/bin/sh"], ["bash"], ["../docker"], ["sh2"], []):
        with pytest.raises(RunnerError):
            await run.run(argv)
    with pytest.raises(RunnerError):
        await run.run(["docker", "a\x00b"])


async def test_timeout_kills_process(bindir: Path) -> None:
    _bin(bindir, "systemctl", "sleep 30\n")
    r = await asyncio.wait_for(SubprocessRunner([str(bindir)]).run(["systemctl", "x"], timeout=0.3), 5)
    assert r.timed_out is True and r.returncode != 0


async def test_output_cap_stops_runaway_producer(bindir: Path) -> None:
    _bin(bindir, "nemo-relay", "yes AAAAAAAAAAAAAAAA\n")
    r = await asyncio.wait_for(SubprocessRunner([str(bindir)]).run(["nemo-relay", "doctor"], max_bytes=1000, timeout=10), 8)
    assert len(r.stdout) <= 1000


async def test_stdin_bytes_and_env_is_minimal(bindir: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    _bin(bindir, "docker", 'cat; echo "KEY=${LITELLM_API_KEY:-unset}"\n')
    monkeypatch.setenv("LITELLM_API_KEY", "should-not-be-inherited-123")
    r = await SubprocessRunner([str(bindir)]).run(["docker", "x"], stdin=b"hello\n")
    assert "hello" in r.stdout and "KEY=unset" in r.stdout


async def test_nonzero_exit_reported(bindir: Path) -> None:
    _bin(bindir, "docker", "echo oops >&2; exit 3\n")
    r = await SubprocessRunner([str(bindir)]).run(["docker", "x"])
    assert r.returncode == 3 and "oops" in r.stderr


async def _group_gone(pgid: int) -> bool:
    """True once no process is left in the group (a SIGKILLed orphan stays a zombie until init reaps it)."""
    import os
    for _ in range(100):
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            return True
        await asyncio.sleep(0.02)
    return False


def test_long_timeout_cap_is_only_for_skillevaluator_tier3() -> None:
    from nemo_console.services.runner import MAX_LIVE_EVAL_TIMEOUT, MAX_TIMEOUT, max_timeout
    assert max_timeout(["skillevaluator-local", "tier3", "/s"]) == MAX_LIVE_EVAL_TIMEOUT == 4800.0
    assert max_timeout(["skillevaluator", "tier3"]) == MAX_LIVE_EVAL_TIMEOUT
    for argv in (["skillevaluator-local", "tier1", "/s"], ["skillevaluator-local", "doctor"], ["docker", "tier3"],
                 ["systemctl", "tier3"], ["nemo-relay", "tier3"], ["skillevaluator-local"]):
        assert max_timeout(argv) == MAX_TIMEOUT == 600.0, argv


async def test_cancelled_skillevaluator_launcher_gets_sigint_then_its_group_is_gone(bindir: Path,
                                                                                    monkeypatch: pytest.MonkeyPatch) -> None:
    from nemo_console.services import runner as runner_mod
    monkeypatch.setattr(runner_mod, "STOP_GRACE", 2.0)
    marker, pidfile = bindir / "got-int", bindir / "pid"
    # The lab launcher stops its job container on SIGINT; this stand-in records that it saw one.
    _bin(bindir, "skillevaluator-local", f'echo $$ > {pidfile}\ntrap "touch {marker}; exit 130" INT\nsleep 30 &\nwait\n')
    task = asyncio.ensure_future(SubprocessRunner([str(bindir)]).run(["skillevaluator-local", "tier3", "x"], timeout=60))
    for _ in range(100):
        if pidfile.exists() and pidfile.read_text().strip():
            break
        await asyncio.sleep(0.02)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(task, 10)
    assert marker.exists()                                   # interrupted first, not just killed
    assert await _group_gone(int(pidfile.read_text()))      # the whole group (incl. the background sleep) is gone


async def test_cancelled_other_binary_is_killed_without_grace(bindir: Path) -> None:
    import time
    pidfile = bindir / "pid"
    _bin(bindir, "docker", f'echo $$ > {pidfile}\ntrap "" INT\nsleep 30\n')     # ignores SIGINT: only SIGKILL stops it
    task = asyncio.ensure_future(SubprocessRunner([str(bindir)]).run(["docker", "x"], timeout=60))
    for _ in range(100):
        if pidfile.exists() and pidfile.read_text().strip():
            break
        await asyncio.sleep(0.02)
    t0 = time.monotonic()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(task, 10)
    assert time.monotonic() - t0 < 3
    assert await _group_gone(int(pidfile.read_text()))


def test_addendum_98_container_and_image_settings_refuse_option_like_values() -> None:
    """Audit L8: container names and the diag image reach docker argv; a leading '-' must never get through."""
    import pytest
    from pydantic import ValidationError

    from nemo_console.config import Settings
    for kw in ({"dcgm_exporter_container": "-v"}, {"guardrails_container": "--rm"},
               {"openshell_containers": ["ok-name", "-x"]}, {"dcgm_diag_image": "--privileged"}):
        with pytest.raises(ValidationError):
            Settings(**kw)  # type: ignore[arg-type]
    s = Settings()
    assert s.dcgm_exporter_container and s.guardrails_container and s.openshell_containers and s.dcgm_diag_image
