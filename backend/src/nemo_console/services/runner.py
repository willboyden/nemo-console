"""The ONE process-control choke point (ADR 3): argv list, binary allowlist, timeout, output cap, redaction, no shell."""
from __future__ import annotations

import asyncio
import contextlib
import os
import shutil
import signal
import time
from pathlib import Path

from nemo_console.adapters.base import CommandResult
from nemo_console.services.common import redact

ALLOWED_BINARIES = frozenset({"docker", "systemctl", "nemo-relay"})
CHILD_ENV_KEYS = ("PATH", "HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "DOCKER_HOST", "DOCKER_CONTEXT",
                  "DOCKER_CONFIG", "LANG", "LC_ALL")
MAX_TIMEOUT = 600.0
MAX_ARGS = 64
MAX_ARG_LEN = 8192


class RunnerError(Exception):
    """Refused before spawning (bad binary / argv)."""


def _kill_group(proc: asyncio.subprocess.Process) -> None:
    """The child leads its own session/process group: kill the whole group so grandchildren cannot hold pipes open."""
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(proc.pid, signal.SIGKILL)
    with contextlib.suppress(ProcessLookupError):
        proc.kill()


class SubprocessRunner:
    def __init__(self, extra_bin_dirs: list[str] | None = None) -> None:
        self._extra = [str(Path(d).expanduser()) for d in (extra_bin_dirs or [])]

    def resolve(self, name: str) -> str:
        if name not in ALLOWED_BINARIES:          # bare names only: also rules out '/bin/sh', '../x'
            raise RunnerError(f"binary {name!r} is not allowed")
        path = os.pathsep.join([os.environ.get("PATH", os.defpath), *self._extra])
        found = shutil.which(name, path=path)
        if found is None:
            raise RunnerError(f"binary {name!r} not found on PATH")
        return found

    @staticmethod
    def _check(argv: list[str]) -> None:
        if not argv or len(argv) > MAX_ARGS:
            raise RunnerError("bad argv length")
        for a in argv:
            if not isinstance(a, str) or "\x00" in a or len(a) > MAX_ARG_LEN:
                raise RunnerError("bad argv element")

    def _env(self) -> dict[str, str]:
        env = {k: os.environ[k] for k in CHILD_ENV_KEYS if k in os.environ}
        env["PATH"] = os.pathsep.join([env.get("PATH", os.defpath), *self._extra])
        return env

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self._check(argv)
        exe = self.resolve(argv[0])
        timeout = max(0.1, min(float(timeout), MAX_TIMEOUT))
        start = time.monotonic()
        proc = await asyncio.create_subprocess_exec(
            exe, *argv[1:], stdin=asyncio.subprocess.PIPE if stdin is not None else asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, cwd=cwd, env=self._env(),
            start_new_session=True)
        assert proc.stdout is not None and proc.stderr is not None
        state = {"over": False}

        async def drain(stream: asyncio.StreamReader) -> bytes:
            buf = bytearray()
            while chunk := await stream.read(65536):
                if len(buf) < max_bytes:
                    buf += chunk[: max_bytes - len(buf)]
                if len(buf) >= max_bytes and not state["over"]:
                    state["over"] = True
                    _kill_group(proc)         # runaway output: stop the producer
            return bytes(buf)

        async def feed() -> None:
            if stdin is not None and proc.stdin is not None:
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    proc.stdin.write(stdin)
                    await proc.stdin.drain()
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    proc.stdin.close()

        timed_out = False
        try:
            out, err, _ = await asyncio.wait_for(asyncio.gather(drain(proc.stdout), drain(proc.stderr), feed()),
                                                 timeout=timeout)
            await asyncio.wait_for(proc.wait(), timeout=5)
        except TimeoutError:
            timed_out = True
            _kill_group(proc)
            with contextlib.suppress(Exception):
                await asyncio.wait_for(proc.wait(), timeout=5)
            out, err = b"", b""
        return CommandResult(
            returncode=proc.returncode if proc.returncode is not None else -1,
            stdout=redact(out.decode("utf-8", "replace")), stderr=redact(err.decode("utf-8", "replace")),
            timed_out=timed_out, duration_ms=int((time.monotonic() - start) * 1000))
