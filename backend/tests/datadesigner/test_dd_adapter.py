"""Data Designer adapter: status, the tool's own yaml config, config scripts (CAS-backed), preview, report,
check-models, show_config, events/metrics — offline, fakes + captured fixtures only."""
from __future__ import annotations

import json
import os
import stat
from pathlib import Path

import httpx
import pytest
from dd_helpers import EXPORTER, FIXTURES, LAUNCHER, FakeRunner, make_ctx, ok, standard_runner, writes_create

from nemo_console.adapters import datadesigner as dd
from nemo_console.adapters.datadesigner import DataDesignerAdapter
from nemo_console.services.runner import DD_CREATE, SubprocessRunner, max_timeout


def codes(st) -> set[str]:  # type: ignore[no-untyped-def]
    return {p.code for p in st.problems}


# ── runner policy ───────────────────────────────────────────────────────────────────────────────
def test_only_create_gets_the_long_cap() -> None:
    assert max_timeout([LAUNCHER, "create", "x"]) == DD_CREATE == 3600.0
    assert max_timeout([LAUNCHER, "preview", "x"]) == 600.0
    assert max_timeout([LAUNCHER, "validate", "x"]) == 600.0
    assert max_timeout([LAUNCHER, "check-models", "x"]) == 600.0
    assert max_timeout([LAUNCHER, "--version"]) == 600.0
    assert max_timeout([EXPORTER, "a.parquet", "b.jsonl"]) == 600.0


def test_binaries_are_allowlisted() -> None:
    from nemo_console.services.runner import ALLOWED_BINARIES
    assert {"data-designer-local", "data-designer-export"} <= ALLOWED_BINARIES
    with pytest.raises(Exception, match="not allowed"):
        SubprocessRunner([]).resolve("data-designer")


