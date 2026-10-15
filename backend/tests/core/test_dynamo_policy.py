"""Addendum 9.4: the Dynamo card policy comes from Settings, and the topology draws opt-in engines as off."""
from __future__ import annotations

import pytest

from nemo_console.adapters.base import CommandResult
from nemo_console.adapters.dynamo import CardPolicy, DynamoAdapter
from nemo_console.config import Settings
from nemo_console.container import build_container

from .conftest import Env

FN = "ai-lab-sglang-flashnext-flashnext-1"
QWEN = "ai-lab-vllm-qwen38-vllm-1"
LIGHTNING = "ai-lab-vllm-nemotron-lightning-nemotron-lightning-1"


def _states(env: Env, states: dict[str, str]) -> None:
    """Per-container `docker inspect -f {{.State.Status}}` answers for the shared engine probe."""
    async def run(argv: list[str], **_k: object) -> CommandResult:
        env.runner.calls.append(argv)
        return CommandResult(0, states.get(argv[-1], "running") + "\n", "")
    env.runner.run = run  # type: ignore[method-assign]


def test_defaults_describe_the_lab_and_lists_replace(monkeypatch: pytest.MonkeyPatch) -> None:
    s = Settings()
    assert s.dynamo_opt_in is True
    assert s.dynamo_start == "scripts/use-model.sh qwen3.8-27b-dynamo"
    assert s.default_model_start == "scripts/use-model.sh qwen3.8-flash-next"
    assert s.dynamo_exclusive_engines == [FN, QWEN]
    assert s.dynamo_coexisting_engines == [LIGHTNING, "ai-lab-ollama-ollama-1"]
    assert s.dynamo_ignored_containers == ["ai-lab-observability-dcgm-exporter-1"]
    assert CardPolicy() == CardPolicy(start=s.dynamo_start, restore=s.default_model_start,
                                      exclusive=tuple(s.dynamo_exclusive_engines),
                                      coexisting=tuple(s.dynamo_coexisting_engines),
                                      ignored=tuple(s.dynamo_ignored_containers), dark=tuple(s.dynamo_dark_while_held))
    monkeypatch.setenv("NEMO_CONSOLE_DYNAMO_EXCLUSIVE_ENGINES", '["only-this-1"]')
    monkeypatch.setenv("NEMO_CONSOLE_DYNAMO_OPT_IN", "false")
    s2 = Settings()
    assert s2.dynamo_exclusive_engines == ["only-this-1"] and s2.dynamo_opt_in is False      # replaced, not merged


async def test_settings_reach_the_registry_built_dynamo_adapter(env: Env) -> None:
    cfg = Settings(state_dir=env.tmp / "state-p", frontend_dir=env.tmp / "front", background_tasks=False,
                   dynamo_opt_in=False, dynamo_start="my/start", default_model_start="my/restore",
                   dynamo_exclusive_engines=["x-1"], dynamo_coexisting_engines=["y-1"],
                   dynamo_ignored_containers=["z-1"], dynamo_dark_while_held=["alias-a"])
    fakes = {k: v for k, v in env.adapters.items() if k != "dynamo"}
    c = build_container(cfg, adapters=dict(fakes), runner=env.runner, http=env.http)  # type: ignore[arg-type]
    dyn = c.adapters["dynamo"]
    assert isinstance(dyn, DynamoAdapter)
    assert dyn.policy == CardPolicy(opt_in=False, start="my/start", restore="my/restore", exclusive=("x-1",),
                                    coexisting=("y-1",), ignored=("z-1",), dark=("alias-a",))
    # an injected adapter keeps its own policy
    mine = DynamoAdapter(dyn.ctx, policy=CardPolicy(start="injected"))
    c2 = build_container(cfg, adapters={**fakes, "dynamo": mine}, runner=env.runner, http=env.http)  # type: ignore[dict-item]
    assert c2.adapters["dynamo"] is mine and mine.policy.start == "injected"


async def test_topology_draws_opt_in_engines_off_and_shows_lightning(env: Env) -> None:
    env.adapters["dynamo"].health = "off"
    env.adapters["switchyard"].health = "degraded"          # e.g. capable tier dark: still routing, still the default path
    _states(env, {FN: "running", QWEN: "exited", LIGHTNING: "running"})
    async with env.client() as c:
        topo = (await c.get("/api/v1/topology")).json()
    nodes = {n["id"]: n for n in topo["nodes"]}
    assert nodes["dynamo-frontend"]["health"] == "off" and nodes["vllm-qwen38"]["health"] == "off"
    assert nodes["sglang-flashnext"]["health"] == "up" and nodes["vllm-lightning"]["health"] == "up"
    assert "efficient" in nodes["vllm-lightning"]["label"]
    edges = {(e["src"], e["dst"]): e for e in topo["edges"]}
    assert edges[("litellm", "vllm-lightning")]["active"] is True
    assert edges[("litellm", "vllm-qwen38")]["active"] is False and edges[("litellm", "dynamo-frontend")]["active"] is False
    sy = edges[("clients", "switchyard")]
    assert sy["label"] == "default" and sy["active"] is True and len(sy["label"]) <= 9   # short: longer chips are clipped
    # the always-on engines stopped are faults, not "off"
    env.container.topology._cache = None
    env.container.topology.probe._cache.clear()
    _states(env, {FN: "exited", QWEN: "exited", LIGHTNING: "exited"})
    async with env.client() as c:
        nodes = {n["id"]: n for n in (await c.get("/api/v1/topology")).json()["nodes"]}
    assert nodes["sglang-flashnext"]["health"] == "down" and nodes["vllm-lightning"]["health"] == "down"
    assert nodes["vllm-qwen38"]["health"] == "off"
