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
