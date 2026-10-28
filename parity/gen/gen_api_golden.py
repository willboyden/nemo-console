"""Generate backend/crates/nemo-console/tests/fixtures/api_golden.json from the REAL Python app.

    REF=$(node parity/ref.mjs) && cd backend && env -i HOME=$(mktemp -d) PATH=/nonexistent NEMO_CONSOLE_NO_USER_ENV=1 LANG=C.UTF-8 \
        "$REF/.venv/bin/python" -I ../parity/gen/gen_api_golden.py

Hermetic: every adapter URL points at a closed port (127.0.0.1:9), PATH holds no binary, no credential env is set,
so no request reaches a real service or docker.

Each case is one HTTP request through FastAPI + the security middleware; the golden is (status, body). It pins the
request pipeline's order (JSON parse -> dependencies -> query/body validation), pydantic's error texts and locations,
and the error envelope. Only requests whose answer does NOT depend on a real adapter are listed (validation errors,
auth, 404s of the core); the Rust core tests replay them (tests/core_validation_golden.rs).
"""
from __future__ import annotations

import asyncio
import json
import tempfile
from pathlib import Path

import httpx

from nemo_console.config import Settings
from nemo_console.container import build_container
from nemo_console.main import create_app

OUT = Path(__file__).resolve().parents[2] / "backend" / "crates" / "nemo-console" / "tests" / "fixtures" / "api_golden.json"
S = "/api/v1/systems/switchyard"
SHA = "a" * 64
MSG = [{"role": "user", "content": "x"}]

# (name, method, path, json-or-raw, content_type, role)  role: admin | viewer | none
CASES: list[tuple[str, str, str, object, str | None, str]] = []


def add(name: str, method: str, path: str, body: object = None, ctype: str | None = "application/json",
        role: str = "admin") -> None:
    CASES.append((name, method, path, body, ctype, role))


RAW = object()
# ── bodies of every model ─────────────────────────────────────────────────────────────────────
for name, body in [
    ("validate_missing", {}), ("validate_int", {"content": 5}), ("validate_null", {"content": None}),
    ("validate_list", {"content": ["a"]}), ("validate_extra", {"content": "a", "x": 1}),
    ("validate_bool", {"content": True}), ("validate_float", {"content": 1.5}), ("validate_obj", {"content": {}}),
    ("validate_array_body", ["content"]), ("validate_str_body", "content"), ("validate_num_body", 5),
    ("validate_null_body", None),
]:
    add(name, "POST", f"{S}/config/routes/validate", body)
for name, body in [
    ("apply_missing_all", {}), ("apply_bad_sha", {"content": "a", "base_sha256": "XYZ"}),
    ("apply_upper_sha", {"content": "a", "base_sha256": "A" * 64}),
    ("apply_sha_int", {"content": "a", "base_sha256": 5}),
    ("apply_restart_str", {"content": "a", "base_sha256": SHA, "restart": "maybe"}),
    ("apply_restart_strtrue", {"content": "a", "base_sha256": SHA, "restart": "yes", "confirm": "nope"}),
    ("apply_confirm_int2", {"content": "a", "base_sha256": SHA, "confirm": 2}),
    ("apply_confirm_float", {"content": "a", "base_sha256": SHA, "confirm": 0.5}),
    ("apply_confirm_list", {"content": "a", "base_sha256": SHA, "confirm": []}),
    ("apply_extra", {"content": "a", "base_sha256": SHA, "z": None}),
    ("apply_six_errors", {"content": 1, "base_sha256": 2, "restart": "x", "confirm": "y", "a": 1, "b": 2}),
]:
    add(name, "POST", f"{S}/config/routes/apply", body)
for name, body in [
    ("rollback_missing", {}), ("rollback_short", {"revision_id": "abc"}), ("rollback_upper", {"revision_id": "ABCDEFAB"}),
    ("rollback_long", {"revision_id": "a" * 33}), ("rollback_int", {"revision_id": 12345678}),
]:
    add(name, "POST", f"{S}/config/routes/rollback", body)
for name, body in [
    ("action_params_list", {"params": []}), ("action_params_str", {"params": "x"}), ("action_extra", {"zzz": 1}),
    ("action_confirm_str", {"confirm": "maybe"}), ("action_null_params", {"params": None}),
]:
    add(name, "POST", f"{S}/actions/status", body, role="viewer")
add("action_no_body", "POST", f"{S}/actions/status", None, None)
add("action_empty_body_json_ct", "POST", f"{S}/actions/status", RAW, "application/json")
for name, body in [
    ("params_values_list", {"values": []}), ("params_extra", {"values": {}, "x": 1}),
    ("params_put_confirm_str", {"values": {}, "confirm": "x"}),
]:
    add(name, "POST" if "put" not in name else "PUT", f"{S}/params" + ("/preview" if "put" not in name else ""), body)
