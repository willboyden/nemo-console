"""Security-audit fixes F1, F4, F5, F6a-e."""
from __future__ import annotations

import asyncio
import logging
from pathlib import Path

import httpx
import pytest

from nemo_console.config import Settings
from nemo_console.container import build_container
from nemo_console.main import create_app
from nemo_console.services.audit import DenialLog
from nemo_console.services.events import SseLimiter, event_stream

from .conftest import Env

URL = "/api/v1/systems/switchyard/config/routes"


def _client(app: object, host: str = "127.0.0.1", token: str | None = None) -> httpx.AsyncClient:
    h = {"X-Console": "1", **({"Authorization": f"Bearer {token}"} if token else {})}
    return httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=(host, 1)), base_url="http://127.0.0.1:8793", headers=h)  # type: ignore[arg-type]


def _app(env: Env, **kw: object) -> object:
    cfg = Settings(state_dir=env.tmp / f"s{len(kw)}{abs(hash(str(kw))) % 999}", frontend_dir=env.tmp / "front", background_tasks=False, **kw)  # type: ignore[arg-type]
    return create_app(cfg, build_container(cfg, adapters=dict(env.adapters), runner=env.runner, http=env.http))  # type: ignore[arg-type]


# F1 ---------------------------------------------------------------------------------------------
def test_loopback_trust_defaults_off() -> None:
    assert Settings().trust_loopback is False


async def test_default_is_public_role_and_needs_a_key(env: Env) -> None:
    app = _app(env)
    async with _client(app) as c:
        h = (await c.get("/api/v1/health")).json()
        assert h["role"] == "public" and h["trust"] == "bearer" and h["trust_notice"] is None
        assert (await c.get("/api/v1/systems")).status_code == 401
        assert (await c.put("/api/v1/settings", json={})).status_code == 401


@pytest.mark.parametrize("kw", [{"allowed_hosts": ["console.lan:8793"]}, {"host": "0.0.0.0"}])
async def test_loopback_trust_refused_when_exposure_plausible(env: Env, kw: dict[str, object], caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.WARNING)
    app = _app(env, trust_loopback=True, **kw)
    async with _client(app) as c:
        h = (await c.get("/api/v1/health", headers={"Host": "127.0.0.1:8793"})).json()
        assert h["trust"] == "bearer" and "loopback trust disabled" in h["trust_notice"]
        assert (await c.get("/api/v1/systems")).status_code == 401
    assert "loopback trust disabled" in caplog.text


async def test_explicit_opt_in_still_works_on_plain_loopback(env: Env) -> None:
    async with _client(_app(env, trust_loopback=True)) as c:
        assert (await c.get("/api/v1/health")).json()["role"] == "admin"


# F4 ---------------------------------------------------------------------------------------------
async def test_denied_requests_are_not_audit_rows_but_are_counted(env: Env) -> None:
    _, vk = env.container.auth.mint("viewer")
    _, ak = env.container.auth.mint("admin")
    async with env.client(client_host="10.0.0.9") as c:
        for _ in range(5):
            await c.put("/api/v1/settings", json={})                              # 401
        await c.put("/api/v1/settings", json={}, headers={"Host": "evil:1"})     # 421
    async with env.client(client_host="10.0.0.9", token=vk) as c:
        assert (await c.put("/api/v1/settings", json={})).status_code == 403     # viewer denied
    async with env.client(csrf=False) as c:
        await c.put("/api/v1/settings", json={})                                  # 403 csrf
    assert env.container.store.query("SELECT COUNT(*) n FROM audit")[0]["n"] == 0
    assert env.container.denials.total[401] == 5 and env.container.denials.total[421] == 1
    async with env.client(client_host="10.0.0.9", token=ak) as c:
        assert (await c.put("/api/v1/settings", json={})).status_code == 200      # authorised: one row
        rows = (await c.get("/api/v1/audit")).json()
        assert len(rows) == 1 and rows[0]["actor"].startswith("key:")
        assert 'nemo_console_denied_requests_total{status="401"} 5.0' in (await c.get("/metrics")).text


def test_denial_log_is_bounded_and_logs_one_line_per_interval(caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.WARNING)
    d = DenialLog(interval=0.0, ring=10)
    for _ in range(500):
        d.note(401, "PUT")
    assert len(d.ring) == 10
    d2 = DenialLog(interval=3600)
    caplog.clear()
    for _ in range(50):
        d2.note(403, "POST")
    assert not caplog.records                       # nothing per request


# F5 ---------------------------------------------------------------------------------------------
async def test_rollback_validates_and_scans_the_target_revision(env: Env) -> None:
    store = env.container.store
    async with env.client() as c:
        for rid, content, code in (("aa000001", "INVALID = 1\n", "validation_failed"),
                                   ("aa000002", 'api_key = "sk-abcdefghijklmnop1234"\n', "secret_in_content")):
            store.execute("INSERT INTO revisions(id, system, file_id, ts, actor, sha256, note, content) VALUES (?,?,?,?,?,?,?,?)",
                          (rid, "switchyard", "routes", 1.0, "t", "0" * 64, "", content.encode()))
            r = await c.post(f"{URL}/rollback", json={"revision_id": rid, "confirm": True})
            assert r.status_code == 422 and r.json()["error"]["code"] == code
            assert "abcdefghijklmnop" not in r.text
        store.execute("INSERT INTO revisions(id, system, file_id, ts, actor, sha256, note, content) VALUES (?,?,?,?,?,?,?,?)",
                      ("aa000003", "switchyard", "routes", 1.0, "t", "0" * 64, "", b"\xff\xfe"))
        assert (await c.post(f"{URL}/rollback", json={"revision_id": "aa000003", "confirm": True})).status_code == 422
    assert env.cfg_file.read_text() == "a = 1\n"


