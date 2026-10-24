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

ALLOWED_BINARIES = frozenset({"docker", "systemctl", "nemo-relay", "skillevaluator", "skillevaluator-local",
                              "data-designer-local", "data-designer-export",
                              # addendum 9.8: the OpenShell CLI and the lab wrappers of AIPerf, NeMo Guardrails and
                              # NeMo Evaluator (each wrapper reads its OWN 0600 router key file; none needs our env)
                              "openshell", "aiperf-local", "guardrails-local", "evaluator-local"})
CHILD_ENV_KEYS = ("PATH", "HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "DOCKER_HOST", "DOCKER_CONTEXT",
                  "DOCKER_CONFIG", "LANG", "LC_ALL")
# SkillEvaluator is configured by environment only (SKILL_EVAL_*, no config file). Those variables reach ITS children
# and no other binary's. The `skillevaluator-local` wrapper needs none of them: it reads its own key file, so with the
# wrapper the console process never holds the provider key.
SKILLEVAL_BINARIES = frozenset({"skillevaluator", "skillevaluator-local"})
SKILLEVAL_ENV_PREFIXES = ("SKILL_EVAL_", "SKILLEVAL_")
SKILLEVAL_ENV_KEYS = ("LOCAL_AI_LAB", "LITELLM_URL")
MAX_TIMEOUT = 600.0
# SkillEvaluator Tier 3 (live agent runs in the lab's per-job runner container) may run as long as the launcher's own
# limit: 120 s + dataset generation 900 s + tier3 3600 s (clients/skillevaluator/runner/launch.py, container_timeout).
# ONLY `skillevaluator[-local] tier3 ...` gets this cap; every other command keeps MAX_TIMEOUT.
MAX_LIVE_EVAL_TIMEOUT = 4800.0
# Data Designer: ONLY `data-designer-local create ...` gets the long cap (a dataset build runs minutes to an hour,
# 3600 s, same ceiling as a skilleval tier3 job); preview/validate/check-models/export stay at MAX_TIMEOUT. The
# `data-designer-local` wrapper injects its own credentials from its own 0600 key files, so it needs NO env of ours
# (never pass secrets on its behalf); `data-designer-export` is a fixed parquet converter.
DD_BINARIES = frozenset({"data-designer-local"})
DD_CREATE = 3600.0
# addendum 9.8: ONLY `aiperf-local profile ...` (a load test) and `evaluator-local run ...` (a benchmark run) get long
# caps; every other subcommand of those wrappers keeps MAX_TIMEOUT. Both wrappers stop their children on SIGINT.
AIPERF_BINARIES = frozenset({"aiperf-local"})
AIPERF_PROFILE = 3600.0
EVALUATOR_BINARIES = frozenset({"evaluator-local"})
EVALUATOR_RUN = MAX_LIVE_EVAL_TIMEOUT
# SkillEvaluator launchers and the Data Designer wrapper are stopped with SIGINT first (the lab launchers clean up
# their own children/jobs on SIGINT), then SIGKILL after this grace. Every other binary gets SIGKILL straight away.
GRACEFUL_BINARIES = SKILLEVAL_BINARIES | DD_BINARIES | AIPERF_BINARIES | EVALUATOR_BINARIES
STOP_GRACE = 5.0        # short enough for `nemo-console stop`, which waits 10 s for the console to exit
MAX_ARGS = 64
MAX_ARG_LEN = 8192


class RunnerError(Exception):
    """Refused before spawning (bad binary / argv)."""


def max_timeout(argv: list[str]) -> float:
    """The cap for this argv: the long one only for a SkillEvaluator launcher's `tier3` subcommand,
    `data-designer-local create` (DD_CREATE), `aiperf-local profile` and `evaluator-local run` (addendum 9.8);
    everything else keeps MAX_TIMEOUT."""
    if argv[:1] and argv[0] in SKILLEVAL_BINARIES and argv[1:2] == ["tier3"]:
        return MAX_LIVE_EVAL_TIMEOUT
    if argv[:1] and argv[0] in DD_BINARIES and argv[1:2] == ["create"]:
        return DD_CREATE
    if argv[:1] and argv[0] in AIPERF_BINARIES and argv[1:2] == ["profile"]:
        return AIPERF_PROFILE
    if argv[:1] and argv[0] in EVALUATOR_BINARIES and argv[1:2] == ["run"]:
        return EVALUATOR_RUN
    return MAX_TIMEOUT


def _kill_group(proc: asyncio.subprocess.Process) -> None:
    """The child leads its own session/process group: kill the whole group so grandchildren cannot hold pipes open."""
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(proc.pid, signal.SIGKILL)
    with contextlib.suppress(ProcessLookupError):
        proc.kill()


async def _stop(proc: asyncio.subprocess.Process, *, graceful: bool) -> None:
    """Stop the child's whole group: SIGINT and a grace period first when `graceful`, SIGKILL in every case."""
    try:
        if graceful and proc.returncode is None:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(proc.pid, signal.SIGINT)
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(proc.wait(), timeout=STOP_GRACE)
    finally:
        _kill_group(proc)           # also reaps grandchildren that outlived the leader (and runs on a second cancel)
    with contextlib.suppress(Exception):
        await asyncio.wait_for(proc.wait(), timeout=5)


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

    def _env(self, binary: str = "") -> dict[str, str]:
        env = {k: os.environ[k] for k in CHILD_ENV_KEYS if k in os.environ}
        env["PATH"] = os.pathsep.join([env.get("PATH", os.defpath), *self._extra])
        if binary in SKILLEVAL_BINARIES:
            env.update({k: v for k, v in os.environ.items()
                        if k.startswith(SKILLEVAL_ENV_PREFIXES) or k in SKILLEVAL_ENV_KEYS})
        return env

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self._check(argv)
        exe = self.resolve(argv[0])
        timeout = max(0.1, min(float(timeout), max_timeout(argv)))
        graceful = argv[0] in GRACEFUL_BINARIES
        start = time.monotonic()
        proc = await asyncio.create_subprocess_exec(
            exe, *argv[1:], stdin=asyncio.subprocess.PIPE if stdin is not None else asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, cwd=cwd, env=self._env(argv[0]),
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
            await _stop(proc, graceful=graceful)
            out, err = b"", b""
        except asyncio.CancelledError:
            # The caller gave up (console shutdown, an action timeout): never leave the child's group running behind it.
            await _stop(proc, graceful=graceful)
            raise
        return CommandResult(
            returncode=proc.returncode if proc.returncode is not None else -1,
            stdout=redact(out.decode("utf-8", "replace")), stderr=redact(err.decode("utf-8", "replace")),
            timed_out=timed_out, duration_ms=int((time.monotonic() - start) * 1000))
