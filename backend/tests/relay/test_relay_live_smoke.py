"""Opt-in READ-ONLY live smoke against this workstation's real gateway. Run with NEMO_CONSOLE_LIVE=1.

Uses a real subprocess runner limited to `systemctl --user show`, `nemo-relay --version` and
`nemo-relay plugins list --json`. It never restarts anything and never sends a prompt.
"""
from __future__ import annotations

import os
from pathlib import Path

import httpx
import pytest
from relay_helpers import ReadOnlyRealRunner

from nemo_console.adapters.base import AdapterContext
from nemo_console.adapters.relay import RelayAdapter
from nemo_console.config import Settings

pytestmark = pytest.mark.skipif(os.environ.get("NEMO_CONSOLE_LIVE") != "1", reason="live smoke is opt-in (NEMO_CONSOLE_LIVE=1)")


async def test_live_status_metrics_events() -> None:
    home = Path.home()
    repo = Settings().repo_root  # NEMO_CONSOLE_REPO_ROOT, else walk up from the cwd
    paths = {"relay_routes": repo / "clients/nemo-relay/routes.toml", "relay_plugins": home / ".config/nemo-relay/plugins.toml",
             "relay_config": home / ".config/nemo-relay/config.toml", "relay_atof_dir": home / ".local/share/nemo-relay/atof",
             "relay_atif_dir": home / ".local/share/nemo-relay/atif"}
    async with httpx.AsyncClient(trust_env=False) as client:
        a = RelayAdapter(AdapterContext(runner=ReadOnlyRealRunner(), http=client, urls={"relay": "http://127.0.0.1:4040"},
                                        paths=paths, state_dir=Path("/tmp")))
        st = await a.status()
        m = await a.metrics()
        evs = await a.recent_events(limit=20)
        cfg = await a.read_config("routes")
        val = await a.validate_config("routes", cfg.content)
    print("STATUS", st.health, st.version, st.summary, [(p.severity, p.code) for p in st.problems])
    print("METRICS", {p.key: p.value for p in m.points})
    print("EVENTS", len(evs), [e.summary for e in evs[-3:]])
    print("ROUTES", val.ok, [d.message for d in val.diagnostics])
    assert st.health in ("up", "degraded", "down")
    for e in evs:
        assert set(e.data) <= {"atof_kind", "name", "size_bytes", "phase", "category", "uuid", "gateway_path", "status_code", "severity",
                               "session_id", "turn_index", "model", "served_model", "target", "target_model", "algorithm", "outcome",
                               "call_role", "failure_kind", "stop_reason", "event_type", "provider", "error_category", "error_phase",
                               "latency_ms", "call_index", "upstream_status", "chunk_index", "fallback_used", "finish_reason",
                               "input_tokens", "output_tokens", "total_tokens", "cache_read_tokens", "cache_write_tokens",
                               "reasoning_tokens", "cached_tokens"}
