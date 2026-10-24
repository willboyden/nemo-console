"""Data Designer `create`: the ONE background run, destination containment, formats, records cap, interrupted
runs, and the core's confirm + param checks. Offline: fakes + captured fixtures only."""
from __future__ import annotations

import asyncio
import csv
import io
import json
import os
import stat
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import httpx
import pytest
from dd_helpers import EXPORTER, LAUNCHER, export_responder, make_ctx, ok, standard_runner, writes_create

from nemo_console.adapters.datadesigner import MAX_PREVIEW_RECORDS, DataDesignerAdapter
from nemo_console.config import NO_USER_ENV, Settings
from nemo_console.container import build_container
from nemo_console.main import create_app

LIVE = {"script": "reviews_smoke.py", "num_records": 10}


def codes(res) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in res.problems}


async def _finish(a: DataDesignerAdapter) -> None:
    assert a._live is not None
    await asyncio.wait_for(a._live.task, 5)


def _run_json(a: DataDesignerAdapter, run_id: str) -> dict[str, Any]:
    return json.loads((a._runs_dir / run_id / "run.json").read_text())


def datasets_root(a: DataDesignerAdapter) -> Path:
    return a.ctx.paths["datasets_root"]


# ── start in the background ────────────────────────────────────────────────────────────────────
async def test_create_starts_in_the_background_and_returns_at_once(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()

    async def gated(argv: list[str]) -> Any:
        await gate.wait()
        return writes_create()(argv)
    run.on([LAUNCHER, "create"], gated)
    a, run2 = make_adapter(run)
    res = await asyncio.wait_for(a.run_action("create", dict(LIVE)), 2)     # does not wait for the run
    assert res.ok and "Started dataset build" in res.output and "in the background" in res.output
    rid = a._live_run_id()
    assert rid is not None and rid.endswith("-reviews_smoke.py")
    run_dir = a._runs_dir / rid
    meta = _run_json(a, rid)
    assert meta["verdict"] == "running" and meta["mode"] == "create"
    assert meta["dataset_name"] == "dataset_reviews_smoke" and meta["destination"] == "dataset_reviews_smoke"
    assert meta["format"] == "parquet" and meta["num_records_requested"] == 10
    assert stat.S_IMODE(os.stat(run_dir).st_mode) == 0o700
    assert stat.S_IMODE(os.stat(run_dir / "run.json").st_mode) == 0o600
    await asyncio.sleep(0.02)
    call = next(c for c in run2.calls if c[1:2] == ["create"])
    assert call[:2] == [LAUNCHER, "create"] and "--no-tui" in call and "--num-records" in call
    assert call[call.index("--dataset-name") + 1] == "dataset_reviews_smoke"
    assert run2.cwds[-1] == run_dir
    st = await a.status()
    assert any(c.name == "create reviews_smoke.py" and c.state == "running" for c in st.components)
    ev = (await a.recent_events())[0]
    assert ev.data["verdict"] == "running" and "running for" in ev.summary
    pts = {p.key: p.value for p in (await a.metrics()).points}
    assert pts["dd.live_running"] == 1.0
    rep = await a.run_action("report", {"run_id": rid})
    assert rep.ok and "Running for" in rep.output
    gate.set()
    await _finish(a)
    assert _run_json(a, rid)["verdict"] == "succeeded" and a._live_run_id() is None


async def test_one_create_at_a_time(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()

    async def gated(argv: list[str]) -> Any:
        await gate.wait()
        return writes_create()(argv)
    run.on([LAUNCHER, "create"], gated)
    a, _ = make_adapter(run)
    first = await a.run_action("create", dict(LIVE))
    second = await a.run_action("create", {"script": "tiny.yaml"})
    assert first.ok and not second.ok and codes(second) == {"dd.create.busy"}
    assert "already running" in second.output and str(a._live_run_id()) in second.output
    await asyncio.sleep(0.02)
    assert len([c for c in run.calls if c[1:2] == ["create"]]) == 1
    assert len(list(a._runs_dir.iterdir())) == 1                    # the refused request made no run directory
    gate.set()
    await _finish(a)
    third = await a.run_action("create", {"script": "tiny.yaml"})   # free again once the first ended
    assert third.ok
    await _finish(a)


# ── completion: records.jsonl + the user copy ──────────────────────────────────────────────────
async def test_completion_records_metadata_and_copies_the_parquet_batches(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    res = await a.run_action("create", {"script": "reviews_smoke.py", "num_records": 2, "format": "parquet",
                                        "dataset_name": "smoke_set", "destination_subdir": "smoke/nested"})
    assert res.ok
    rid = str(a._live_run_id())
    await _finish(a)
    meta = _run_json(a, rid)
    assert meta["verdict"] == "succeeded" and meta["records"] == 2 and meta["batches"] == 1
    assert meta["dataset_name"] == "smoke_set" and meta["destination"] == "smoke/nested"
    dest = datasets_root(a) / "smoke" / "nested"
    assert (dest / "batch_00000.parquet").is_file()
    assert (a._runs_dir / rid / "records.jsonl").read_text().count("\n") == 2
    ev = (await a.recent_events())[0]
    assert ev.data["num_records"] == 2 and ev.data["dataset_name"] == "smoke_set" and ev.data["format"] == "parquet"
    assert "succeeded" in ev.summary and "smoke_set" in ev.summary
    rep = await a.run_action("report", {"run_id": rid})
    assert rep.ok and "| product_category | review |" in rep.output and "batch" in rep.output
    pts = {p.key: p.value for p in (await a.metrics()).points}
    assert pts["dd.last_records"] == 2.0 and pts["dd.runs_by_verdict{verdict=succeeded}"] == 1.0


@pytest.mark.parametrize("fmt,check", [
    ("csv", "header once"),
    ("jsonl", "lines"),
    ("json", "array"),
])
async def test_converted_formats(make_adapter, fmt: str, check: str) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    res = await a.run_action("create", {"script": "reviews_smoke.py", "num_records": 2, "format": fmt})
    assert res.ok
    rid = str(a._live_run_id())
    await _finish(a)
    assert _run_json(a, rid)["verdict"] == "succeeded"
    target = datasets_root(a) / "dataset_reviews_smoke" / f"dataset_reviews_smoke.{fmt}"
    assert target.is_file()
    if fmt == "jsonl":
        lines = target.read_text().splitlines()
        assert len(lines) == 2 and json.loads(lines[0])["product_category"] == "Books"
    elif fmt == "csv":
        rows = list(csv.reader(io.StringIO(target.read_text())))
        assert rows[0] == sorted(["product_category", "review"]) and len(rows) == 3   # header + 2 records
        # regression: the console's records.jsonl is ALWAYS jsonl, even when the user's copy is csv
        rec_lines = (a.ctx.state_dir / "datadesigner" / "runs" / rid / "records.jsonl").read_text().splitlines()
        assert len(rec_lines) == 2 and json.loads(rec_lines[0])["product_category"] == "Books"
    else:
        data = json.loads(target.read_text())
        assert isinstance(data, list) and len(data) == 2 and data[1]["product_category"] == "Electronics"


async def test_multi_batch_json_and_csv_and_the_records_cap(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([LAUNCHER, "create"], writes_create(batches=3, records=6))
    a, _ = make_adapter(run)
    res = await a.run_action("create", {"script": "reviews_smoke.py", "num_records": 6, "format": "json"})
    assert res.ok
    rid = str(a._live_run_id())
    await _finish(a)
    meta = _run_json(a, rid)
    assert meta["verdict"] == "succeeded" and meta["batches"] == 3 and meta["records"] == 6
    target = datasets_root(a) / "dataset_reviews_smoke" / "dataset_reviews_smoke.json"
    data = json.loads(target.read_text())
    assert isinstance(data, list) and len(data) == 6
    assert (a._runs_dir / rid / "records.jsonl").read_text().count("\n") == 6   # 2 rows per batch × 3 batches
    # the console copy is capped even when the dataset is bigger
    big = [{"product_category": f"c{i}", "review": "x" * 20} for i in range(MAX_PREVIEW_RECORDS + 500)]
    run.on([EXPORTER], export_responder(rows_override=big))
    run.on([LAUNCHER, "create"], writes_create(batches=1, records=MAX_PREVIEW_RECORDS + 500))
    a2, _ = make_adapter(run)
    await a2.run_action("create", {"script": "tiny.yaml", "num_records": 10, "format": "parquet",
                                   "dataset_name": "big", "destination_subdir": "big"})
    rid2 = str(a2._live_run_id())
    await _finish(a2)
    assert _run_json(a2, rid2)["verdict"] == "succeeded"
    assert _run_json(a2, rid2)["records"] == MAX_PREVIEW_RECORDS + 500
    assert (a2._runs_dir / rid2 / "records.jsonl").read_text().count("\n") == MAX_PREVIEW_RECORDS
    page = a2.records_page(rid2, limit=5, offset=0)
    assert page["total"] == MAX_PREVIEW_RECORDS + 500 and len(page["rows"]) == 5


# ── refusals and failure-is-data ────────────────────────────────────────────────────────────────
async def test_bad_values_are_refused_without_running(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    for params, why in (({"script": "nope.py"}, "unknown script"),
                        ({**LIVE, "num_records": 0}, "num_records"),
                        ({**LIVE, "num_records": True}, "num_records"),
                        ({**LIVE, "num_records": 100001}, "num_records"),
                        ({**LIVE, "dataset_name": "bad name"}, "dataset_name"),
                        ({**LIVE, "destination_subdir": "/etc"}, "destination_subdir"),
                        ({**LIVE, "destination_subdir": "../outside"}, "destination_subdir"),
                        ({**LIVE, "destination_subdir": "a/../b"}, "destination_subdir"),
                        ({**LIVE, "destination_subdir": "a//b"}, "destination_subdir"),
                        ({**LIVE, "format": "xlsx"}, "format")):
        r = await a.run_action("create", params)
        assert not r.ok and why in r.output, params
    assert not [c for c in run.calls if c[1:2] == ["create"]] and a._live is None


async def test_destination_may_not_escape_the_datasets_root(tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    async with httpx.AsyncClient(trust_env=False) as client:
        a = DataDesignerAdapter(make_ctx(tmp_path, standard_runner(), client))
        outside = tmp_path / "outside"
        outside.mkdir()
        datasets_root(a).mkdir(parents=True, exist_ok=True)
        (datasets_root(a) / "link").symlink_to(outside, target_is_directory=True)
        (datasets_root(a) / "blocker").write_text("a file, not a directory\n")
        r = await a.run_action("create", {**LIVE, "destination_subdir": "link/evil"})
        assert not r.ok and "outside the datasets root" in r.output
        r2 = await a.run_action("create", {**LIVE, "destination_subdir": "blocker"})
        assert not r2.ok and "not a directory" in r2.output
        assert a._live is None


async def test_failed_create_is_data(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    run.on([LAUNCHER, "create"], ok("", rc=1, stderr="Model call failed: Bearer sk-abcdefghijklmnop"))
    await a.run_action("create", dict(LIVE))
    rid = str(a._live_run_id())
    await _finish(a)
    meta = _run_json(a, rid)
    assert meta["verdict"] == "failed" and "Model call failed" in meta["error"]
    assert "sk-abcdefghijklmnop" not in (a._runs_dir / rid / "run.json").read_text()
    rep = await a.run_action("report", {"run_id": rid})
    assert "did not complete" in rep.output and "sk-abcdefghijklmnop" not in rep.output


async def test_create_without_metadata_or_batches_is_an_error(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([LAUNCHER, "create"], writes_create(with_metadata=False))
    a, _ = make_adapter(run)
    await a.run_action("create", dict(LIVE))
    rid = str(a._live_run_id())
    await _finish(a)
    assert _run_json(a, rid)["verdict"] == "error" and "metadata.json" in _run_json(a, rid)["error"]
    run.on([LAUNCHER, "create"], writes_create(with_file_paths=False))
    b, _ = make_adapter(run)
    await b.run_action("create", dict(LIVE))
    rid2 = str(b._live_run_id())
    await _finish(b)
    assert _run_json(b, rid2)["verdict"] == "error" and "batch" in _run_json(b, rid2)["error"]


async def test_export_failure_fails_the_run_but_keeps_the_batches(make_adapter) -> None:  # type: ignore[no-untyped-def]
    run = standard_runner()
    run.on([EXPORTER], ok("", rc=2, stderr="parquet unreadable"))
    a, _ = make_adapter(run)
    await a.run_action("create", dict(LIVE))
    rid = str(a._live_run_id())
    await _finish(a)
    meta = _run_json(a, rid)
    assert meta["verdict"] == "failed" and "converting" in meta["error"]
    assert (datasets_root(a) / "dataset_reviews_smoke" / "batch_00000.parquet").is_file()   # copy already done


# ── interrupted ─────────────────────────────────────────────────────────────────────────────────
async def test_console_stop_cancels_the_build_and_marks_it_interrupted(make_adapter) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()

    async def gated(argv: list[str]) -> Any:
        await gate.wait()
        return writes_create()(argv)
    run.on([LAUNCHER, "create"], gated)
    a, _ = make_adapter(run)
    await a.run_action("create", dict(LIVE))
    rid = str(a._live_run_id())
    assert a._live is not None
    await asyncio.sleep(0.02)
    a._live.task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await a._live.task
    meta = _run_json(a, rid)
    assert meta["verdict"] == "interrupted" and meta["interrupted_by"] == "console stop"
    rep = await a.run_action("report", {"run_id": rid})
    assert "Interrupted" in rep.output


# ── through the core: confirm required, params validated against the ActionSpec ─────────────────
@pytest.fixture
def isolated(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Iterator[None]:
    for k in list(os.environ):
        if k.startswith("NEMO_CONSOLE_"):
            monkeypatch.delenv(k)
    monkeypatch.setenv(NO_USER_ENV, "1")
    (tmp_path / "home").mkdir()
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    yield


async def test_core_requires_confirm_and_validates_create_params(isolated, tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    gate = asyncio.Event()
    run = standard_runner()

    async def gated(argv: list[str]) -> Any:
        await gate.wait()
        return writes_create()(argv)
    run.on([LAUNCHER, "create"], gated)
    async with httpx.AsyncClient(trust_env=False) as http:
        a = DataDesignerAdapter(make_ctx(tmp_path, run, http))
        cfg = Settings(state_dir=tmp_path / "state", frontend_dir=tmp_path / "front", background_tasks=False,
                       trust_loopback=True)
        container = build_container(cfg, adapters={"datadesigner": a}, runner=run, http=http)  # type: ignore[arg-type]
        app = create_app(cfg, container)
        url = "/api/v1/systems/datadesigner/actions/create"
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=("127.0.0.1", 5000)),
                                     base_url="http://127.0.0.1:8793", headers={"X-Console": "1"}) as c:
            spec = next(s for s in (await c.get("/api/v1/systems/datadesigner/actions")).json()
                        if s["id"] == "create")
            assert spec["danger"] == "disruptive" and spec["role"] == "admin"
            assert {p["key"]: p["default"] for p in spec["params"]} == {
                "script": "reviews_smoke.py", "num_records": 100, "dataset_name": None,
                "destination_subdir": None, "format": "parquet"}
            r = await c.post(url, json={"params": dict(LIVE)})
            assert r.status_code == 400 and r.json()["error"]["code"] == "confirm_required"
            for bad in ({"num_records": 0}, {"num_records": "2"}, {"format": "xlsx"}, {"script": "nope.py"},
                        {"dataset_name": "bad\nname"}, {"unknown": 1}):
                r = await c.post(url, json={"params": {**LIVE, **bad}, "confirm": True})
                assert r.status_code == 422 and r.json()["error"]["code"] == "invalid_param", bad
            # values that pass the spec but violate the adapter's shape rules are refused by the adapter (200)
            r = await c.post(url, json={"params": {**LIVE, "destination_subdir": "../out"}, "confirm": True})
            assert r.status_code == 200 and r.json()["ok"] is False and "datasets root" in r.json()["output"]
            assert not [x for x in run.calls if x[1:2] == ["create"]]
            r = await c.post(url, json={"params": dict(LIVE), "confirm": True})
            assert r.status_code == 200 and r.json()["ok"] is True
        rid = str(a._live_run_id())
        # while the build runs (no records.jsonl yet) the records endpoint answers an EMPTY page, not 404
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=("127.0.0.1", 5000)),
                                     base_url="http://127.0.0.1:8793") as c2:
            live = await c2.get(f"/api/v1/systems/datadesigner/runs/{rid}/records")
            assert live.status_code == 200 and live.json()["rows"] == [] and live.json()["total"] == 0
        gate.set()
        await _finish(a)
        # the run artifacts endpoints: viewer GET, unknown runs 404, other systems 404
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app, client=("127.0.0.1", 5000)),
                                     base_url="http://127.0.0.1:8793") as c2:
            rec = await c2.get(f"/api/v1/systems/datadesigner/runs/{rid}/records?limit=1")
            assert rec.status_code == 200 and rec.json()["total"] == 2 and len(rec.json()["rows"]) == 1
            files = await c2.get(f"/api/v1/systems/datadesigner/runs/{rid}/files")
            assert files.status_code == 200 and any(f["path"] == "records.jsonl" for f in files.json()["files"])
            assert (await c2.get("/api/v1/systems/datadesigner/runs/nope/records")).status_code == 404
            assert (await c2.get("/api/v1/systems/switchyard/runs/x/records")).status_code == 404
        container.store.close()
