"""Addendum 9.11: deployment identities and the repo layout are settings, validated, defaulting to the old constants."""
from __future__ import annotations

from pathlib import Path

import httpx
import pytest
from pydantic import ValidationError

from nemo_console.adapters import aiperf as aiperf_mod
from nemo_console.adapters import dcgm as dcgm_mod
from nemo_console.adapters import dynamo as dynamo_mod
from nemo_console.adapters import guardrails as guardrails_mod
from nemo_console.adapters import relay as relay_mod
from nemo_console.adapters import switchyard as switchyard_mod
from nemo_console.adapters.aiperf import AiPerfAdapter
from nemo_console.adapters.base import AdapterContext, CommandResult
from nemo_console.adapters.dcgm import DcgmAdapter
from nemo_console.adapters.dynamo import DynamoAdapter, DynamoNames, merged_worker_uuids, render_override
from nemo_console.adapters.guardrails import GuardrailsAdapter
from nemo_console.adapters.relay import RelayAdapter
from nemo_console.adapters.switchyard import SwitchyardAdapter
from nemo_console.config import DEFAULT_LAYOUT, Settings
from nemo_console.container import build_container

from .conftest import FakeRunner


def _ctx(tmp: Path, runner: FakeRunner, paths: dict[str, Path] | None = None) -> AdapterContext:
    return AdapterContext(runner=runner, http=httpx.AsyncClient(trust_env=False), urls={}, paths=paths or {},
                          state_dir=tmp / "state")


# ── defaults: the existing deployment behaves exactly as before ──────────────────────────────────
def test_defaults_equal_the_adapters_former_constants() -> None:
    s = Settings()
    assert s.switchyard_container == switchyard_mod.CONTAINER
    assert (s.switchyard_compose_project, s.switchyard_compose_service) == (switchyard_mod.COMPOSE_PROJECT,
                                                                            switchyard_mod.COMPOSE_SERVICE)
    assert (s.switchyard_image, s.switchyard_log_path) == (switchyard_mod.FALLBACK_IMAGE, switchyard_mod.LOG_PATH)
    assert s.relay_unit == relay_mod.UNIT
    assert s.dynamo_compose_project == dynamo_mod.PROJECT
    assert s.dynamo_containers == {"frontend": dynamo_mod.FRONTEND, "worker": dynamo_mod.WORKER,
                                   "mocker": dynamo_mod.MOCKER, "sv_frontend": dynamo_mod.SV_FRONTEND}
    assert s.dynamo_services == {"frontend": dynamo_mod.SVC_FRONTEND, "worker": dynamo_mod.SVC_WORKER,
                                 "mocker": dynamo_mod.SVC_MOCKER, "sv_frontend": dynamo_mod.SVC_SV_FRONTEND}
    assert s.guardrails_compose_project == guardrails_mod.COMPOSE_PROJECT
    assert s.dcgm_compose_project == dcgm_mod.DcgmAdapter.compose_project
    assert s.layout == DEFAULT_LAYOUT and not s.capable_probe


def test_default_layout_reproduces_the_old_paths(tmp_path: Path) -> None:
    p = Settings(repo_root=tmp_path).resolved_paths()
    assert p["switchyard_routes"] == tmp_path / "inference" / "switchyard" / "routes.toml"
    assert p["relay_routes"] == tmp_path / "clients" / "nemo-relay" / "routes.toml"
    assert p["dynamo_compose"] == tmp_path / "inference" / "dynamo" / "docker-compose.yml"
    assert p["skills_dir"] == tmp_path / ".claude" / "skills"
    assert p["openshell_policy"] == tmp_path / "clients" / "data-designer" / "runner" / "policy.yaml"
    assert p["dcgm_counters"] == tmp_path / "observability" / "dcgm" / "counters.csv"
    assert p["guardrails_config_dir"] == tmp_path / "security" / "nemo-guardrails" / "config"
    assert p["evaluator_suites"] == tmp_path / "clients" / "nemo-evaluator" / "suites.yaml"