# ── status ──────────────────────────────────────────────────────────────────────────────────────
async def test_status_up_with_wrapper_and_yaml_config(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    st = await a.status()
    assert st.system == "datadesigner" and st.health == "up" and st.version == "0.9.3"
    assert st.components[0].name == LAUNCHER and "OpenShell-contained" in (st.components[0].detail or "")
    assert "2 scripts · 2 aliases" in st.summary
    assert st.problems == []
    assert st.endpoints[0].name == "router (generation)" and st.endpoints[0].auth == "bearer"
    assert run.calls[0] == [LAUNCHER, "--version"]


async def test_version_is_cached_between_polls(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    await a.status()
    await a.status()
    assert sum(1 for c in run.calls if c[-1] == "--version") == 1


async def test_not_installed_is_down_with_the_lab_installer_fix(tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    async with httpx.AsyncClient(trust_env=False) as client:
        a = DataDesignerAdapter(make_ctx(tmp_path, FakeRunner(), client))
        st = await a.status()
        assert st.health == "down" and "dd.not_installed" in codes(st)
        p = next(p for p in st.problems if p.code == "dd.not_installed")
        assert p.fix and "clients/data-designer/install.sh" in p.fix and "0.9.3" in p.fix
        # with the lab repo configured, the fix is that repo's installer, by path
        repo = tmp_path / "lab"
        (repo / "clients" / "data-designer").mkdir(parents=True)
        (repo / "clients" / "data-designer" / "install.sh").write_text("#!/bin/sh\n")
        b = DataDesignerAdapter(make_ctx(tmp_path, FakeRunner(), client, repo=repo))
        q = next(p for p in (await b.status()).problems if p.code == "dd.not_installed")
        assert q.fix == f"{repo}/clients/data-designer/install.sh   # pinned data-designer 0.9.3 + the " \
                        "data-designer-local wrapper"


async def test_router_down_is_degraded(make_adapter, router) -> None:  # type: ignore[no-untyped-def]
    router.get("http://127.0.0.1:4000/health/liveliness").mock(side_effect=httpx.ConnectError("refused"))
    a, _ = make_adapter()
    st = await a.status()
    assert st.health == "degraded" and "dd.router_down" in codes(st)


async def test_missing_key_file_is_a_warning(tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    async with httpx.AsyncClient(trust_env=False) as client:
        a = DataDesignerAdapter(make_ctx(tmp_path, standard_runner(), client, key_file=False))
        st = await a.status()
        assert "dd.no_key" in codes(st)
        p = next(p for p in st.problems if p.code == "dd.no_key")
        assert p.severity == "warn" and "mint-key.sh" in (p.fix or "")
        assert not a.ctx.paths["dd_key_file"].exists()


async def test_existing_key_file_is_never_read(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    st = await a.status()
    assert "dd.no_key" not in codes(st)
    # existence check only: the key value never appears anywhere in the status
    assert "sk-FAKE-not-a-real-key" not in json.dumps(st.model_dump())


async def test_missing_tool_config_dir_is_reported_not_created(tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    async with httpx.AsyncClient(trust_env=False) as client:
        a = DataDesignerAdapter(make_ctx(tmp_path, standard_runner(), client, providers=False))
        st = await a.status()
        assert "dd.no_config_dir" in codes(st)
        assert not (tmp_path / "home" / ".data-designer").exists()      # the core never creates it


async def test_no_scripts_dir_is_a_warning(tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    async with httpx.AsyncClient(trust_env=False) as client:
        a = DataDesignerAdapter(make_ctx(tmp_path, standard_runner(), client, scripts=False))
        st = await a.status()
        assert "dd.no_scripts" in codes(st) and a.scripts() == []
        assert a.actions()[0].params[0].choices is None


# ── script discovery and containment ────────────────────────────────────────────────────────────
async def test_only_allowlisted_script_files_are_offered(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    assert a.scripts() == ["reviews_smoke.py", "tiny.yaml"]     # no UPPER, no .txt, no dir, no symlink
    assert a._script_path("reviews_smoke.py").name == "reviews_smoke.py"


@pytest.mark.parametrize("name", ["../outside.py", "UPPER.py", "notes.txt", "subdir.py", "linked.py", "", "a/b.py",
                                  ".hidden.py", "x" * 70])
async def test_script_path_refuses_anything_outside_the_list(make_adapter, name: str) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        a._script_path(name)
    with pytest.raises(KeyError):
        a.config_path(name)


# ── the tool's own yaml config ──────────────────────────────────────────────────────────────────
async def test_yaml_config_is_parsed_and_api_key_is_never_surfaced(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    provs, aliases = a.providers(), a.aliases()
    assert [p["name"] for p in provs] == ["lab-router", "nvidia"]
    assert provs[0]["endpoint"] == "http://127.0.0.1:4000/v1"
    assert all("api_key" not in p and "LITELLM_DD_KEY" not in json.dumps(p) for p in provs)
    assert [x["alias"] for x in aliases] == ["lab-text", "lab-embed"]
    assert aliases[0]["model"] == "glm-5.3-flash-next-local" and aliases[0]["temperature"] == 0.7
    assert aliases[0]["max_tokens"] == 1024.0 and aliases[1]["temperature"] is None


async def test_show_config_action_never_shows_key_fields(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    res = await a.run_action("show_config", {})
    assert res.ok and "lab-router" in res.output and "glm-5.3-flash-next-local" in res.output
    assert "api_key" not in res.output and "LITELLM_DD_KEY" not in res.output and "NVIDIA_API_KEY" not in res.output


async def test_broken_yaml_is_zero_entries_never_an_error(tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    async with httpx.AsyncClient(trust_env=False) as client:
        a = DataDesignerAdapter(make_ctx(tmp_path, standard_runner(), client))
        (tmp_path / "home" / ".data-designer" / "model_configs.yaml").write_text("{ not yaml !!!")
        assert a.aliases() == []
        st = await a.status()
        assert st.health == "up"                                     # failure is data, never an exception


async def test_yaml_toolshape_top_level_key_with_list_is_parsed(tmp_path: Path, router) -> None:  # type: ignore[no-untyped-def]
    """The shape the tool itself writes: one top-level key ('providers'/'model_configs') holding a
    list (regression: only the name→mapping and bare-list shapes were handled, so a real
    ~/.data-designer showed 0 aliases)."""
    home = tmp_path / "home"
    dd = home / ".data-designer"
    dd.mkdir(parents=True, exist_ok=True)
    (dd / "model_providers.yaml").write_text(
        "providers:\n"
        "- name: lab-router\n"
        "  endpoint: http://127.0.0.1:4000/v1\n"
        "  provider_type: openai\n"
        "  api_key: LITELLM_DD_KEY\n"
        "- name: nvidia\n"
        "  endpoint: https://integrate.api.nvidia.com/v1\n"
        "  provider_type: openai\n"
        "  api_key: NVIDIA_API_KEY\n"
    )
    (dd / "model_configs.yaml").write_text(
        "model_configs:\n"
        "- alias: lab-text\n"
        "  model: glm-5.3-flash-next-local\n"
        "  provider: lab-router\n"
        "  inference_parameters:\n"
        "    generation_type: chat-completion\n"
        "    temperature: 1.0\n"
        "    top_p: 0.95\n"
        "    max_tokens: 4096\n"
        "- alias: lab-lightning\n"
        "  model: nemotron-lightning\n"
        "  provider: lab-router\n"
    )
    async with httpx.AsyncClient(trust_env=False) as client:
        a = DataDesignerAdapter(make_ctx(tmp_path, standard_runner(), client, providers=False))
        provs, aliases = a.providers(), a.aliases()
        assert [p["name"] for p in provs] == ["lab-router", "nvidia"]
        assert all("api_key" not in json.dumps(p) for p in provs)
        assert [x["alias"] for x in aliases] == ["lab-text", "lab-lightning"]
        assert aliases[0]["model"] == "glm-5.3-flash-next-local"


# ── config files (the core's CAS/backup/revision machinery) ─────────────────────────────────────
async def test_config_files_specs_and_read(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    specs = {s.id: s for s in a.config_files()}
    assert set(specs) == {"reviews_smoke.py", "tiny.yaml"}
    assert specs["tiny.yaml"].format == "yaml" and specs["reviews_smoke.py"].format == "text"
    assert specs["reviews_smoke.py"].editable and specs["reviews_smoke.py"].authoritative
    assert "EXECUTES" in specs["reviews_smoke.py"].description
    cf = await a.read_config("reviews_smoke.py")
    assert "load_config_builder" in cf.content and cf.sha256 and cf.mtime > 0


async def test_validate_config_runs_the_cli_on_a_temp_copy(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.validate_config("reviews_smoke.py", "# new content\n")
    call = next(c for c in run.calls if c[1:2] == ["validate"])
    assert call[0] == LAUNCHER and call[2].startswith(str(tmp_path / "state" / "datadesigner" / "tmp"))
    assert call[2].endswith(".py")        # the CLI rejects a temp file whose extension is not last
    assert res.ok and res.authoritative and "EXECUTES" in (res.note or "")
    assert not list((tmp_path / "state" / "datadesigner" / "tmp").iterdir())     # temp deleted in finally


async def test_validate_config_reports_the_cli_errors_redacted(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    run.on([LAUNCHER, "validate"], ok("", rc=1, stderr="Config error at line 3\napi_key=sk-abcdefghijklmnop\n"))
    res = await a.validate_config("reviews_smoke.py", "bad")
    assert not res.ok and res.authoritative
    msgs = [d.message for d in res.diagnostics]
    assert "Config error at line 3" in msgs and len(msgs) <= 10
    assert "sk-abcdefghijklmnop" not in json.dumps(res.model_dump())


async def test_validate_unknown_id_is_keyerror(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    with pytest.raises(KeyError):
        await a.validate_config("nope.py", "x")


async def test_post_apply_is_healthy_without_restart(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    res = await a.post_apply("reviews_smoke.py", restart=True)
    assert res.ok and res.post_check == "healthy" and not res.restarted


async def test_script_executing_actions_are_disruptive_admin(make_adapter) -> None:  # type: ignore[no-untyped-def]
    """preview/create/check_models execute the config script locally: they must never be confirm-free."""
    a, _ = make_adapter()
    by_id = {s.id: s for s in a.actions()}
    assert by_id["preview"].danger == "disruptive"
    assert by_id["create"].danger == "disruptive"
    assert by_id["check_models"].danger == "disruptive"
    assert all(s.role == "admin" for s in by_id.values())


# ── preview ─────────────────────────────────────────────────────────────────────────────────────
async def test_preview_runs_the_cli_keeps_the_report_and_exports_records(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("preview", {"script": "reviews_smoke.py", "num_records": 3})
    call = next(c for c in run.calls if c[1:2] == ["preview"])
    assert call[:5] == [LAUNCHER, "preview", str((tmp_path / "home/Datasets/data-designer/scripts/"
                                                   "reviews_smoke.py").resolve()), "-n", "3"]
    assert "--save-results" in call and Path(call[call.index("-o") + 1]).parent.name == "runs"
    run_dir = Path(call[call.index("-o") + 1])
    assert run.cwds[run.calls.index(call)] == run_dir      # the CLI's scratch files land in the run dir
    assert next(c for c in run.calls if c[0] == EXPORTER)[1].endswith("dataset.parquet")
    assert stat.S_IMODE(os.stat(run_dir).st_mode) == 0o700
    assert stat.S_IMODE(os.stat(run_dir / "run.json").st_mode) == 0o600
    meta = json.loads((run_dir / "run.json").read_text())
    assert meta["verdict"] == "succeeded" and meta["mode"] == "preview" and meta["records"] == 2
    assert meta["num_records_requested"] == 3 and meta["preview_dir"] == "preview_results_20261023_222454"
    assert (run_dir / "records.jsonl").read_text().count("\n") == 2
    assert "sample_records_browser.html" in res.output and res.ok
    # records are data: the event carries metadata only, never record text
    ev = (await a.recent_events())[0]
    assert ev.kind == "dd.run" and ev.data["num_records"] == 2 and ev.data["mode"] == "preview"
    assert "Books" not in json.dumps(ev.data)      # record contents never enter events


async def test_preview_cli_failure_is_data(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    run.on([LAUNCHER, "preview"], ok("", rc=1, stderr="Config error: unknown model alias"))
    res = await a.run_action("preview", {"script": "reviews_smoke.py"})
    assert not res.ok and "unknown model alias" in res.output
    rid = str((await a.recent_events())[0].data["run_id"])
    meta = json.loads((a._runs_dir / rid / "run.json").read_text())
    assert meta["verdict"] == "failed" and "unknown model alias" in meta["error"]


async def test_preview_without_convertible_output_is_an_error(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    run.on([EXPORTER], ok("", rc=3, stderr="boom"))
    res = await a.run_action("preview", {"script": "reviews_smoke.py"})
    assert not res.ok
    rid = str((await a.recent_events())[0].data["run_id"])
    assert json.loads((a._runs_dir / rid / "run.json").read_text())["verdict"] == "error"


# ── report / check-models ───────────────────────────────────────────────────────────────────────
async def test_report_shows_a_markdown_table_of_the_first_records(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    await a.run_action("preview", {"script": "reviews_smoke.py"})
    rep = await a.run_action("report", {"run_id": ""})                    # empty = newest
    assert rep.ok and "| product_category | review |" in rep.output and "| --- | --- |" in rep.output
    assert "Books" in rep.output and "records.jsonl" in rep.output
    rep2 = await a.run_action("report", {"run_id": "20990101T000000-000-nope"})
    assert not rep2.ok and rep2.output == "unknown run id"


async def test_check_models_passes_the_script_through(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, run = make_adapter()
    res = await a.run_action("check_models", {"script": "tiny.yaml"})
    call = next(c for c in run.calls if c[1:2] == ["check-models"])
    assert call[0] == LAUNCHER and call[2].endswith("tiny.yaml")
    assert res.ok and "lab-router: ok" in res.output


# ── records / files (API support) ───────────────────────────────────────────────────────────────
async def test_records_page_paginates_and_caps_cell_size(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    await a.run_action("preview", {"script": "reviews_smoke.py"})
    rid = str((await a.recent_events())[0].data["run_id"])
    page = a.records_page(rid, limit=1, offset=0)
    assert page["run_id"] == rid and page["total"] == 2 and page["columns"] == ["product_category", "review"]
    assert page["rows"][0]["product_category"] == "Books" and page["truncated"] is False
    page2 = a.records_page(rid, limit=50, offset=1)
    assert len(page2["rows"]) == 1 and page2["rows"][0]["product_category"] == "Electronics"
    page3 = a.records_page(rid, limit=50, offset=9)
    assert page3["rows"] == []
    # a long cell is truncated to 2 KiB with a flag, no marker fields
    rid2, run_dir2 = a._new_run_dir("reviews_smoke.py")
    (run_dir2 / "run.json").write_text(json.dumps({"run_id": rid2, "records": 1, "verdict": "succeeded"}))
    (run_dir2 / "records.jsonl").write_text(json.dumps({"c": "x" * 5000}) + "\n")
    page4 = a.records_page(rid2, limit=10, offset=0)
    assert len(page4["rows"][0]["c"]) == dd.CELL_CAP and page4["truncated"] is True
    with pytest.raises(KeyError):
        a.records_page("20990101T000000-000-nope", 10, 0)
    with pytest.raises(KeyError):
        a.records_page("../escape", 10, 0)


async def test_run_files_lists_relpaths_and_sizes(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    await a.run_action("preview", {"script": "reviews_smoke.py"})
    rid = str((await a.recent_events())[0].data["run_id"])
    files = a.run_files(rid)["files"]
    rels = {f["path"] for f in files}
    assert "records.jsonl" in rels and "run.json" in rels
    assert "preview_results_20261023_222454/dataset.parquet" in rels
    assert "preview_results_20261023_222454/sample_records/sample_records_browser.html" in rels
    parquet = next(f for f in files if f["path"].endswith("dataset.parquet"))
    assert parquet["bytes"] == (FIXTURES / "preview-artifacts" / "dataset.parquet").stat().st_size
    with pytest.raises(KeyError):
        a.run_files("20990101T000000-000-nope")


# ── events / metrics ────────────────────────────────────────────────────────────────────────────
async def test_metrics_and_runs_by_verdict(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    await a.run_action("preview", {"script": "reviews_smoke.py"})
    pts = {p.key: p.value for p in (await a.metrics()).points}
    assert pts["dd.scripts"] == 2.0 and pts["dd.runs"] == 1.0 and pts["dd.live_running"] == 0.0
    assert pts["dd.last_records"] == 2.0
    assert pts["dd.runs_by_verdict{verdict=succeeded}"] == 1.0
    assert pts["dd.runs_by_verdict{verdict=failed}"] == 0.0


async def test_a_running_run_left_by_a_previous_process_reads_as_interrupted(make_adapter) -> None:  # type: ignore[no-untyped-def]
    import asyncio as _asyncio
    gate = _asyncio.Event()

    async def gated(argv):  # type: ignore[no-untyped-def]
        await gate.wait()
        return writes_create()(argv)
    _, run = make_adapter()
    run.on([LAUNCHER, "create"], gated)
    old, _ = make_adapter(run)
    await old.run_action("create", {"script": "reviews_smoke.py", "num_records": 2})
    new, _ = make_adapter(standard_runner())
    ev = (await new.recent_events())[0]
    assert ev.data["verdict"] == "interrupted"
    pts = {p.key: p.value for p in (await new.metrics()).points}
    assert pts["dd.runs_by_verdict{verdict=interrupted}"] == 1.0
