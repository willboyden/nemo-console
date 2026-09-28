"""A fake secret must not appear in ANY response, audit row, log line or database file."""
from __future__ import annotations

import json
import logging

import pytest

from nemo_console.adapters.base import CommandResult
from nemo_console.services.common import KNOWN, find_secret, redact, redact_obj

from .conftest import FAKE_SECRET, Env


async def test_secret_never_leaks_anywhere(env: Env, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.DEBUG)
    monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
    KNOWN.add(FAKE_SECRET)
    seen: list[str] = []
    route = env.mock.post("http://127.0.0.1:4010/v1/chat/completions")
    route.respond(200, json={"echo": f"your key is {FAKE_SECRET}"})
    async with env.client() as c:
        r = await c.post("/api/v1/systems/switchyard/playground/chat",
                         json={"model": "m1", "messages": [{"role": "user", "content": f"my key is {FAKE_SECRET}"}], "stream": False})
        seen.append(r.text + str(r.headers))
        route.respond(401, text=f"bad key {FAKE_SECRET}")
        r = await c.post("/api/v1/systems/switchyard/playground/chat",
                         json={"model": "m1", "messages": [{"role": "user", "content": "hi"}], "stream": False})
        assert r.status_code == 502
        seen.append(r.text)
        for path in ("/api/v1/settings", "/api/v1/audit?limit=500", "/api/v1/overview", "/api/v1/systems/switchyard/status",
                     "/api/v1/systems/switchyard/events", "/api/v1/health"):
            seen.append((await c.get(path)).text)
        r = await c.put("/api/v1/settings", json={"playground_key_env": {"switchyard": FAKE_SECRET}})   # a VALUE, not a name
        assert r.status_code == 422
        seen.append(r.text)
        r = await c.put("/api/v1/settings", json={"urls": {"switchyard": f"http://user:{FAKE_SECRET}@127.0.0.1:4010"}})
        assert r.status_code == 422
        seen.append(r.text)
        seen.append((await c.get("/api/v1/audit?limit=500")).text)
    assert route.calls[0].request.headers["authorization"] == f"Bearer {FAKE_SECRET}"     # it WAS used upstream
    blob = "\n".join(seen) + caplog.text + json.dumps([r.getMessage() for r in caplog.records])
    assert FAKE_SECRET not in blob
    assert FAKE_SECRET.encode() not in (env.settings.state_dir / "console.db").read_bytes()
    wal = env.settings.state_dir / "console.db-wal"
    if wal.exists():
        assert FAKE_SECRET.encode() not in wal.read_bytes()


async def test_status_problem_text_is_redacted(env: Env) -> None:
    # the fake adapter puts 'Bearer abcdefgh12345678' in a Problem; adapters are supposed to redact, the core must too
    async with env.client() as c:
        r = await c.get("/api/v1/systems/relay/status")
    assert r.status_code == 200
    body = r.json()
    assert body["health"] == "up"
    assert "abcdefgh12345678" not in r.text and "leaked" in r.text