# F6 ---------------------------------------------------------------------------------------------
async def test_settings_state_dir_hidden_from_non_admins(env: Env, tmp_path: Path) -> None:
    home = Path.home()
    cfg = Settings(state_dir=home / ".local" / "share" / "nc-test", frontend_dir=env.tmp / "f", background_tasks=False)
    c = build_container(cfg, adapters=dict(env.adapters), runner=env.runner, http=env.http)  # type: ignore[arg-type]
    app = create_app(cfg, c)
    _, vk = c.auth.mint("viewer")
    _, ak = c.auth.mint("admin")
    async with _client(app, "10.0.0.9", vk) as cl:
        v = (await cl.get("/api/v1/settings")).json()
        assert v["state_dir"] == "~/.local/share/nc-test" and str(home) not in str(v)
    async with _client(app, "10.0.0.9", ak) as cl:
        assert (await cl.get("/api/v1/settings")).json()["state_dir"] == str(cfg.state_dir)
    _, vk2 = env.container.auth.mint("viewer")
    async with env.client(client_host="10.0.0.9", token=vk2) as cl:      # state dir outside $HOME
        assert (await cl.get("/api/v1/settings")).json()["state_dir"] == "<state dir>"


async def test_nul_in_static_path_is_404_not_500(env: Env) -> None:
    (env.settings.frontend_dir / "js").mkdir(parents=True)
    (env.settings.frontend_dir / "index.html").write_text("x")
    async with env.client(client_host="10.0.0.9") as c:
        for p in ("/js/a%00.js", "/%00", "/js/%00/x", "/index.html%00"):
            r = await c.get(p)
            assert r.status_code == 404, p
            assert "content-security-policy" in r.headers


async def test_playground_requests_ask_for_identity_encoding(env: Env, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LITELLM_API_KEY", "sk-FAKEFAKEFAKE1234567890abcdef")
    route = env.mock.post("http://127.0.0.1:4010/v1/chat/completions").respond(200, json={"ok": 1})
    async with env.client() as c:
        await c.post("/api/v1/systems/switchyard/playground/chat",
                     json={"model": "m1", "messages": [{"role": "user", "content": "x"}], "stream": False})
    assert route.calls[0].request.headers["accept-encoding"] == "identity"


async def test_parallel_validations_are_bounded(env: Env) -> None:
    running = peak = 0

    async def slow(file_id: str, content: str) -> object:
        nonlocal running, peak
        running += 1
        peak = max(peak, running)
        await asyncio.sleep(0.05)
        running -= 1
        from nemo_console.domain.models import ValidationResult
        return ValidationResult(ok=True, validator="v", authoritative=True)

    env.adapters["switchyard"].validate_config = slow  # type: ignore[assignment,method-assign]
    async with env.client() as c:
        rs = await asyncio.gather(*(c.post(f"{URL}/validate", json={"content": "a = 1"}) for _ in range(8)))
    assert all(r.status_code == 200 for r in rs) and peak <= 2


async def test_sse_slots_do_not_leak_on_aborted_streams(env: Env) -> None:
    env.adapters["switchyard"].follow_block = True
    app = env.app

    async def aborted(delay: float) -> None:
        scope = {"type": "http", "asgi": {"version": "3.0"}, "http_version": "1.1", "method": "GET", "root_path": "", "scheme": "http",
                 "path": "/api/v1/systems/switchyard/events/stream", "raw_path": b"", "query_string": b"",
                 "headers": [(b"host", b"127.0.0.1:8793")], "client": ("127.0.0.1", 1), "server": ("127.0.0.1", 8793)}

        async def receive() -> dict[str, str]:
            await asyncio.sleep(delay)
            return {"type": "http.disconnect"}

        async def send(_m: object) -> None:
            return None

        try:
            await asyncio.wait_for(app(scope, receive, send), 3)  # type: ignore[arg-type,misc]
        except Exception:  # noqa: BLE001, S110
            pass

    await asyncio.gather(*(aborted(0.0 if i % 2 else 0.02) for i in range(20)))
    await asyncio.sleep(0.1)
    assert env.container.sse.active == 0


async def test_generator_that_never_starts_holds_no_slot(env: Env) -> None:
    lim = SseLimiter(1)
    gen = event_stream(env.adapters["switchyard"], lim, 1, 1)
    del gen
    assert lim.active == 0
    a, b = SseLimiter(1), None
    assert a.acquire()
    chunks = [x async for x in event_stream(env.adapters["switchyard"], a, 1, 1)]      # over the cap: error event, no release
    assert b"too many" in chunks[0] and a.active == 1
    assert b is None


