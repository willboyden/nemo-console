from __future__ import annotations

import json

import httpx
import pytest

from .conftest import FAKE_SECRET, Env

CHAT = "/api/v1/systems/switchyard/playground/chat"
URL = "http://127.0.0.1:4010/v1/chat/completions"
MSG = [{"role": "user", "content": "hello"}]


async def test_spec_and_no_playground_system(env: Env) -> None:
    env.adapters["relay"].playground = lambda: None  # type: ignore[method-assign]
    async with env.client() as c:
        assert (await c.get("/api/v1/systems/switchyard/playground")).json()["default_model"] == "m1"
        assert (await c.get("/api/v1/systems/relay/playground")).json() is None
        assert (await c.post("/api/v1/systems/relay/playground/chat", json={"model": "m", "messages": MSG})).status_code == 404


async def test_max_tokens_clamped_and_credential_from_env_name(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
    route = env.mock.post(URL).respond(200, json={"ok": True}, headers={"x-switchyard-route": "auto-local", "x-switchyard-tier": "capable"})
    async with env.client() as c:
        r = await c.post(CHAT, json={"model": "m1", "messages": MSG, "max_tokens": 999999, "stream": False})
        assert r.status_code == 200 and r.json() == {"ok": True}
        assert r.headers["x-served-by-route"] == "auto-local" and r.headers["x-served-by-tier"] == "capable"
        r = await c.post(CHAT, json={"model": "m1", "messages": MSG, "stream": False})
    sent = [json.loads(call.request.content) for call in route.calls]
    assert sent[0]["max_tokens"] == 4096 and sent[1]["max_tokens"] == 1024
    assert route.calls[0].request.headers["authorization"] == f"Bearer {FAKE_SECRET}"
    assert FAKE_SECRET not in json.dumps(sent)                           # never in the body


async def test_stream_passthrough(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
    sse = b'data: {"a":1}\n\ndata: {"leak":"' + FAKE_SECRET.encode() + b'"}\n\ndata: [DONE]\n\n'
    env.mock.post(URL).respond(200, content=sse, headers={"content-type": "text/event-stream"})
    async with env.client() as c:
        r = await c.post(CHAT, json={"model": "m1", "messages": MSG, "stream": True})
    assert r.status_code == 200 and r.headers["content-type"].startswith("text/event-stream")
    assert 'data: {"a":1}' in r.text and "[DONE]" in r.text and FAKE_SECRET not in r.text


async def test_missing_credential_is_503_naming_the_variable(env: Env) -> None:
    async with env.client() as c:
        r = await c.post(CHAT, json={"model": "m1", "messages": MSG})
    assert r.status_code == 503 and "LITELLM_API_KEY" in r.json()["error"]["message"]


async def test_fails_closed_never_direct(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
    env.mock.post(URL).mock(side_effect=httpx.ConnectError("refused"))
    async with env.client() as c:
        r = await c.post(CHAT, json={"model": "m1", "messages": MSG, "stream": False})
    assert r.status_code == 502 and r.json()["error"]["code"] == "upstream_unreachable"


async def test_request_validation(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
    async with env.client() as c:
        for body in ({"model": "bad model!", "messages": MSG}, {"model": "m1", "messages": []},
                     {"model": "m1", "messages": [{"role": "root", "content": "x"}]},
                     {"model": "m1", "messages": MSG, "max_tokens": 0}, {"model": "m1", "messages": MSG, "extra": 1}):
            assert (await c.post(CHAT, json=body)).status_code == 422, body


async def test_prompt_text_is_never_persisted_or_audited(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
    env.mock.post(URL).respond(200, json={"ok": True})
    async with env.client() as c:
        await c.post(CHAT, json={"model": "m1", "messages": [{"role": "user", "content": "PRIVATE-PROMPT-TEXT"}], "stream": False})
        audit = (await c.get("/api/v1/audit")).text
    assert "PRIVATE-PROMPT-TEXT" not in audit and "1 messages" in audit
    db = env.settings.state_dir
    for f in db.iterdir():
        if f.is_file():
            assert b"PRIVATE-PROMPT-TEXT" not in f.read_bytes()


async def test_viewer_cannot_chat(env: Env) -> None:
    _, key = env.container.auth.mint("viewer")
    async with env.client(client_host="10.0.0.2", token=key) as c:
        assert (await c.post(CHAT, json={"model": "m1", "messages": MSG})).status_code == 403


async def test_relay_playground_sends_the_key_named_for_relay(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    # 2026-10-17: the Relay gateway's /v1/* answers 401 without a bearer. The default names LITELLM_API_KEY for relay too,
    # resolved server-side like Switchyard's; a missing variable is a clear 503, never an anonymous upstream call.
    route = env.mock.post(URL).respond(200, json={"ok": True})
    async with env.client() as c:
        r = await c.post("/api/v1/systems/relay/playground/chat", json={"model": "m1", "messages": MSG, "stream": False})
        assert r.status_code == 503 and "LITELLM_API_KEY" in r.json()["error"]["message"] and not route.called
        monkeypatch.setenv("LITELLM_API_KEY", FAKE_SECRET)
        r = await c.post("/api/v1/systems/relay/playground/chat", json={"model": "m1", "messages": MSG, "stream": False})
    assert r.status_code == 200 and route.calls[0].request.headers["authorization"] == f"Bearer {FAKE_SECRET}"
