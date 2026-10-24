"""The lab wrapper's flag ALLOWLIST (clients/aiperf/aiperf-local), run against a stub `aiperf` and a throwaway HOME.
Skipped when the console repo is checked out without the lab repo around it. Nothing here touches the real router."""
from __future__ import annotations

import http.server
import os
import shutil
import subprocess
import threading
from collections.abc import Iterator
from pathlib import Path

import pytest

WRAPPER_SRC = Path(__file__).resolve().parents[4] / "aiperf" / "aiperf-local"
pytestmark = pytest.mark.skipif(not WRAPPER_SRC.is_file(), reason="lab repo (clients/aiperf) not present")


class _Live(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *a: object) -> None:
        pass


@pytest.fixture
def env(tmp_path: Path) -> Iterator[dict[str, str]]:
    lab = tmp_path / "lab" / "clients" / "aiperf"
    (lab / ".venv" / "bin").mkdir(parents=True)
    shutil.copy(WRAPPER_SRC, lab / "aiperf-local")
    shutil.copy(WRAPPER_SRC.parent / "auth-skeleton.yaml", lab / "auth-skeleton.yaml")
    stub = lab / ".venv" / "bin" / "aiperf"
    stub.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$STUB_OUT"\nprintf "%s" "$AIPERF_ROUTER_KEY" > "$STUB_OUT.key"\n')
    stub.chmod(0o755)
    (lab / ".tiktoken-cache").mkdir()
    (lab / ".tiktoken-cache" / "x").write_text("x")
    home = tmp_path / "home"
    (home / ".config" / "aiperf").mkdir(parents=True)
    key = home / ".config" / "aiperf" / "litellm.key"
    key.write_text("sk-FAKE-not-a-real-key-0123456789\n")
    key.chmod(0o600)
    srv = http.server.HTTPServer(("127.0.0.1", 0), _Live)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield {"HOME": str(home), "PATH": os.environ["PATH"], "LITELLM_URL": f"http://127.0.0.1:{srv.server_port}",
           "STUB_OUT": str(tmp_path / "stub.out"), "WRAPPER": str(lab / "aiperf-local"), "ART": str(tmp_path / "art")}
    srv.shutdown()


def run(env: dict[str, str], *args: str) -> subprocess.CompletedProcess[str]:
    e = {k: v for k, v in env.items() if k not in ("WRAPPER", "ART")}
    return subprocess.run([env["WRAPPER"], "profile", *args], env=e, capture_output=True, text=True, timeout=30, check=False)


GOOD = ["--model", "aitop/nemotron-lightning", "--endpoint-type", "chat", "--streaming", "--concurrency", "2",
        "--request-count", "8", "--prompt-input-tokens-mean", "128", "--prompt-output-tokens-mean", "64",
        "--warmup-request-count", "1", "--request-timeout-seconds", "120", "--random-seed", "42",
        "--export-level", "records", "--use-server-token-count"]


def test_every_flag_the_console_renders_is_accepted(env: dict[str, str]) -> None:
    r = run(env, *GOOD, "--artifact-dir", env["ART"])
    assert r.returncode == 0, r.stderr
    got = Path(env["STUB_OUT"]).read_text().split("\n")
    assert got[0] == "profile" and "--config" in got and "--no-gpu-telemetry" in got and "--no-server-metrics" in got
    assert got[got.index("--model") + 1] == "aitop/nemotron-lightning" and "--use-server-token-count" in got
    assert Path(env["STUB_OUT"] + ".key").read_text() == "sk-FAKE-not-a-real-key-0123456789"     # env only
    assert "sk-FAKE" not in "\n".join(got)                                                        # never argv


def test_equals_forms_are_accepted(env: dict[str, str]) -> None:
    r = run(env, "--model=m", "--concurrency=1", f"--artifact-dir={env['ART']}")
    assert r.returncode == 0, r.stderr


@pytest.mark.parametrize("flag", [
    ["--api-key", "x"], ["--api-k", "x"], ["--api", "x"], ["--conc", "2"], ["--concurr=2"], ["--model-n", "x"],
    ["--header", "a:b"], ["-H", "a:b"], ["-Ha:b"], ["--config", "/etc/passwd"], ["-f", "x"],
    ["--public-dataset", "sharegpt"], ["--input-file", "/etc/passwd"], ["--otel-url", "http://x"],
    ["--mlflow-tracking-uri=http://x"], ["--wandb-project", "p"], ["--gpu-telemetry"], ["--server-metrics", "http://x"],
    ["--ui", "dashboard"], ["--auto-plot"], ["--tokenizer-trust-remote-code"], ["--api-port", "9"], ["--zmq-host", "x"],
    ["--some-future-flag", "1"], ["stray-positional"], ["--streaming=false"], ["-u"], ["--model"],
])
def test_everything_else_is_refused_before_any_run(env: dict[str, str], flag: list[str]) -> None:
    r = run(env, *flag, "--artifact-dir", env["ART"])
    assert r.returncode == 2, (flag, r.stderr)
    assert not Path(env["STUB_OUT"]).exists() and not Path(env["ART"]).exists()


@pytest.mark.parametrize("url", ["http://evil.example:4000", "http://127.0.0.1:4000@evil.example", "https://127.0.0.1:4000",
                                 "http://127.0.0.1:4000/v1", "http://0.0.0.0:4000"])
def test_non_loopback_urls_are_refused(env: dict[str, str], url: str) -> None:
    for spelled in (["--url", url], [f"--url={url}"], ["-u", url]):
        r = run(env, *spelled, "--artifact-dir", env["ART"])
        assert r.returncode == 2 and "non-loopback" in r.stderr and not Path(env["STUB_OUT"]).exists()


def test_tokenizer_must_be_builtin_or_a_local_dir(env: dict[str, str], tmp_path: Path) -> None:
    assert run(env, "--tokenizer", "meta-llama/Llama-3", "--artifact-dir", env["ART"]).returncode == 2
    assert run(env, "--tokenizer", "builtin", "--artifact-dir", env["ART"]).returncode == 0


def test_artifact_dir_is_required_and_the_key_file_too(env: dict[str, str]) -> None:
    assert run(env, "--model", "m").returncode == 2
    Path(env["HOME"], ".config", "aiperf", "litellm.key").unlink()
    r = run(env, "--model", "m", "--artifact-dir", env["ART"])
    assert r.returncode == 3 and "mint-key.sh" in r.stderr
