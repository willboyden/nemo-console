from __future__ import annotations

import os
import stat

import pytest

from .conftest import Env, apply_body

URL = "/api/v1/systems/switchyard/config/routes"


async def _apply(env: Env, content: str, base: str | None = None, **kw: object) -> tuple[int, dict]:  # type: ignore[type-arg]
    async with env.client() as c:
        r = await c.post(f"{URL}/apply", json=apply_body(content, base or env.sha(), **kw))
    return r.status_code, r.json()


async def test_apply_happy_path_backs_up_and_writes_atomically(env: Env) -> None:
    old = env.cfg_file.read_text()
    code, body = await _apply(env, "a = 2\n")
    assert code == 200, body
    assert body["ok"] and body["post_check"] == "healthy" and body["restarted"] and not body["rolled_back"]
    assert env.cfg_file.read_text() == "a = 2\n"
    assert stat.S_IMODE(env.cfg_file.stat().st_mode) == 0o640              # mode preserved
    assert not [p for p in env.cfg_file.parent.iterdir() if p.name != "routes.toml"]   # no temp litter
    assert body["new_sha256"] == env.sha()
    assert env.adapters["switchyard"].post_calls == [True]
    async with env.client() as c:
        hist = (await c.get(f"{URL}/history")).json()
    assert len(hist) == 1 and hist[0]["id"] == body["revision_id"]
    rows = env.container.store.query("SELECT content FROM revisions WHERE id=?", (body["revision_id"],))
    assert bytes(rows[0]["content"]).decode() == old


async def test_get_config_sha_is_of_raw_file_bytes(env: Env) -> None:
    async with env.client() as c:
        r = await c.get(URL)
    assert r.status_code == 200 and r.json()["sha256"] == env.sha()


async def test_stale_base_sha_is_409_and_file_untouched(env: Env) -> None:
    code, body = await _apply(env, "a = 2\n", base="0" * 64)
    assert code == 409 and body["error"]["code"] == "stale_base"
    assert env.cfg_file.read_text() == "a = 1\n"
    assert env.adapters["switchyard"].post_calls == []


async def test_concurrent_edit_between_load_and_apply_is_409(env: Env) -> None:
    base = env.sha()
    env.cfg_file.write_text("a = 99\n")          # someone else edits
    code, _ = await _apply(env, "a = 2\n", base=base)
    assert code == 409


async def test_confirm_is_required(env: Env) -> None:
    code, body = await _apply(env, "a = 2\n", confirm=False)
    assert code == 400 and body["error"]["code"] == "confirm_required"
    assert env.cfg_file.read_text() == "a = 1\n"


async def test_bad_sha_format_is_422(env: Env) -> None:
    code, _ = await _apply(env, "a = 2\n", base="XYZ")
    assert code == 422


async def test_secret_looking_content_is_refused(env: Env) -> None:
    for bad in ('api_key = "sk-abcdefghijklmnop1234"\n', 'h = "Authorization: Bearer abcdefghijklmnopqrstuv"\n',
                'api_key = "literalsecretvalue"\n', 'token: hunter2hunter2\n', 'k = "hf_abcdefghijklmnopqrstuvwx"\n',
                "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----\n"):
        code, body = await _apply(env, bad)
        assert code == 422 and body["error"]["code"] == "secret_in_content", bad
        assert "abcdefghijklmnop" not in str(body) and "hunter2" not in str(body)
    assert env.cfg_file.read_text() == "a = 1\n"
    async with env.client() as c:            # validate never accepts it either
        r = await c.post(f"{URL}/validate", json={"content": 'api_key = "sk-abcdefghijklmnop1234"'})
    assert r.status_code == 422


async def test_env_var_names_are_fine(env: Env) -> None:
    good = 'api_key_env = "LITELLM_MASTER_KEY"\napi_key = "LITELLM_MASTER_KEY"\nmax_tokens = 4096\nkey: os.environ/LITELLM_KEY\n'
    code, _ = await _apply(env, good)
    assert code == 200


async def test_real_repo_route_files_pass_the_secret_scan() -> None:
    from pathlib import Path

    from nemo_console.services.common import find_secret
    root = next((p for p in Path(__file__).resolve().parents if (p / "inference" / "switchyard").is_dir()), None)
    if root is None:
        pytest.skip("not inside a lab repository checkout")
    for rel in ("inference/switchyard/routes.toml", "clients/nemo-relay/routes.toml"):
        f = root / rel
        if f.exists():
            assert find_secret(f.read_text()) is None, rel


