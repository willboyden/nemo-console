from __future__ import annotations

import os
from pathlib import Path

import httpx

from nemo_console.config import user_env_file

from .conftest import Env

HEADERS = ("content-security-policy", "x-content-type-options", "referrer-policy", "x-frame-options", "permissions-policy")


def assert_secure(r: httpx.Response) -> None:
    for h in HEADERS:
        assert h in r.headers, f"missing {h} on {r.status_code}"
    assert "frame-ancestors 'none'" in r.headers["content-security-policy"]
    assert r.headers["x-content-type-options"] == "nosniff"


async def test_user_env_file_is_disabled_in_tests() -> None:
    assert user_env_file() is None
    assert os.environ["HOME"].endswith("home")


async def test_bad_host_is_421(env: Env) -> None:
    async with env.client(headers={"Host": "evil.example:8793"}) as c:
        r = await c.get("/api/v1/health")
    assert r.status_code == 421
    assert r.json()["error"]["code"] == "misdirected_request"
    assert_secure(r)


async def test_cross_origin_post_is_403(env: Env) -> None:
    async with env.client(headers={"Origin": "http://evil.example"}) as c:
        r = await c.put("/api/v1/settings", json={})
    assert r.status_code == 403 and r.json()["error"]["code"] == "cross_origin"
    assert_secure(r)
    async with env.client(headers={"Origin": "null"}) as c:
        assert (await c.put("/api/v1/settings", json={})).status_code == 403


async def test_same_origin_is_allowed(env: Env) -> None:
    async with env.client(headers={"Origin": "http://127.0.0.1:8793"}) as c:
        assert (await c.put("/api/v1/settings", json={})).status_code == 200


async def test_missing_csrf_header_is_403_even_with_bearer(env: Env) -> None:
    async with env.client(csrf=False) as c:
        r = await c.put("/api/v1/settings", json={})
    assert r.status_code == 403 and r.json()["error"]["code"] == "csrf_header_required"
    _, key = env.container.auth.mint("admin")
    async with env.client(csrf=False, token=key) as c:
        assert (await c.put("/api/v1/settings", json={})).status_code == 403


async def test_get_needs_no_csrf_header(env: Env) -> None:
    async with env.client(csrf=False) as c:
        assert (await c.get("/api/v1/systems")).status_code == 200


async def test_headers_on_401_and_404_and_405(env: Env) -> None:
    async with env.client(client_host="10.1.2.3") as c:
        r401 = await c.get("/api/v1/systems")
    assert r401.status_code == 401 and r401.json()["error"]["code"] == "unauthorized"
    assert_secure(r401)
    assert r401.headers["cache-control"] == "no-store"
    async with env.client() as c:
        r404 = await c.get("/api/v1/systems/nope/status")
        r404b = await c.get("/api/v1/definitely-not-a-route")
        r405 = await c.post("/api/v1/systems")
    assert r404.status_code == 404 and r404b.status_code == 404 and r405.status_code == 405
    for r in (r404, r404b, r405):
        assert_secure(r)
        assert "error" in r.json()


async def test_headers_on_validation_errors_and_500(env: Env) -> None:
    async with env.client() as c:
        r = await c.post("/api/v1/systems/switchyard/config/routes/validate", json={"content": 5})
    assert r.status_code == 422
    assert_secure(r)
    env.adapters["switchyard"].status_error = None

    async def boom(*_a: object, **_k: object) -> None:
        raise RuntimeError("boom secret sk-FAKEFAKEFAKE1234567890")

    env.container.topology.get = boom  # type: ignore[method-assign]
    async with env.client() as c:
        r = await c.get("/api/v1/topology")
    assert r.status_code == 500 and "sk-" not in r.text
    assert_secure(r)


async def test_no_swagger_or_redoc(env: Env) -> None:
    async with env.client() as c:
        for p in ("/docs", "/redoc", "/docs/oauth2-redirect"):
            r = await c.get(p)
            assert r.status_code == 404
            assert_secure(r)


async def test_x_forwarded_for_disables_loopback_trust(env: Env) -> None:
    async with env.client() as c:
        assert (await c.get("/api/v1/systems")).status_code == 200
        assert (await c.get("/api/v1/health")).json()["trust"] == "loopback-admin"
    async with env.client(headers={"X-Forwarded-For": "8.8.8.8"}) as c:
        assert (await c.get("/api/v1/systems")).status_code == 401
        assert (await c.get("/api/v1/health")).json()["trust"] == "bearer"


