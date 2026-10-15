from __future__ import annotations

from pathlib import Path

import pytest

from nemo_console.adapters.base import CommandResult
from nemo_console.adapters.switchyard import capable_models
from nemo_console.config import DEFAULT_ENGINE_PROBES, DEFAULT_ENGINE_START, Settings
from nemo_console.services.engines import CONTAINER_RE, EngineProbe, capable_tier_problems, compose_dir_hint

from .conftest import Env

ALIAS = "qwen3.8-27b-1m"                          # the 27B: still probed, dark by default since 2026-09-26
NAME = "ai-lab-vllm-qwen38-vllm-1"
FN_ALIAS = "qwen3.8-flash-next"                   # the default engine and capable tier since 2026-09-26
FN_NAME = "ai-lab-sglang-flashnext-flashnext-1"

# Shaped like inference/switchyard/routes.toml: capable_target names a [targets.*] KEY whose `id` is the alias, and the
# 27B also appears in a passthrough (direct-27b) and its own target table. CAPABLE is replaced per test.
ROUTES = """
[targets.lightning]
id = "nemotron-lightning"
llm_client = "litellm"

[targets.qwen-27b-1m]
id = "qwen3.8-27b-1m"
llm_client = "litellm"

[targets.flash-next]
id = "qwen3.8-flash-next"
llm_client = "litellm"

[routes.auto-local]
id = "auto-local"
type = "stage_router"
capable_target = "CAPABLE"
efficient_target = "lightning"

[routes.direct-27b]
id = "direct-27b"
type = "passthrough"
target = "qwen-27b-1m"
"""


def routes(capable: str = "flash-next") -> str:
    return ROUTES.replace("CAPABLE", capable)


async def _status(env: Env) -> dict:  # type: ignore[type-arg]
    env.container.status.invalidate("switchyard")
    async with env.client() as c:
        return (await c.get("/api/v1/systems/switchyard/status")).json()


def _codes(st: dict) -> list[str]:  # type: ignore[type-arg]
    return [p["code"] for p in st["problems"]]


def _dark(st: dict) -> list[dict]:  # type: ignore[type-arg]
    return [p for p in st["problems"] if p["code"] == "switchyard.tier.capable_dark"]


def _states(env: Env, states: dict[str, str]) -> None:
    """Per-container `docker inspect` answers (the shared FakeRunner answers every call the same way)."""
    async def run(argv: list[str], **_k: object) -> CommandResult:
        env.runner.calls.append(argv)
        return CommandResult(0, states.get(argv[-1], "running") + "\n", "")
    env.runner.run = run  # type: ignore[method-assign]


async def test_running_engine_no_problem(env: Env) -> None:
    env.cfg_file.write_text(routes())
    st = await _status(env)
    assert "switchyard.tier.capable_dark" not in _codes(st) and st["health"] == "up"


async def test_stopped_capable_engine_degrades_with_its_own_start_command(env: Env) -> None:
    env.cfg_file.write_text(routes())
    env.runner.result = CommandResult(0, "exited\n", "")     # EVERY probed engine reads stopped, the 27B included
    st = await _status(env)
    [p] = _dark(st)                                           # only the capable target, not the passthrough-only 27B
    assert p["severity"] == "warn"
    assert p["message"] == (f"Capable tier target {FN_ALIAS} is not answering: engine container {FN_NAME} is not running, "
                            "so route auto-local cannot escalate to it")
    assert p["fix"] == f"cd {env.settings.repo_root} && inference/sglang-flashnext/flashnext.sh up"
    assert "docker compose up" not in p["fix"]
    assert st["health"] == "degraded" and st["summary"].startswith("capable tier dark")


async def test_passthrough_only_alias_is_not_a_capable_tier(env: Env) -> None:
    # Regression: the old check was `alias in routes_text`, which the 27B always satisfies through direct-27b and
    # [targets.qwen-27b-1m], so a 27B that is dark ON PURPOSE kept Switchyard degraded forever.
    text = routes()
    assert ALIAS in text
    env.cfg_file.write_text(text)
    _states(env, {FN_NAME: "running", NAME: "exited"})
    st = await _status(env)
    assert _dark(st) == [] and st["health"] == "up"
    assert ["docker", "inspect", "-f", "{{.State.Status}}", NAME] not in env.runner.calls   # not even probed


async def test_27b_as_capable_target_points_at_use_model(env: Env) -> None:
    env.cfg_file.write_text(routes("qwen-27b-1m"))
    _states(env, {FN_NAME: "running", NAME: "exited"})
    [p] = _dark(await _status(env))
    assert p["message"].startswith(f"Capable tier target {ALIAS} is not answering: engine container {NAME}")
    assert p["fix"].startswith(f"cd {env.settings.repo_root} && scripts/use-model.sh qwen3.8-27b-1m")
    assert "docker compose up" not in p["fix"]


async def test_absent_container_counts_as_stopped(env: Env) -> None:
    env.cfg_file.write_text(routes())
    env.runner.result = CommandResult(1, "", "Error: No such object: x\n")
    assert "switchyard.tier.capable_dark" in _codes(await _status(env))


async def test_stopped_but_alias_not_in_routes_no_problem(env: Env) -> None:
    env.cfg_file.write_text('id = "something-else"\n')
    env.runner.result = CommandResult(0, "exited\n", "")
    st = await _status(env)
    assert "switchyard.tier.capable_dark" not in _codes(st) and st["health"] == "up"


async def test_unparseable_routes_no_problem(env: Env) -> None:
    env.cfg_file.write_text(routes() + "\n[routes.broken\n")
    env.runner.result = CommandResult(0, "exited\n", "")
    st = await _status(env)
    assert "switchyard.tier.capable_dark" not in _codes(st) and st["health"] == "up"