for name, body in [
    ("chat_missing", {}), ("chat_model_int", {"model": 1, "messages": MSG}), ("chat_messages_empty", {"model": "m", "messages": []}),
    ("chat_messages_201", {"model": "m", "messages": MSG * 201}), ("chat_bad_role", {"model": "m", "messages": [{"role": "root", "content": "x"}]}),
    ("chat_content_int", {"model": "m", "messages": [{"role": "user", "content": 5}]}),
    ("chat_content_list_of_str", {"model": "m", "messages": [{"role": "user", "content": ["a"]}]}),
    ("chat_msg_extra", {"model": "m", "messages": [{"role": "user", "content": "x", "name": "n"}]}),
    ("chat_msg_not_obj", {"model": "m", "messages": ["x"]}), ("chat_max_tokens_0", {"model": "m", "messages": MSG, "max_tokens": 0}),
    ("chat_max_tokens_str", {"model": "m", "messages": MSG, "max_tokens": "12"}),
    ("chat_max_tokens_float", {"model": "m", "messages": MSG, "max_tokens": 1.5}),
    ("chat_stream_str", {"model": "m", "messages": MSG, "stream": "no"}),
    ("chat_system_long", {"model": "m", "messages": MSG, "system": "x" * 100_001}),
    ("chat_system_int", {"model": "m", "messages": MSG, "system": 3}),
    ("chat_extra", {"model": "m", "messages": MSG, "extra": 1}),
    ("chat_bad_model_ok_body", {"model": "bad model!", "messages": MSG}),
]:
    add(name, "POST", f"{S}/playground/chat", body)
for name, body in [
    ("settings_urls_list", {"urls": []}), ("settings_urls_val_int", {"urls": {"dynamo": 5}}),
    ("settings_poll_str", {"poll_interval_s": "fast"}), ("settings_poll_strnum", {"poll_interval_s": "5"}),
    ("settings_poll_bool", {"poll_interval_s": True}), ("settings_extra", {"state_dir": "/tmp"}),
    ("settings_key_env_int", {"playground_key_env": {"a": 1}}), ("settings_all_null", {"urls": None, "poll_interval_s": None}),
]:
    add(name, "PUT", "/api/v1/settings", body)
# ── raw bodies / content types ────────────────────────────────────────────────────────────────
add("json_malformed", "PUT", "/api/v1/settings", '{"urls": ', "application/json")
add("json_malformed2", "POST", f"{S}/config/routes/validate", "{,}", "application/json")
add("json_trailing", "POST", f"{S}/config/routes/validate", '{"content": "a"} x', "application/json")
add("json_text_plain", "POST", f"{S}/config/routes/validate", '{"content": "a"}', "text/plain")
add("json_no_ctype", "POST", f"{S}/config/routes/validate", '{"content": "a"}', None)
add("json_vnd", "POST", f"{S}/config/routes/validate", '{"content": 5}', "application/vnd.x+json")
add("json_form", "POST", f"{S}/config/routes/validate", "content=a", "application/x-www-form-urlencoded")
add("json_malformed_unknown_system", "POST", "/api/v1/systems/nope/config/routes/validate", "{", "application/json")
add("json_malformed_viewer_admin_route", "POST", f"{S}/config/routes/validate", "{", "application/json", "viewer")
add("json_nan", "POST", f"{S}/config/routes/validate", '{"content": NaN}', "application/json")
add("json_bad_utf8", "POST", f"{S}/config/routes/validate", b'{"content": "\xff"}', "application/json")
# ── dependency order ──────────────────────────────────────────────────────────────────────────
add("viewer_bad_body_admin_route", "POST", f"{S}/config/routes/validate", {"content": 5}, role="viewer")
add("unknown_system_bad_body", "POST", "/api/v1/systems/nope/config/routes/validate", {"content": 5})
add("bad_file_id_bad_body", "POST", f"{S}/config/Bad%20Id/validate", {"content": 5})
add("bad_action_id", "POST", f"{S}/actions/Bad%20Id", {"zzz": 1})
add("events_limit_0_unknown_system", "GET", "/api/v1/systems/nope/events?limit=0")
# ── query params ──────────────────────────────────────────────────────────────────────────────
for q in ("limit=0", "limit=501", "limit=abc", "limit=1.5", "limit=", "limit=10&limit=0", "limit=%2B5", "limit=+5", "limit=05",
          "limit=1_0", "limit= 5", "limit=5.0"):
    add(f"events_{q}", "GET", f"{S}/events?{q}")
