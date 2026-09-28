"""Independent-review fixes: repo root, action timeout, param warnings, absent config file, urls env merge."""
from __future__ import annotations

import asyncio
import stat
from pathlib import Path

import pytest

from nemo_console import config as config_mod
from nemo_console.config import DEFAULT_URLS, Settings, default_repo_root
from nemo_console.container import build_container
from nemo_console.domain.models import ConfigFile
from nemo_console.services.common import sha256_hex

from .conftest import Env, FakeAdapter, apply_body

EMPTY = sha256_hex(b"")


# (1) repo root ---------------------------------------------------------------------------------
def test_repo_root_env_override(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("NEMO_CONSOLE_REPO_ROOT", str(tmp_path))
    assert Settings().repo_root == tmp_path


def test_repo_root_falls_back_to_cwd_standalone(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    standalone = tmp_path / "site-packages" / "nemo_console"
    standalone.mkdir(parents=True)
    monkeypatch.setattr(config_mod, "__file__", str(standalone / "config.py"))
    work = tmp_path / "work"
    work.mkdir()
    monkeypatch.chdir(work)
    assert default_repo_root() == work.resolve()


def test_repo_root_walks_up_to_compose_files(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    (tmp_path / "inference" / "dynamo").mkdir(parents=True)
    (tmp_path / "inference" / "dynamo" / "docker-compose.yml").write_text("services: {}\n")
    deep = tmp_path / "clients" / "x" / "src" / "nemo_console"
    deep.mkdir(parents=True)
    monkeypatch.setattr(config_mod, "__file__", str(deep / "config.py"))
    assert default_repo_root() == tmp_path.resolve()


async def test_missing_repo_root_is_a_problem_not_a_crash(env: Env) -> None:
    cfg = Settings(state_dir=env.tmp / "st9", repo_root=env.tmp / "nowhere", background_tasks=False)
    c = build_container(cfg, adapters=dict(env.adapters), runner=env.runner, http=env.http)  # type: ignore[arg-type]
    st = await c.status.status("relay", fresh=True)
    assert st.problems[0].code == "console.repo_root.missing"
    assert "NEMO_CONSOLE_REPO_ROOT" in (st.problems[0].fix or "")
    assert cfg.resolved_paths()["dynamo_compose"].name == "docker-compose.yml"     # still builds paths


# (2) action timeout ----------------------------------------------------------------------------
async def test_action_timeout_default_and_per_action_override(env: Env) -> None:
    assert Settings().action_timeout_s == 900.0
    env.container.cfg.action_timeouts = {"dynamo.restart": 0.05}

    async def slow(_a: str, _p: dict[str, object]) -> object:
        await asyncio.sleep(2)
        raise AssertionError

    env.adapters["dynamo"].run_action = slow  # type: ignore[assignment,method-assign]
    async with env.client() as c:
        r = await c.post("/api/v1/systems/dynamo/actions/restart", json={"confirm": True})
    assert r.status_code == 200 and r.json()["ok"] is False and "timed out" in r.json()["problems"][0]["message"]


# (3) param warnings ----------------------------------------------------------------------------
async def test_preview_returns_adapter_warnings(env: Env) -> None:
    env.adapters["dynamo"].param_warnings = lambda v: [f"careful {sorted(v)}"]  # type: ignore[attr-defined]
    async with env.client() as c:
        r = await c.post("/api/v1/systems/dynamo/params/preview", json={"values": {"n": 2}})
        assert r.json()["warnings"] == ["careful ['n']"]
        r = await c.post("/api/v1/systems/switchyard/params/preview", json={"values": {"n": 2}})
        assert r.json()["warnings"] == []                       # adapter without the hook


async def test_preview_warning_hook_crash_is_ignored(env: Env) -> None:
    def boom(_v: object) -> list[str]:
        raise RuntimeError("x")

    env.adapters["dynamo"].param_warnings = boom  # type: ignore[attr-defined]
    async with env.client() as c:
        r = await c.post("/api/v1/systems/dynamo/params/preview", json={"values": {"n": 2}})
    assert r.status_code == 200 and r.json()["warnings"] == []


# (4) config file that does not exist yet -------------------------------------------------------
class PlaceholderAdapter(FakeAdapter):
    async def read_config(self, file_id: str) -> ConfigFile:
        content = self.cfg_file.read_text() if self.cfg_file and self.cfg_file.exists() else ""
        return ConfigFile(spec=self.config_files()[0], content=content, sha256="x", mtime=0)


def _placeholder(env: Env) -> tuple[PlaceholderAdapter, Path]:
    target = env.tmp / "newstate" / "dynamo" / "routes.toml"
    a = PlaceholderAdapter(env.adapters["switchyard"].ctx, "switchyard", target)
    c = build_container(env.settings.model_copy(update={"state_dir": env.tmp / "st5"}),
                        adapters={"switchyard": a, "relay": env.adapters["relay"], "dynamo": env.adapters["dynamo"]},  # type: ignore[arg-type]
                        runner=env.runner, http=env.http)
    env.container = c
    from nemo_console.main import create_app
    env.app = create_app(env.settings, c)
    return a, target


async def test_absent_file_reads_as_empty_and_apply_creates_it_private(env: Env) -> None:
    a, target = _placeholder(env)
    url = "/api/v1/systems/switchyard/config/routes"
    async with env.client() as c:
        r = await c.get(url)
        assert r.status_code == 200 and r.json()["sha256"] == EMPTY and r.json()["content"] == ""
        stale = await c.post(f"{url}/apply", json=apply_body("a = 1\n", "1" * 64))
        assert stale.status_code == 409 and not target.exists()
        ok = await c.post(f"{url}/apply", json=apply_body("a = 1\n", EMPTY))
    assert ok.status_code == 200 and ok.json()["ok"], ok.text
    assert target.read_text() == "a = 1\n"
    assert stat.S_IMODE(target.stat().st_mode) == 0o600
    assert stat.S_IMODE(target.parent.stat().st_mode) == 0o700
    assert a.post_calls == [True]


async def test_failed_first_apply_removes_the_new_file(env: Env) -> None:
    _, target = _placeholder(env)
    async with env.client() as c:
        r = await c.post("/api/v1/systems/switchyard/config/routes/apply", json=apply_body("a = 'BAD'\n", EMPTY))
    assert r.json()["rolled_back"] is True and not target.exists()


# (5) env urls merge ----------------------------------------------------------------------------
def test_env_url_override_merges_into_defaults(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("NEMO_CONSOLE_URLS__DYNAMO", "http://127.0.0.1:9999")
    urls = Settings().urls
    assert urls["dynamo"] == "http://127.0.0.1:9999"
    assert {k: v for k, v in urls.items() if k != "dynamo"} == {k: v for k, v in DEFAULT_URLS.items() if k != "dynamo"}
    monkeypatch.setenv("NEMO_CONSOLE_PLAYGROUND_KEY_ENV__DYNAMO", "DYN_KEY")
    assert Settings().playground_key_env == {"switchyard": "LITELLM_API_KEY", "dynamo": "DYN_KEY"}