async def test_key_endpoint_returns_only_status_never_value(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
    async with env.client() as c:
        s = (await c.get("/api/v1/settings")).json()
    assert s["tokens"]["switchyard"] == f"[set, {len(FAKE_SECRET)} chars]"
    assert FAKE_SECRET not in json.dumps(s)
    monkeypatch.delenv("LITELLM_API_KEY")
    async with env.client() as c:
        assert (await c.get("/api/v1/settings")).json()["tokens"]["switchyard"] == "[unset]"


async def test_adapter_crash_message_is_redacted(env: Env) -> None:
    env.adapters["relay"].status_error = RuntimeError(f"connect failed with token {FAKE_SECRET}")
    KNOWN.add(FAKE_SECRET)
    env.container.status._cache.clear()
    async with env.client() as c:
        r = await c.get("/api/v1/systems/relay/status")
    assert r.status_code == 200 and r.json()["health"] == "unknown"
    assert FAKE_SECRET not in r.text
    assert r.json()["problems"][0]["code"] == "console.adapter.error"


def test_redact_patterns() -> None:
    text = ('Authorization: Bearer abcdefghijklmnop\napi_key = "supersecretvalue"\nhf_abcdefghijklmnopqrstuv '
            'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklm password: hunter2222 ok=1 max_tokens=4096')
    out = redact(text)
    for leaked in ("abcdefghijklmnop", "supersecretvalue", "hf_abc", "eyJhbGci", "hunter2222"):
        assert leaked not in out
    assert "max_tokens=4096" in out and "ok=1" in out
    obj = redact_obj({"api_key": "x", "nested": {"Authorization": "y", "max_tokens": 5, "note": "Bearer zzzzzzzzzzzz"}})
    assert obj["api_key"] == "[redacted]" and obj["nested"]["Authorization"] == "[redacted]"
    assert obj["nested"]["max_tokens"] == 5 and "zzzz" not in json.dumps(obj)


def test_find_secret_allows_env_names() -> None:
    assert find_secret('api_key_env = "LITELLM_MASTER_KEY"\nmodel = "qwen3.6-35b-a3b"\n') is None
    assert find_secret('api_key = "LITELLM_MASTER_KEY"') is None
    assert find_secret('api_key = "abcdefghijk"') is not None


async def test_runner_output_is_redacted_by_the_real_runner(tmp_path: pytest.TempPathFactory) -> None:
    from pathlib import Path

    from nemo_console.services.runner import SubprocessRunner
    d = Path(str(tmp_path)) / "bin"
    d.mkdir()
    (d / "docker").write_text(f"#!/bin/sh\necho 'env FOO={FAKE_SECRET} Bearer abcdefghijklmnop'\necho err-token=abcdefghijk1 >&2\n")
    (d / "docker").chmod(0o755)
    KNOWN.add(FAKE_SECRET)
    res = await SubprocessRunner([str(d)]).run(["docker", "ps"])
    assert isinstance(res, CommandResult) and res.returncode == 0
    assert FAKE_SECRET not in res.stdout and "abcdefghijklmnop" not in res.stdout and "abcdefghijk1" not in res.stderr




# ── redaction must not corrupt JSON it is asked to scrub ──────────────────────────────────────────
# `docker inspect` output goes through the runner's redact(). The KEY=VALUE pattern used to let an unquoted value
# START with a quote character, so for an env entry `"HF_TOKEN=",` the "value" it found was the JSON string's own
# closing quote: `"HF_TOKEN=[redacted],` -> invalid JSON. The Dynamo adapter inspects vllm-qwen38 (whose env has an
# empty HF_TOKEN=) in the same call as its own containers, so every Dynamo container then read as "absent".
def _inspect_like(env_entries: list[str]) -> str:
    return json.dumps([{"Name": "/c", "Config": {"Env": env_entries}}], indent=4)


@pytest.mark.parametrize("entry", [
    "HF_TOKEN=",                                    # empty value: nothing to hide, must survive intact
    "LITELLM_API_KEY=",
    "HF_TOKEN=hf_abcdefghijklmnopqrst",             # prefixed secret inside a JSON string
    "SOME_PASSWORD=hunter2hunter2",                 # plain secret inside a JSON string
    "MAX_JOBS=4",                                   # not a secret at all
])
def test_redaction_keeps_docker_inspect_json_parseable(entry: str) -> None:
    out = redact(_inspect_like([entry, "TORCH_CUDA_ARCH_LIST=12.0"]))
    doc = json.loads(out)                           # the point: it must still be JSON
    env = doc[0]["Config"]["Env"]
    assert env[-1] == "TORCH_CUDA_ARCH_LIST=12.0"   # neighbours untouched
    assert len(env) == 2


def test_redaction_still_hides_secret_values_inside_json() -> None:
    out = redact(_inspect_like(["HF_TOKEN=hf_abcdefghijklmnopqrst", "SOME_PASSWORD=hunter2hunter2"]))
    assert "hf_abcdefghijklmnopqrst" not in out and "hunter2hunter2" not in out
    assert out.count("[redacted]") >= 2


@pytest.mark.parametrize("text", [
    'token="abcdefgh1234"',                         # quoted
    "token='abcdefgh1234'",
    'token="abcdefgh1234',                          # UNTERMINATED quote: must still be hidden, not skipped
    "token=abcdefgh1234",                           # bare
    "api_key: abcdefgh1234",
    'password=ab"cdefgh1234',                       # a quote INSIDE the secret must not cut the redaction short
])
def test_redaction_still_hides_non_json_forms(text: str) -> None:
    out = redact(text)
    assert "abcdefgh1234" not in out and "cdefgh1234" not in out, out