for q in ("limit=0", "limit=201", "offset=-1", "limit=x&offset=y"):
    add(f"records_{q}", "GET", f"/api/v1/systems/datadesigner/runs/r1/records?{q}")
for q in ("limit=0", "limit=x", "system=zzz", "limit=600&system=zzz"):
    add(f"audit_{q}", "GET", f"/api/v1/audit?{q}")
for q in ("window=2h", "window=", "key=", "key=Bad%20Key", "window=1h&key=a.b{x=1}", "window=1h&key=a.b"):
    add(f"history_{q}", "GET", f"{S}/metrics/history?{q}")
for q in ("window=9y", "window="):
    add(f"routing_{q}", "GET", f"/api/v1/routing/summary?{q}")
# ── auth / roles / 404 / 405 ──────────────────────────────────────────────────────────────────
add("no_key_systems", "GET", "/api/v1/systems", role="none")
add("no_key_health", "GET", "/api/v1/health", role="none")
add("viewer_audit", "GET", "/api/v1/audit", role="viewer")
add("viewer_settings_put", "PUT", "/api/v1/settings", {}, role="viewer")
add("unknown_system_status", "GET", "/api/v1/systems/nope/status")
add("unknown_api_route", "GET", "/api/v1/definitely-not-a-route")
add("post_systems_405", "POST", "/api/v1/systems", {})
add("delete_settings_405", "DELETE", "/api/v1/settings")
add("patch_root_405", "PATCH", "/", {})
add("head_health", "HEAD", "/api/v1/health")
add("options_health", "OPTIONS", "/api/v1/health")
add("trailing_slash_systems", "GET", "/api/v1/systems/")
add("trailing_slash_settings_put", "PUT", "/api/v1/settings/", {})
add("docs_404", "GET", "/docs", role="none")
add("unknown_file_404", "GET", f"{S}/config/nope")
add("bad_file_id_404", "GET", f"{S}/config/..%2f..%2fetc")
add("unknown_action_404", "POST", f"{S}/actions/nope", {})
add("dd_records_wrong_system", "GET", "/api/v1/systems/switchyard/runs/r1/records")
add("dd_files_wrong_system", "GET", "/api/v1/systems/switchyard/runs/r1/files")


async def main() -> None:
    tmp = Path(tempfile.mkdtemp(prefix="nc-golden-"))
    closed = "http://127.0.0.1:9"
    cfg = Settings(state_dir=tmp / "state", repo_root=tmp / "repo", frontend_dir=tmp / "front", background_tasks=False,
                   urls={k: closed for k in ("switchyard", "relay", "dynamo", "dynamo_worker_metrics", "litellm", "dcgm",
                                             "prometheus", "guardrails")})
    assert "LITELLM_API_KEY" not in __import__("os").environ and "docker" not in __import__("os").environ["PATH"]
    c = build_container(cfg)
    app = create_app(cfg, c)
    _, admin = c.auth.mint("admin")
    _, viewer = c.auth.mint("viewer")
    out = []
    transport = httpx.ASGITransport(app=app, client=("127.0.0.1", 5000))
    async with httpx.AsyncClient(transport=transport, base_url="http://127.0.0.1:8793") as cl:
        for name, method, path, body, ctype, role in CASES:
            headers = {"X-Console": "1"}
            if role != "none":
                headers["Authorization"] = f"Bearer {admin if role == 'admin' else viewer}"
            if ctype is not None:
                headers["Content-Type"] = ctype
            if body is RAW:
                content: bytes | None = b""
            elif isinstance(body, bytes):
                content = body
            elif isinstance(body, str) and ctype != "application/json" or name.startswith("json_"):
                content = body.encode() if isinstance(body, str) else body
            elif body is None and method in ("GET", "HEAD", "OPTIONS", "DELETE") or name == "action_no_body":
                content = None
            else:
                content = json.dumps(body).encode()
            r = await cl.request(method, path, content=content, headers=headers)
            try:
                doc = r.json()
            except ValueError:
                doc = {"_text": r.text}
            sent = None if content is None else content.decode("utf-8", "replace")
            out.append({"name": name, "method": method, "path": path, "content_type": ctype, "role": role,
                        "body": sent, "status": r.status_code, "response": doc,
                        "allow": r.headers.get("allow")})
    await c.http.aclose()
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps({"generated_by": "parity/gen/gen_api_golden.py", "cases": out}, indent=1) + "\n")
    print(f"{len(out)} cases -> {OUT}")


asyncio.run(main())
