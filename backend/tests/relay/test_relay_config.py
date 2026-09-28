from __future__ import annotations

import hashlib
import os
from pathlib import Path

import pytest
from relay_helpers import fx

from nemo_console.adapters.relay import base_url_problem, validate_routes_semantics


async def test_config_files_contract(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    specs = {s.id: s for s in a.config_files()}
    assert set(specs) == {"routes", "plugins", "config"}
    r = specs["routes"]
    assert r.editable and r.authoritative is False and r.format == "toml" and "restart" in r.apply_effect
    for fid in ("plugins", "config"):
        s = specs[fid]
        assert not s.editable and s.why_readonly and "human decision" in s.why_readonly and "diff" in s.why_readonly
    assert all(s.display_path and not s.display_path.startswith("/home/") for s in specs.values())


async def test_config_path_and_read(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        a.config_path("nope")
    with pytest.raises(KeyError):
        await a.read_config("nope")
    cf = await a.read_config("routes")
    assert cf.sha256 == hashlib.sha256(fx("routes.toml").encode()).hexdigest()
    assert cf.content == fx("routes.toml") and cf.spec.editable
    pl = await a.read_config("plugins")
    assert "ed25519:" in pl.content and pl.spec.editable is False


async def test_read_config_refuses_symlink_oversize_and_redacts(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    target = tmp_path / "elsewhere.toml"
    target.write_text("x = 1\n")
    p = a.config_path("config")
    p.unlink()
    os.symlink(target, p)
    with pytest.raises(PermissionError):
        await a.read_config("config")
    p.unlink()
    p.write_text("a" * (300 * 1024))
    with pytest.raises(PermissionError):
        await a.read_config("config")
    p.write_text('api_key = "sk-abcdefghijklmnopqrstuvwxyz012345"\ntoken = "hunter2hunter2"\nauth = "Bearer abcdefghijkl123"\n')
    cf = await a.read_config("config")
    assert "sk-abcdef" not in cf.content and "hunter2" not in cf.content and "abcdefghijkl123" not in cf.content
    assert cf.content.count("[REDACTED]") == 3


async def test_repo_routes_toml_validates_clean_but_non_authoritative(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    v = await a.validate_config("routes", fx("routes.toml"))
    assert v.ok, [d.message for d in v.diagnostics if d.severity == "error"]
    assert v.authoritative is False and v.note
    assert "switchyard-server --dry-run" in v.note and "NOT" in v.note and "restart" in v.note


def errs(text: str) -> list[str]:
    return [d.message for d in validate_routes_semantics(text) if d.severity == "error"]


def test_semantic_rejections() -> None:
    base = fx("routes.toml")
    assert any("capable_system_prompt" in m for m in errs(base.replace('type = "stage_router"', 'type = "stage_router"\ncapable_system_prompt = "x"', 1)))
    assert any("not defined under [targets]" in m for m in errs(base.replace('target = "qwen-27b-1m"', 'target = "missing"', 1)))
    assert any("api_key_env" in m for m in errs(base.replace('api_key_env = "LITELLM_MASTER_KEY"', 'api_key_env = "sk-live-1234567890abcdefgh"', 1)))
    assert any("literal API keys" in m for m in errs(base.replace('api_key_env = "LITELLM_MASTER_KEY"', 'api_key = "abc"', 1)))
    assert any("schema_version" in m for m in errs(base.replace("schema_version = 1", "schema_version = 2")))
    assert any("confidence_threshold" in m for m in errs(base.replace("confidence_threshold = 0.5", "confidence_threshold = 1.5", 1)))
    assert any("format" in m for m in errs(base.replace('format = "openai_chat"', 'format = "grpc"')))
    assert any("duplicates" in m for m in errs(base.replace('id = "direct-27b"', 'id = "direct-lightning"')))


def test_two_wire_formats_for_one_model_id_is_an_error() -> None:
    # the judge alias exists precisely because the 0.3.0 plugin refuses this
    bad = fx("routes.toml").replace('id = "nemotron-lightning-judge"', 'id = "nemotron-lightning"')
    assert any("two wire formats" in m for m in errs(bad))


def test_syntax_error_reports_line_and_unknown_field_is_a_warning() -> None:
    d = validate_routes_semantics('schema_version = 1\n[routes.x\nid = "a"\n')
    assert d[0].severity == "error" and d[0].line is not None
    warn = validate_routes_semantics(fx("routes.toml").replace("context_window = 262144", "context_window = 262144\nwarp = 9", 1))
    assert any(x.severity == "warn" and "warp" in x.message and x.line for x in warn)
    assert not any(x.severity == "error" for x in warn)


async def test_validate_flags_literal_secret_and_readonly_files(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    v = await a.validate_config("routes", fx("routes.toml") + "\n# sk-abcdefghijklmnopqrstuvwxyz012345\n")
    assert not v.ok
    v2 = await a.validate_config("plugins", "version = 1\n")
    assert v2.ok and v2.authoritative is False and v2.note and "PASS" in v2.note
    assert not (await a.validate_config("config", "= broken")).ok
    with pytest.raises(KeyError):
        await a.validate_config("nope", "")


@pytest.mark.parametrize("url", [
    "http://127.0.0.1:4000", "http://127.0.0.1:4000/v1", "http://litellm:4000/v1", "http://localhost:4000", "http://[::1]:4000/v1",
    "http://127.5.5.5", "http://127.0.0.1.:4000", "http://LiteLLM./v1", "https://localhost",
])
def test_base_url_local_targets_allowed(url: str) -> None:
    assert base_url_problem(url) is None


@pytest.mark.parametrize("url", [
    "http://evil.com", "http://evil.com.", "http://127.0.0.1@evil.com", "http://127.0.0.1:4000@evil.com/v1",
    "http://user:pw@127.0.0.1:4000", "http://[::1]@evil.com", "http://[2001:db8::1]:4000", "http://[::ffff:8.8.8.8]",
    "http://10.0.0.5:4000", "http://192.168.1.7:14001/v1", "http://127.0.0.1.evil.com", "http://localhost.evil.com",
    "http://2130706433", "http://0x7f.1", "http://evil.com\\@127.0.0.1", "http://127.0.0.1 @evil.com", "http://:4000",
    "http://litellm:99999", "ftp://litellm", "http://[::1", "http://exa mple",
])
def test_base_url_offbox_or_tricky_rejected(url: str) -> None:
    assert base_url_problem(url)


def test_offbox_base_url_is_an_error_diagnostic() -> None:
    bad = fx("routes.toml").replace('base_url = "http://127.0.0.1:4000/v1"', 'base_url = "http://127.0.0.1@evil.com/v1"', 1)
    e = errs(bad)
    assert any("userinfo" in m for m in e)
    off = fx("routes.toml").replace('base_url = "http://127.0.0.1:4000"', 'base_url = "https://api.example.com"', 1)
    assert any("off-box" in m for m in errs(off))


def test_repo_switchyard_server_routes_toml_validates_clean() -> None:
    real = next((d / "inference" / "switchyard" / "routes.toml" for d in Path(__file__).resolve().parents
                 if (d / "inference" / "switchyard" / "routes.toml").is_file()), None)
    if real is None:
        pytest.skip("no inference/switchyard/routes.toml in any parent directory (console cloned outside the lab repo)")
    assert not [m for m in errs(real.read_text()) if "base_url" in m]