async def test_symlink_is_refused(env: Env) -> None:
    real = env.cfg_file.parent / "real.toml"
    real.write_text("a = 1\n")
    env.cfg_file.unlink()
    env.cfg_file.symlink_to(real)
    code, body = await _apply(env, "a = 2\n", base="0" * 64)
    assert code == 403 and body["error"]["code"] == "symlink_refused"
    assert real.read_text() == "a = 1\n"


async def test_parent_dir_swapped_for_symlink_is_refused(env: Env) -> None:
    elsewhere = env.tmp / "elsewhere"
    elsewhere.mkdir()
    (elsewhere / "routes.toml").write_text("a = 1\n")
    moved = env.tmp / "cfg_moved"
    env.cfg_file.parent.rename(moved)
    (env.tmp / "cfg").symlink_to(elsewhere)
    code, body = await _apply(env, "a = 2\n", base="0" * 64)
    assert code == 403 and body["error"]["code"] == "path_changed"
    assert (elsewhere / "routes.toml").read_text() == "a = 1\n"


async def test_size_cap(env: Env) -> None:
    code, body = await _apply(env, "a = '" + "x" * 300_000 + "'\n")
    assert code == 413 and body["error"]["code"] == "config_too_large"


async def test_validation_failure_blocks_write(env: Env) -> None:
    code, body = await _apply(env, "INVALID = 1\n")
    assert code == 422 and body["error"]["code"] == "validation_failed"
    assert env.cfg_file.read_text() == "a = 1\n" and env.adapters["switchyard"].post_calls == []


async def test_read_only_file_is_403(env: Env) -> None:
    env.adapters["switchyard"].editable = False
    code, body = await _apply(env, "a = 2\n")
    assert code == 403 and body["error"]["code"] == "read_only"
    assert "policy file" in body["error"]["message"]


async def test_unknown_file_is_404(env: Env) -> None:
    async with env.client() as c:
        assert (await c.get("/api/v1/systems/switchyard/config/nope")).status_code == 404
        assert (await c.get("/api/v1/systems/switchyard/config/..%2f..%2fetc")).status_code == 404
        assert (await c.get("/api/v1/systems/relay/config/routes")).status_code == 404


async def test_auto_rollback_when_unhealthy(env: Env) -> None:
    code, body = await _apply(env, "a = 'BAD'\n")
    assert code == 200
    assert body["ok"] is False and body["rolled_back"] is True and body["post_check"] == "unhealthy"
    assert env.cfg_file.read_text() == "a = 1\n"                          # previous file restored
    assert body["new_sha256"] == env.sha()
    assert env.adapters["switchyard"].post_calls == [True, True]           # restarted again after the restore
    assert "restored the previous file" in body["detail"]


async def test_auto_rollback_when_post_apply_raises(env: Env) -> None:
    env.adapters["switchyard"].post_health_override = "raise"
    code, body = await _apply(env, "a = 3\n")
    assert code == 200 and body["rolled_back"] is True and env.cfg_file.read_text() == "a = 1\n"


async def test_manual_rollback_to_a_revision(env: Env) -> None:
    _, first = await _apply(env, "a = 2\n")
    async with env.client() as c:
        r = await c.post(f"{URL}/rollback", json={"revision_id": first["revision_id"], "confirm": False})
        assert r.status_code == 400
        r = await c.post(f"{URL}/rollback", json={"revision_id": "deadbeefdeadbeef", "confirm": True})
        assert r.status_code == 404
        r = await c.post(f"{URL}/rollback", json={"revision_id": first["revision_id"], "confirm": True})
    assert r.status_code == 200 and r.json()["ok"]
    assert env.cfg_file.read_text() == "a = 1\n"
    async with env.client() as c:
        assert len((await c.get(f"{URL}/history")).json()) == 2


async def test_history_is_pruned(env: Env) -> None:
    env.container.config_store.keep = 3
    for i in range(6):
        code, _ = await _apply(env, f"a = {i + 10}\n")
        assert code == 200
    async with env.client() as c:
        assert len((await c.get(f"{URL}/history")).json()) == 3


async def test_file_written_with_expected_perms_after_umask(env: Env) -> None:
    old = os.umask(0o077)
    try:
        await _apply(env, "a = 5\n")
    finally:
        os.umask(old)
    assert stat.S_IMODE(env.cfg_file.stat().st_mode) == 0o640


async def test_apply_is_audited_without_content(env: Env) -> None:
    await _apply(env, "a = 'top secret marker zzz'\n")
    async with env.client() as c:
        rows = (await c.get("/api/v1/audit?system=switchyard")).json()
    assert rows and rows[0]["method"] == "POST" and rows[0]["status"] == 200
    assert "zzz" not in str(rows) and "chars]" in str(rows[0]["params"]["content"])
