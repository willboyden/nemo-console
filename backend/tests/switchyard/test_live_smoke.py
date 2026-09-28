"""Read-only smoke against the REAL stack. Opt-in: NEMO_LIVE=1. Never runs a disruptive action;
validate_config sends the candidate over stdin to a throwaway --network none container."""
from __future__ import annotations

import os
from pathlib import Path

import httpx
import pytest

from nemo_console.adapters.base import AdapterContext
from nemo_console.adapters.switchyard import SwitchyardAdapter

from .helpers import RealRunner

pytestmark = pytest.mark.skipif(os.environ.get("NEMO_LIVE") != "1", reason="live smoke is opt-in (NEMO_LIVE=1)")
def _repo_root() -> Path | None:
    for parent in Path(__file__).resolve().parents:
        if (parent / "inference" / "switchyard" / "routes.toml").is_file():
            return parent
    return None


REPO = _repo_root()


async def test_live_read_only() -> None:
    if REPO is None:
        pytest.skip("not inside a lab repository checkout (inference/switchyard/routes.toml not found)")
    d = REPO / "inference" / "switchyard"
    async with httpx.AsyncClient(trust_env=False) as http:
        a = SwitchyardAdapter(AdapterContext(runner=RealRunner(), http=http, urls={"switchyard": "http://127.0.0.1:4010"},
                                             paths={"switchyard_dir": d, "switchyard_routes": d / "routes.toml"}))
        st = await a.status()
        print("STATUS", st.health, st.version, st.summary, [(p.code, p.message) for p in st.problems])
        m = await a.metrics()
        print("METRICS", m.available, sorted({p.key for p in m.points}))
        evs = await a.recent_events(5)
        print("EVENTS", len(evs), [e.summary for e in evs])
        cf = await a.read_config("routes")
        v = await a.validate_config("routes", cf.content)
        print("VALIDATE", v.ok, v.note, [(x.severity, x.message) for x in v.diagnostics])
        bad = await a.validate_config("routes", "a = [")
        print("VALIDATE-BAD", bad.ok, [x.message for x in bad.diagnostics])
        print("PLAYGROUND", a.playground())
        assert st.version == "0.3.0" and m.available and v.ok and not bad.ok