async def test_bearer_keys_roles_and_wrong_key(env: Env) -> None:
    _, viewer_key = env.container.auth.mint("viewer")
    _, admin_key = env.container.auth.mint("admin")
    async with env.client(client_host="10.0.0.9", token=viewer_key) as c:
        assert (await c.get("/api/v1/systems")).status_code == 200
        r = await c.put("/api/v1/settings", json={})
        assert r.status_code == 403 and r.json()["error"]["code"] == "forbidden"
        assert (await c.get("/api/v1/audit")).status_code == 403
        assert (await c.get("/api/v1/health")).json()["trust"] == "bearer"
    async with env.client(client_host="10.0.0.9", token=admin_key) as c:
        assert (await c.put("/api/v1/settings", json={})).status_code == 200
        assert (await c.get("/api/v1/audit")).status_code == 200
    async with env.client(client_host="10.0.0.9", token="nck_wrong") as c:
        assert (await c.get("/api/v1/systems")).status_code == 401
    # a bad bearer must not silently fall back to loopback trust
    async with env.client(token="nck_wrong") as c:
        assert (await c.get("/api/v1/systems")).status_code == 401


async def test_keys_are_stored_hashed_and_bootstrap_file_is_0600(env: Env) -> None:
    path = env.container.auth.ensure_bootstrap_key()
    assert path is not None and (path.stat().st_mode & 0o777) == 0o600
    key = path.read_text().strip()
    assert env.container.auth.authenticate(key) is not None
    db = (env.settings.state_dir / "console.db").read_bytes()
    assert key.encode() not in db
    assert env.container.auth.ensure_bootstrap_key() is None      # only once
    assert (env.settings.state_dir.stat().st_mode & 0o777) == 0o700
    assert ((env.settings.state_dir / "console.db").stat().st_mode & 0o777) == 0o600


async def test_body_cap_content_length_and_chunked(env: Env) -> None:
    async with env.client() as c:
        r = await c.put("/api/v1/settings", content=b"{" + b" " * (1024 * 1024 + 10) + b"}",
                        headers={"content-type": "application/json"})
        assert r.status_code == 413
        assert_secure(r)

        async def gen() -> object:
            for _ in range(20):
                yield b" " * 100_000

        r = await c.put("/api/v1/settings", content=gen(), headers={"content-type": "application/json"})  # type: ignore[arg-type]
        assert r.status_code == 413
        assert_secure(r)


async def test_static_allowlist_and_traversal(env: Env, tmp_path: Path) -> None:
    front = env.settings.frontend_dir
    (front / "js").mkdir(parents=True)
    (front / "dev").mkdir()
    (front / "index.html").write_text("<html>ok</html>")
    (front / "js" / "app.js").write_text("x")
    (front / "dev" / "mock.js").write_text("secret-dev")
    (front / "package.json").write_text("{}")
    outside = tmp_path / "outside.txt"
    outside.write_text("outside")
    (front / "js" / "link.js").symlink_to(outside)
    async with env.client(client_host="10.0.0.9") as c:       # static shell needs no auth
        for path, code in (("/", 200), ("/index.html", 200), ("/js/app.js", 200), ("/dev/mock.js", 404),
                           ("/package.json", 404), ("/docs", 404), ("/redoc", 404), ("/js/link.js", 404)):   # NB httpx normalises '..' itself; raw-path traversal: test_audit2.py
            r = await c.get(path)
            assert r.status_code == code, path
            assert_secure(r)
        assert "secret-dev" not in (await c.get("/dev/mock.js")).text


async def test_static_files_are_revalidated_on_every_load(env: Env) -> None:
    # Without Cache-Control the browser kept 0.2 modules for days and the five 0.3.0 views failed their imports.
    front = env.settings.frontend_dir
    (front / "js").mkdir(parents=True)
    (front / "index.html").write_text("<html>ok</html>")
    (front / "js" / "format.js").write_text("export const a = 1;")
    async with env.client() as c:
        for path in ("/", "/index.html", "/js/format.js"):
            r = await c.get(path)
            assert r.status_code == 200, path
            assert r.headers["cache-control"] == "no-cache", path
            etag = r.headers["etag"]
            again = await c.get(path, headers={"If-None-Match": f'W/{etag}, "other"'})
            assert again.status_code == 304, path
            assert again.content == b""
            assert again.headers["etag"] == etag and again.headers["cache-control"] == "no-cache"
            assert_secure(again)
        (front / "js" / "format.js").write_text("export const a = 1, fmtUnit = 2;")   # changed: new size -> new etag
        r = await c.get("/js/format.js", headers={"If-None-Match": etag})
        assert r.status_code == 200 and "fmtUnit" in r.text
        assert (await c.get("/api/v1/health")).headers["cache-control"] == "no-store"   # API stays uncached