async def test_probe_error_is_not_a_problem_and_never_raises(env: Env) -> None:
    env.cfg_file.write_text(routes())

    async def boom(*_a: object, **_k: object) -> CommandResult:
        raise OSError("no docker")

    env.runner.run = boom  # type: ignore[method-assign]
    st = await _status(env)
    assert "switchyard.tier.capable_dark" not in _codes(st) and st["health"] == "up"
    env.runner.run = None  # type: ignore[assignment,method-assign]
    env.runner.result = CommandResult(1, "", "Cannot connect to the Docker daemon")
    p = EngineProbe(env.runner)
    env.runner.run = lambda *a, **k: _ret(env.runner.result)  # type: ignore[assignment,method-assign]
    assert await p.state(NAME) == "unknown"
    assert await p.state("bad name; rm -rf") == "unknown"


async def _ret(r: CommandResult) -> CommandResult:
    return r


async def test_topology_and_status_share_one_probe(env: Env) -> None:
    env.cfg_file.write_text(routes())
    _states(env, {FN_NAME: "exited", NAME: "exited"})
    async with env.client() as c:
        await c.get("/api/v1/systems/switchyard/status")
        topo = (await c.get("/api/v1/topology")).json()
    nodes = {n["id"]: n for n in topo["nodes"]}
    # the default engine stopped is a fault (red); the opt-in 27B stopped is just off
    assert nodes["sglang-flashnext"]["health"] == "down" and nodes["vllm-qwen38"]["health"] == "off"
    assert "both cards" in nodes["sglang-flashnext"]["label"]
    assert any(e["src"] == "litellm" and e["dst"] == "sglang-flashnext" and e["active"] is False for e in topo["edges"])
    inspected = sorted(x[-1] for x in env.runner.calls if x[:2] == ["docker", "inspect"])
    # one call per container: status and topology share it (lightning is probed by the topology only)
    assert inspected == sorted([FN_NAME, NAME, "ai-lab-vllm-nemotron-lightning-nemotron-lightning-1"])


async def test_routes_listed_and_generic_hint_for_unknown_engine(tmp_path: Path) -> None:
    root = tmp_path / "repo"
    (root / "inference" / "myengine").mkdir(parents=True)
    text = routes() + '\n[routes.auto-two]\nid = "auto-two"\ntype = "stage_router"\ncapable_target = "flash-next"\n'

    class Stopped:
        async def run(self, argv: list[str], **_k: object) -> CommandResult:
            return CommandResult(0, "exited\n", "")

    probe = EngineProbe(Stopped())  # type: ignore[arg-type]
    [p] = await capable_tier_problems(probe, {FN_ALIAS: "ai-lab-myengine-srv-1"}, text, root)
    assert p.message.endswith("so routes auto-local, auto-two cannot escalate to it")
    assert p.fix == "cd inference/myengine && docker compose up -d"   # no start command configured for it
    [q] = await capable_tier_problems(probe, {FN_ALIAS: FN_NAME}, text, root, dict(DEFAULT_ENGINE_START))
    assert q.fix == f"cd {root} && inference/sglang-flashnext/flashnext.sh up"


def test_capable_models_parses_only_stage_router_capable_targets() -> None:
    import tomllib
    assert capable_models(tomllib.loads(routes())) == {FN_ALIAS: ["auto-local"]}
    assert capable_models(tomllib.loads(routes("qwen-27b-1m"))) == {ALIAS: ["auto-local"]}
    assert capable_models(tomllib.loads(routes("no-such-target"))) == {}
    # malformed shapes never raise
    assert capable_models({"routes": [], "targets": 5}) == {}
    assert capable_models({"routes": {"r": {"type": "stage_router", "capable_target": ["x"]}}, "targets": {}}) == {}


async def test_dir_hint_and_validation(env: Env) -> None:
    root = env.tmp / "repo"
    (root / "inference" / "vllm-qwen38").mkdir(parents=True)
    assert compose_dir_hint(root, NAME) == "inference/vllm-qwen38"
    assert compose_dir_hint(root, "ai-lab-zzz-1") is None
    assert CONTAINER_RE.fullmatch("a\n") is None


def test_engine_probes_env_merges(monkeypatch: pytest.MonkeyPatch) -> None:
    assert DEFAULT_ENGINE_PROBES == {FN_ALIAS: FN_NAME, ALIAS: NAME}
    monkeypatch.setenv("NEMO_CONSOLE_ENGINE_PROBES__other", "ai-lab-x-1")     # env keys are lower-cased by pydantic
    assert Settings().engine_probes == {**DEFAULT_ENGINE_PROBES, "other": "ai-lab-x-1"}
    monkeypatch.setenv("NEMO_CONSOLE_ENGINE_PROBES", '{"a.b": "ai-lab-y-1"}')   # JSON form allows dotted aliases; still merged
    assert Settings().engine_probes["a.b"] == "ai-lab-y-1" and ALIAS in Settings().engine_probes
    assert FN_ALIAS in Settings().engine_probes


def test_engine_start_env_merges(monkeypatch: pytest.MonkeyPatch) -> None:
    assert set(DEFAULT_ENGINE_START) == {FN_NAME, NAME}
    assert all("docker compose up" not in cmd for cmd in DEFAULT_ENGINE_START.values())
    monkeypatch.setenv("NEMO_CONSOLE_ENGINE_START", '{"ai-lab-x-1": "scripts/start-x.sh"}')
    assert Settings().engine_start == {**DEFAULT_ENGINE_START, "ai-lab-x-1": "scripts/start-x.sh"}
