"""Second independent audit: F1 audit JSON validity, F2 quadratic regex, F3 raw-path traversal, F6, F7a, F7d."""
from __future__ import annotations

import asyncio
import json
import time
from typing import Any

import pytest

from nemo_console.services.common import find_secret, redact

from .conftest import Env


# F1 ---------------------------------------------------------------------------------------------
async def test_token_key_in_body_does_not_poison_audit(env: Env) -> None:
    _, vk = env.container.auth.mint("viewer")
    _, ak = env.container.auth.mint("admin")
    big = {f"k{i}": "x" * 100 for i in range(100)}
    async with env.client(client_host="10.0.0.9", token=ak) as c:
        await c.post("/api/v1/systems/switchyard/actions/nosuch", json={"token": "x", "api_key": "y", "password": "z"})
        await c.post("/api/v1/systems/switchyard/actions/nosuch", json=big)
        r = await c.get("/api/v1/audit?limit=500")
        assert r.status_code == 200, r.text
        rows = r.json()
        assert len(rows) == 2 and all(isinstance(x["params"], dict) for x in rows)
        assert rows[1]["params"]["token"] == "[redacted]"
    async with env.client(client_host="10.0.0.9", token=vk) as c:      # viewer can trigger it too, but is a denied count
        await c.post("/api/v1/systems/switchyard/actions/nosuch", json={"token": "x"})
    async with env.client(client_host="10.0.0.9", token=ak) as c:
        assert (await c.get("/api/v1/audit")).status_code == 200


async def test_poisoned_row_does_not_break_list(env: Env) -> None:
    env.container.store.execute(
        "INSERT INTO audit(ts, actor, role, method, path, system, status, params) VALUES (?,?,?,?,?,?,?,?)",
        (1.0, "a", "admin", "POST", "/api/v1/x", None, 200, '{"token": [redacted]'))
    async with env.client() as c:
        r = await c.get("/api/v1/audit")
    assert r.status_code == 200 and r.json()[0]["params"] == {"_unparseable": True}


# F2 ---------------------------------------------------------------------------------------------
def test_redact_and_find_secret_are_not_quadratic() -> None:
    for text in ("a_" * 32_768, "a" * 65_536, "token=" + "a_" * 40_000, "-----BEGIN PRIVATE KEY-----" + "x" * 65_536):
        t0 = time.perf_counter()
        redact(text)
        find_secret(text)
        assert time.perf_counter() - t0 < 0.5, text[:12]


async def test_audit_write_of_pathological_body_is_fast(env: Env) -> None:
    _, ak = env.container.auth.mint("admin")
    body = {"v": "a_" * 30_000}
    t0 = time.perf_counter()
    async with env.client(client_host="10.0.0.9", token=ak) as c:
        await c.post("/api/v1/systems/switchyard/actions/nosuch", json=body)
    assert time.perf_counter() - t0 < 1.0


# F3 ---------------------------------------------------------------------------------------------
async def _raw_get(app: Any, raw_path: str) -> tuple[int, bytes]:
    from urllib.parse import unquote
    scope = {"type": "http", "asgi": {"version": "3.0"}, "http_version": "1.1", "method": "GET", "root_path": "", "scheme": "http",
             "path": unquote(raw_path), "raw_path": raw_path.encode(), "query_string": b"",
             "headers": [(b"host", b"127.0.0.1:8793")], "client": ("10.0.0.9", 1), "server": ("127.0.0.1", 8793)}
    status = 0
    body = b""

    async def receive() -> dict[str, Any]:
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(m: dict[str, Any]) -> None:
        nonlocal status, body
        if m["type"] == "http.response.start":
            status = m["status"]
        elif m["type"] == "http.response.body":
            body += m.get("body", b"")

    await app(scope, receive, send)
    return status, body


