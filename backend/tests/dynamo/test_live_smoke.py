"""READ-ONLY live smoke against the running Dynamo stack. Skipped unless the containers exist and :8020 answers.

Runs status(), metrics(), preview_params() and `docker compose config` (real binary via a runner that refuses
anything that is not inspect/exec/logs/ps/compose-config). Never starts, stops or recreates anything. Works against
the base stack and against a variant started with an extra compose file (the lab's Super VL): with a variant, the
override must be REFUSED, and the version must be the worker's Dynamo tag, never the gateway's digest.
`NEMO_CONSOLE_LIVE_DYNAMO_COMPOSE=/abs/path/docker-compose.yml` points it at a checkout this file is not inside.
"""
from __future__ import annotations

import os
import re
import shutil
from pathlib import Path

import httpx
import pytest

from nemo_console.adapters.base import AdapterContext
from nemo_console.adapters.dynamo import DynamoAdapter

from .helpers import FRONTEND, RealRunner


def _find_base() -> Path:
    """Walk up from this file to the lab checkout; a non-existent path (=> skip) when not inside it."""
    if env := os.environ.get("NEMO_CONSOLE_LIVE_DYNAMO_COMPOSE"):
        return Path(env)
    for parent in Path(__file__).resolve().parents:
        cand = parent / "inference" / "dynamo" / "docker-compose.yml"
        if cand.is_file():
            return cand
    return Path("/nonexistent/inference/dynamo/docker-compose.yml")


BASE = _find_base()


def _live() -> bool:
    if shutil.which("docker") is None or not BASE.is_file():
        return False
    try:
        return httpx.get("http://127.0.0.1:8020/health", timeout=2).status_code == 200
    except httpx.HTTPError:
        return False


pytestmark = pytest.mark.skipif(not _live(), reason="live Dynamo frontend not reachable on 127.0.0.1:8020")


async def test_live_status_metrics_preview(tmp_path: Path) -> None:
    async with httpx.AsyncClient(trust_env=False) as http:
        ctx = AdapterContext(runner=RealRunner(), http=http, urls={"dynamo": "http://127.0.0.1:8020"},
                             paths={"dynamo_compose": BASE}, state_dir=tmp_path)
        ad = DynamoAdapter(ctx)
        st = await ad.status()
        assert st.health in ("up", "degraded") and st.components[0].name == FRONTEND
        assert st.version and not re.fullmatch(r"[0-9a-f]{64}", st.version)      # a tag, never a digest
        cf = await ad._compose_files()
        assert cf.error is None and cf.files
        snap = await ad.metrics()
        assert snap.available and {"dyn.up", "dyn.worker_up"} <= {p.key for p in snap.points}
        preview = await ad.preview_params({"max_num_seqs": 32})
        assert "--max-num-seqs" in preview
        # prove the rendered override merges with the real compose using the real docker binary
        ov = tmp_path / "dynamo" / "override.yml"
        ov.parent.mkdir()
        ov.write_text(preview)
        v = await ad.validate_config("override", preview)
        if cf.overrides:                       # a variant runs: the console override would replace its worker
            assert not v.ok and any("would replace that stack's worker" in d.message for d in v.diagnostics)
        else:
            assert v.ok and v.authoritative, v.diagnostics
        cfg = await ad.read_config("compose")
        assert cfg.spec.editable is False
