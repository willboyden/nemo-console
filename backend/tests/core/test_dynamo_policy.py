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
BOLT = "ai-lab-vllm-nemotron-bolt-nemotron-bolt-1"
GLM = "lil-glm53-tp2"


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
    # neutral: without an alias the script only lists the engines (2026-10-17: no fix may tear down Super VL)
    assert s.default_model_start.startswith("scripts/use-model.sh   # lists the engines")
    assert s.dynamo_exclusive_engines == [BOLT, GLM, FN, QWEN]
    assert s.dynamo_coexisting_engines == [LIGHTNING, "ai-lab-ollama-ollama-1", "ai-lab-chatterbox-tts-tts-1"]
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
    # the always-on engine stopped is a fault; the opt-in ones (flashnext since 2026-10-17) are only off
    env.container.topology._cache = None
    env.container.topology.probe._cache.clear()
    _states(env, {FN: "exited", QWEN: "exited", LIGHTNING: "exited"})
    async with env.client() as c:
        nodes = {n["id"]: n for n in (await c.get("/api/v1/topology")).json()["nodes"]}
    assert nodes["vllm-lightning"]["health"] == "down"
    assert nodes["sglang-flashnext"]["health"] == "off" and nodes["vllm-qwen38"]["health"] == "off"


async def test_topology_shows_missing_containers_as_not_deployed_never_red(env: Env) -> None:
    # 2026-10-17: flashnext and the 27B containers do not exist on the author's host; lightning missing is not red either
    async def run(argv: list[str], **_k: object) -> CommandResult:
        env.runner.calls.append(argv)
        if argv[-1] in (FN, QWEN, LIGHTNING):
            return CommandResult(1, "", f"Error: No such object: {argv[-1]}\n")
        return CommandResult(0, "running\n", "")
    env.runner.run = run  # type: ignore[method-assign]
    async with env.client() as c:
        topo = (await c.get("/api/v1/topology")).json()
    nodes = {n["id"]: n for n in topo["nodes"]}
    for nid in ("sglang-flashnext", "vllm-qwen38", "vllm-lightning"):
        assert nodes[nid]["health"] == "off" and nodes[nid]["label"].endswith("(not deployed)"), nid
    assert nodes["vllm-bolt"]["health"] == "up" and "not deployed" not in nodes["vllm-bolt"]["label"]
    edges = {(e["src"], e["dst"]): e for e in topo["edges"]}
    assert edges[("litellm", "vllm-lightning")]["active"] is False and edges[("litellm", "vllm-bolt")]["active"] is True


def test_topology_engines_are_a_setting(monkeypatch: pytest.MonkeyPatch) -> None:
    from nemo_console.services.topology import TopologyService
    monkeypatch.setenv("NEMO_CONSOLE_TOPOLOGY_ENGINES",
                       '[{"id": "my-engine", "label": "My engine", "container": "my-engine-1", "opt_in": false},'
                       ' {"id": "litellm", "label": "shadow", "container": "x-1"},'
                       ' {"id": "my-engine", "label": "dup", "container": "y-1"}]')
    s = Settings()
    assert [e.id for e in s.topology_engines] == ["my-engine", "litellm", "my-engine"]       # replaced, not merged
    t = TopologyService(None, None, None, "http://127.0.0.1:4000", s.topology_engines)  # type: ignore[arg-type]
    assert [(e.id, e.container, e.opt_in) for e in t.engines] == [("my-engine", "my-engine-1", False)]
    monkeypatch.setenv("NEMO_CONSOLE_TOPOLOGY_ENGINES", '[{"id": "Bad Id", "label": "x", "container": "c-1"}]')
    with pytest.raises(ValueError):
        Settings()