async def test_static_rejects_dotdot_before_resolving(env: Env) -> None:
    front = env.settings.frontend_dir
    for d in ("css", "js", "dev", "tests"):
        (front / d).mkdir(parents=True, exist_ok=True)
    (front / "index.html").write_text("shell")
    (front / "css" / "a.css").write_text("ok")
    (front / "dev" / "mock-server.mjs").write_text("DEV-SECRET")
    (front / "package.json").write_text("PKG-SECRET")
    (front / "tests" / "api.test.mjs").write_text("TEST-SECRET")
    assert (await _raw_get(env.app, "/css/a.css"))[0] == 200
    for raw in ("/css/../dev/mock-server.mjs", "/js/../package.json", "/js/../tests/api.test.mjs", "/css/%2e%2e/dev/mock-server.mjs",
                "/css/..%2fdev/mock-server.mjs", "/css/%2E%2E%2Fdev/mock-server.mjs", "/css//a.css", "/css\\..\\dev\\mock-server.mjs",
                "/css/%252e%252e/dev/mock-server.mjs", "/css/./a.css"):
        status, body = await _raw_get(env.app, raw)
        assert status == 404, raw
        assert b"SECRET" not in body, raw


# F6 ---------------------------------------------------------------------------------------------
async def test_viewer_gets_redacted_config_admin_gets_raw(env: Env) -> None:
    env.cfg_file.write_text('api_key = "literalsecretvalue123"\nname = "ok"\n')
    _, vk = env.container.auth.mint("viewer")
    _, ak = env.container.auth.mint("admin")
    url = "/api/v1/systems/switchyard/config/routes"
    async with env.client(client_host="10.0.0.9", token=vk) as c:
        v = (await c.get(url)).json()
    assert "literalsecretvalue123" not in json.dumps(v) and 'name = "ok"' in v["content"]
    async with env.client(client_host="10.0.0.9", token=ak) as c:
        a = (await c.get(url)).json()
    assert "literalsecretvalue123" in a["content"]
    assert v["sha256"] == a["sha256"]                    # CAS hash still of the raw file


# F7 ---------------------------------------------------------------------------------------------
async def test_playground_key_env_must_be_allowlisted(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SOME_PRIVATE_THING", "topsecretvalue")
    async with env.client() as c:
        for bad in ("SOME_PRIVATE_THING", "PATH", "HOME", "AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN"):
            r = await c.put("/api/v1/settings", json={"playground_key_env": {"switchyard": bad}})
            assert r.status_code == 422, bad
        for good in ("LITELLM_MASTER_KEY", "NEMO_CONSOLE_PLAYGROUND_X", "MY_SERVICE_API_KEY"):
            r = await c.put("/api/v1/settings", json={"playground_key_env": {"switchyard": good}})
            assert r.status_code == 200, good
    assert "topsecretvalue" not in json.dumps(await _settings(env))


async def _settings(env: Env) -> dict[str, Any]:
    async with env.client() as c:
        return (await c.get("/api/v1/settings")).json()  # type: ignore[no-any-return]


async def test_env_supplied_disallowed_key_name_is_ignored(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("PATH_LEN_PROBE", "x")
    env.container.settings.store.execute(
        "INSERT INTO settings(k, v) VALUES ('overrides', ?)", (json.dumps({"playground_key_env": {"switchyard": "HOME"}}),))
    assert env.container.settings.effective_key_env()["switchyard"] == "LITELLM_API_KEY"


async def test_url_userinfo_redacted_in_audit(env: Env) -> None:
    async with env.client() as c:
        await c.put("/api/v1/settings", json={"urls": {"dynamo": "http://user:pw123456@127.0.0.1:1"}})
        await c.post("/api/v1/systems/switchyard/actions/nosuch", json={"note": "see https://bob:hunter22@example.com/x and ok"})
        rows = (await c.get("/api/v1/audit")).text
    assert "pw123456" not in rows and "hunter22" not in rows and "bob" not in rows


# rollback shares the validate semaphore ---------------------------------------------------------
async def test_rollback_validation_takes_the_semaphore(env: Env) -> None:
    await env.container.validate_sem.acquire()
    await env.container.validate_sem.acquire()
    env.container.store.execute("INSERT INTO revisions(id, system, file_id, ts, actor, sha256, note, content) VALUES (?,?,?,?,?,?,?,?)",
                                ("bb000001", "switchyard", "routes", 1.0, "t", "0" * 64, "", b"a = 2\n"))
    async with env.client() as c:
        t = asyncio.create_task(c.post("/api/v1/systems/switchyard/config/routes/rollback", json={"revision_id": "bb000001", "confirm": True}))
        await asyncio.sleep(0.2)
        assert not t.done()
        env.container.validate_sem.release()
        env.container.validate_sem.release()
        assert (await asyncio.wait_for(t, 3)).status_code == 200