# ── environment overrides ──────────────────────────────────────────────────────────────────────────
def test_env_overrides_single_keys_and_keeps_the_rest(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("NEMO_CONSOLE_SWITCHYARD_CONTAINER", "sy-1")
    monkeypatch.setenv("NEMO_CONSOLE_RELAY_UNIT", "relay@main.service")
    monkeypatch.setenv("NEMO_CONSOLE_DYNAMO_CONTAINERS__WORKER", "my-worker")
    monkeypatch.setenv("NEMO_CONSOLE_LAYOUT__SWITCHYARD", "deploy/switchyard")
    s = Settings(repo_root=tmp_path)
    assert (s.switchyard_container, s.relay_unit) == ("sy-1", "relay@main.service")
    assert s.dynamo_containers["worker"] == "my-worker" and s.dynamo_containers["frontend"] == dynamo_mod.FRONTEND
    p = s.resolved_paths()
    assert p["switchyard_routes"] == tmp_path / "deploy" / "switchyard" / "routes.toml"
    assert p["relay_routes"] == tmp_path / "clients" / "nemo-relay" / "routes.toml"      # untouched keys keep defaults


def test_repo_marker_follows_the_dynamo_layout(tmp_path: Path) -> None:
    (tmp_path / "stacks" / "dynamo").mkdir(parents=True)
    (tmp_path / "stacks" / "dynamo" / "docker-compose.yml").write_text("services: {}\n")
    assert Settings(repo_root=tmp_path, layout={"dynamo": "stacks/dynamo"}).repo_root_problem() is None
    problem = Settings(repo_root=tmp_path).repo_root_problem()
    assert problem is not None and "inference/dynamo/docker-compose.yml" in problem


# ── validation: every value is an identifier that reaches argv ─────────────────────────────────────
@pytest.mark.parametrize("field,value", [
    ("switchyard_container", "-rm"), ("switchyard_container", "a b"),
    ("switchyard_compose_project", "Bad"), ("switchyard_compose_project", "-p"),
    ("switchyard_compose_service", "-x"), ("switchyard_image", "UPPER:1"),
    ("switchyard_log_path", "relative/log"), ("switchyard_log_path", "/var/../etc/passwd"),
    ("switchyard_log_path", "/a b"), ("relay_unit", "nemo-relay-gateway"), ("relay_unit", "-x.service"),
    ("dynamo_compose_project", "x;y"), ("guardrails_compose_project", ""), ("dcgm_compose_project", "UP"),
    ("capable_probe_key_env", "HOME"), ("capable_probe_key_env", "AWS_SECRET_ACCESS_KEY"),
    ("capable_probe_interval_s", 60.0),
])
def test_invalid_identifiers_are_refused(field: str, value: object) -> None:
    with pytest.raises(ValidationError):
        Settings(**{field: value})  # type: ignore[arg-type]


@pytest.mark.parametrize("layout", [{"switchyard": "../outside"}, {"switchyard": "/abs/path"}, {"switchyard": "a//b"},
                                    {"switchyard": "a/./b"}, {"switchyard": "a\\b"}, {"nope": "x"}, {"relay": ""}])
def test_bad_layout_entries_are_refused(layout: dict[str, str]) -> None:
    with pytest.raises(ValidationError):
        Settings(layout=layout)


@pytest.mark.parametrize("field,value", [
    ("dynamo_containers", {"router": "x"}), ("dynamo_containers", {"worker": "-x"}),
    ("dynamo_containers", {"worker": "ai-lab-dynamo-dynamo-frontend-1"}),      # two roles, one container
    ("dynamo_services", {"worker": "--rm"}), ("dynamo_services", {"gateway": "x"}),
])
def test_bad_dynamo_maps_are_refused(field: str, value: dict[str, str]) -> None:
    with pytest.raises(ValidationError):
        Settings(**{field: value})  # type: ignore[arg-type]


# ── wiring: the container hands the settings to the real adapters ──────────────────────────────────
def test_container_configures_the_registry_built_adapters(tmp_path: Path) -> None:
    cfg = Settings(state_dir=tmp_path / "state", background_tasks=False,
                   switchyard_container="sy-1", switchyard_compose_project="sy", switchyard_compose_service="svc",
                   switchyard_image="sy:1", switchyard_log_path="/logs/r.jsonl", relay_unit="relay.service",
                   dynamo_compose_project="dyn", dynamo_containers={"worker": "w-1", "frontend": "f-1"},
                   dynamo_services={"worker": "w", "frontend": "f"}, guardrails_compose_project="gr",
                   dcgm_compose_project="obs")
    c = build_container(cfg, runner=FakeRunner(), http=httpx.AsyncClient(trust_env=False))  # type: ignore[arg-type]
    sy, rl, dy = c.adapters["switchyard"], c.adapters["relay"], c.adapters["dynamo"]
    gr, dc = c.adapters["guardrails"], c.adapters["dcgm"]
    assert isinstance(sy, SwitchyardAdapter) and isinstance(rl, RelayAdapter) and isinstance(dy, DynamoAdapter)
    assert isinstance(gr, GuardrailsAdapter) and isinstance(dc, DcgmAdapter)
    assert (sy.container, sy.compose_project, sy.compose_service, sy.fallback_image, sy.log_path) == (
        "sy-1", "sy", "svc", "sy:1", "/logs/r.jsonl")
    assert rl.unit == "relay.service"
    assert (dy.names.project, dy.names.worker, dy.names.frontend, dy.names.svc_worker) == ("dyn", "w-1", "f-1", "w")
    assert dy.names.mocker == dynamo_mod.MOCKER                       # unset roles keep the default
    assert dy.policy.own == dy.names.all and "w-1" in dy.policy.own
    assert gr.compose_project == "gr" and dc.compose_project == "obs"


async def test_switchyard_checks_the_configured_compose_labels(tmp_path: Path) -> None:
    runner = FakeRunner()
    runner.result = CommandResult(0, "running||img:1|sy|svc\n", "")
    a = SwitchyardAdapter(_ctx(tmp_path, runner))
    assert await a._inspect() is None                                 # default labels do not match sy/svc
    a.container, a.compose_project, a.compose_service = "sy-1", "sy", "svc"
    assert await a._inspect() == ("running", "", "img:1")
    assert runner.calls[-1][-1] == "sy-1"


async def test_relay_asks_systemd_about_the_configured_unit(tmp_path: Path) -> None:
    runner = FakeRunner()
    a = RelayAdapter(_ctx(tmp_path, runner))
    a.unit = "relay@main.service"
    await a._unit()
    assert runner.calls[-1][:5] == ["systemctl", "--user", "--timestamp=unix", "show", "relay@main.service"]


async def test_dynamo_inspects_the_configured_containers(tmp_path: Path) -> None:
    compose = tmp_path / "dyn" / "docker-compose.yml"
    compose.parent.mkdir()
    compose.write_text("services: {}\n")
    runner = FakeRunner()
    runner.result = CommandResult(0, "[]", "")
    names = DynamoNames(project="dyn", frontend="f-1", worker="w-1", mocker="m-1", sv_frontend="s-1")
    a = DynamoAdapter(_ctx(tmp_path, runner, {"dynamo_compose": compose}), names=names)
    await a._compose_files()
    assert runner.calls[-1] == ["docker", "inspect", "w-1", "f-1", "s-1", "m-1"]


def test_render_and_gpu_parse_use_the_configured_services() -> None:
    text, _ = render_override({"max_model_len": 4096}, ["python3", "-m", "dynamo.vllm", "--model", "m"],
                              worker_service="w", frontend_service="f")
    assert "\n  w:\n" in text and "dynamo-worker" not in text
    doc = '{"services": {"w": {"deploy": {"resources": {"reservations": {"devices": [{"device_ids": ' \
          '["GPU-12345678-1234-1234-1234-123456789abc"]}]}}}}}}'
    assert merged_worker_uuids(doc, worker_service="w") == ["GPU-12345678-1234-1234-1234-123456789abc"]
    assert merged_worker_uuids(doc) == []                              # the default service is not in this doc


def test_lab_helpers_follow_the_layout_dir_first(tmp_path: Path) -> None:
    lay = tmp_path / "custom" / "aiperf"
    lay.mkdir(parents=True)
    (lay / "install.sh").write_text("#!/bin/sh\n")
    a = AiPerfAdapter(_ctx(tmp_path, FakeRunner(), {"aiperf_dir": lay, "repo": tmp_path}))
    assert a._lab_file(aiperf_mod.LAB_INSTALLER) == lay / "install.sh"
    assert a._lab_file(aiperf_mod.LAB_MINT_KEY) is None               # absent everywhere: the generic text applies
