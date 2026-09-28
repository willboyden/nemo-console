"""Override rendering / validation: pure functions, golden output, and the compose-merge proof (fake + live)."""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest
import yaml

from nemo_console.adapters.dynamo import (
    ParamError,
    _load_base_commands,
    param_warnings,
    parse_values_header,
    render_override,
    validate_values,
)

from .helpers import FIX, fx

BASE = fx("base_compose.yml")
WCMD, FCMD = _load_base_commands(BASE)
GOLDEN_VALUES = {
    "max_model_len": 131072, "gpu_memory_utilization": 0.6, "num_speculative_tokens": 3,
    "enable_prefix_caching": False, "router_mode": "kv", "reasoning_parser": "none",
}


def test_golden_override_matches_fixture() -> None:
    text, warns = render_override(GOLDEN_VALUES, WCMD, FCMD)
    assert text == fx("golden_override.yml")
    assert len(warns) == 2


def test_base_interpolations_stay_live_not_escaped() -> None:
    # regression: escaping `$` froze `${MAX_NUM_SEQS:-64}` into literal text inside the container
    text, _ = render_override({"max_model_len": 4096}, WCMD, FCMD)
    assert '"${MAX_NUM_SEQS:-64}"' in text and "$$" not in text


def test_only_supplied_keys_change_and_unmanaged_flags_survive() -> None:
    text, _ = render_override({"max_num_seqs": 8}, WCMD, FCMD)
    cmd = yaml.safe_load(text)["services"]["dynamo-worker"]["command"]
    assert cmd[cmd.index("--max-num-seqs") + 1] == "8"
    assert cmd[cmd.index("--dyn-tool-call-parser") + 1] == "qwen3_coder"
    assert "--dyn-enable-structural-tag" in cmd and "--enable-prefix-caching" in cmd
    assert cmd[cmd.index("--model") + 1].endswith("16b6615af3548b88e2d8e382457bc705b00479cf")
    assert "dynamo-frontend" not in yaml.safe_load(text)["services"]      # round-robin: frontend untouched


def test_speculative_off_and_count() -> None:
    off = yaml.safe_load(render_override({"speculative_enabled": False}, WCMD, FCMD)[0])
    assert "--speculative-config" not in off["services"]["dynamo-worker"]["command"]
    n = yaml.safe_load(render_override({"num_speculative_tokens": 4}, WCMD, FCMD)[0])["services"]["dynamo-worker"]["command"]
    assert n[n.index("--speculative-config") + 1] == '{"method":"mtp","num_speculative_tokens":4}'


def test_structural_tag_off_warns_and_removes_flag() -> None:
    text, warns = render_override({"enable_structural_tag": False}, WCMD, FCMD)
    assert "--dyn-enable-structural-tag" not in yaml.safe_load(text)["services"]["dynamo-worker"]["command"]
    assert any("tool_choice" in w for w in warns)
    assert "# WARNING:" in text


@pytest.mark.parametrize("values", [
    {"nope": 1}, {"max_model_len": True}, {"max_model_len": 10}, {"gpu_memory_utilization": 1.5},
    {"gpu_memory_utilization": "x"}, {"tool_call_parser": "rm -rf /"}, {"enable_prefix_caching": "yes"},
    {"router_mode": "direct"}, {"max_num_seqs": 1.5}, {"kv_cache_memory_bytes": 5},
])
def test_invalid_values_rejected(values: dict[str, object]) -> None:
    with pytest.raises(ParamError):
        validate_values(values)


def test_kv_and_gpu_util_warning_and_header_roundtrip() -> None:
    assert any("ignores gpu_memory_utilization" in w for w in param_warnings({"kv_cache_memory_bytes": 1, "gpu_memory_utilization": 0.5}))
    text, _ = render_override(GOLDEN_VALUES, WCMD, FCMD)
    assert parse_values_header(text) == validate_values(GOLDEN_VALUES)


def test_catalog_flags_come_from_the_pinned_image_help() -> None:
    """Every catalog flag (and every enum choice) must appear in the captured `--help` of that image."""
    from nemo_console.adapters.dynamo import DynamoAdapter
    helps = fx("help_vllm.txt") + fx("help_frontend.txt")
    for p in DynamoAdapter.param_catalog(None):  # type: ignore[arg-type]
        assert p.flag is None or p.flag in helps, p.flag
        if p.key in ("tool_call_parser", "reasoning_parser", "router_mode"):
            for c in p.choices or []:
                assert c == "none" or c in helps, (p.key, c)
    assert "--kv-cache-memory-bytes" in helps and "--kv-cache-memory " not in helps


def test_compose_merge_replaces_command_proof_offline() -> None:
    """`command` in an override REPLACES the base list (compose semantics); see the live test for the real binary."""
    text, _ = render_override(GOLDEN_VALUES, WCMD, FCMD)
    doc = yaml.safe_load(text)
    assert set(doc) == {"services"} and set(doc["services"]) == {"dynamo-worker", "dynamo-frontend"}
    assert all(set(v) == {"command"} for v in doc["services"].values())


@pytest.mark.skipif(shutil.which("docker") is None, reason="docker CLI not available")
def test_docker_compose_config_renders_override(tmp_path: Path) -> None:
    """READ-ONLY proof with the real `docker compose config` (no containers touched)."""
    base = tmp_path / "docker-compose.yml"
    base.write_text(BASE)
    ov = tmp_path / "override.yml"
    ov.write_text(render_override(GOLDEN_VALUES, WCMD, FCMD)[0])
    r = subprocess.run(["docker", "compose", "--project-directory", str(tmp_path), "-f", str(base), "-f", str(ov), "config"],
                       capture_output=True, text=True, timeout=60, check=False)
    if r.returncode != 0 and "ai-lab" in r.stderr and "network" in r.stderr:
        pytest.skip("external network ai-lab not resolvable here")
    assert r.returncode == 0, r.stderr
    merged = yaml.safe_load(r.stdout)["services"]
    wc = merged["dynamo-worker"]["command"]
    assert wc[wc.index("--max-model-len") + 1] == "131072"
    assert wc[wc.index("--max-num-seqs") + 1] == "64"            # base ${MAX_NUM_SEQS:-64} resolved, not literal
    assert "--no-enable-prefix-caching" in wc and "--dyn-reasoning-parser" not in wc
    assert merged["dynamo-frontend"]["command"][-2:] == ["--router-mode", "kv"]
    assert merged["dynamo-worker"]["volumes"] and merged["dynamo-worker"]["deploy"]     # rest of the base survives
    assert FIX.exists()
