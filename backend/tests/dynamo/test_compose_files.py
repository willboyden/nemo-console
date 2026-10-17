"""Addendum 9.6: the compose files IN USE come from the containers' labels (the lab's Super VL variant runs from
docker-compose.yml + docker-compose.super-vl.yml, measured 2026-10-17), the label is untrusted input, Params and the
console override are refused beside a repo override file, the version is a Dynamo tag (never the nginx gateway's
digest), the real frontend behind the gateway is a component and a log source, and the worker's GPUs come from the
merged config (fixtures/merged_config_super_vl.json: the real `docker compose ... config --format json` of the two
files, trimmed to images/commands/devices, GPU UUIDs anonymised)."""
from __future__ import annotations

import copy
from pathlib import Path
from typing import Any

from nemo_console.adapters.base import CommandResult
from nemo_console.adapters.dynamo import CardPolicy, ComposeFiles, image_version, merged_worker_uuids

from .helpers import FakeRunner, frontend_doc, fx, inspect_json, is_docker, ok, worker_doc
from .test_adapter import FE, WK, compose_calls, gpu_doc, http_live

SV = "ai-lab-dynamo-dynamo-sv-frontend-1"
CARD_A, CARD_B = "GPU-00000000-0000-0000-0000-000000000001", "GPU-00000000-0000-0000-0000-000000000002"
SV_IMAGE = ("nvcr.io/nvidia/ai-dynamo/vllm-runtime:1.6.0-nemotron-3.5-super-vl-dev.1"
            "@sha256:3a55cf2d16db5ffc026326a70bf0c56ac09661fdca5508404520382a49403e4d")
NGINX = "nginx:1.31.6-alpine@sha256:df221db836e1754089190208cee7eeda94f233197056426eda74a43ab1abeac2"
CHATTERBOX = "ai-lab-chatterbox-tts-tts-1"
BOLT = "ai-lab-vllm-nemotron-bolt-nemotron-bolt-1"


def sv_override(compose: Path) -> Path:
    p = compose.parent / "docker-compose.super-vl.yml"
    p.write_text("services:\n  dynamo-worker:\n    restart: \"no\"\n")
    return p


def labelled(doc: dict[str, Any], files: list[Path] | str, workdir: Path | str, *, name: str | None = None,
             service: str | None = None, image: str | None = None) -> dict[str, Any]:
    """A Dynamo container shaped like the live inspect of 2026-10-17 (labels carry BOTH files, comma-separated)."""
    d = copy.deepcopy(doc)
    lb = d["Config"]["Labels"]
    lb["com.docker.compose.project.config_files"] = files if isinstance(files, str) else ",".join(str(f) for f in files)
    lb["com.docker.compose.project.working_dir"] = str(workdir)
    if name:
        d["Name"] = "/" + name
    if service:
        lb["com.docker.compose.service"] = service
    if image:
        d["Config"]["Image"] = image
    return d


def sv_stack(compose: Path, files: list[Path] | None = None) -> list[dict[str, Any]]:
    """The Super VL stack: nginx at dynamo-frontend (no GPU), the real frontend, the worker on BOTH cards."""
    files = files or [compose, sv_override(compose)]
    fe = labelled(frontend_doc(), files, compose.parent, image=NGINX)
    sv = labelled(frontend_doc(), files, compose.parent, name=SV, service="dynamo-sv-frontend", image=SV_IMAGE)
    wk = labelled(worker_doc(), files, compose.parent, image=SV_IMAGE)
    wk["HostConfig"]["DeviceRequests"][0]["DeviceIDs"] = [CARD_B, CARD_A]
    return [fe, sv, wk]


def wire_docs(runner: FakeRunner, docs: list[dict[str, Any]], ps: str = "", merged: str | None = None) -> None:
    runner.on(lambda a: a[:2] == ["docker", "compose"] and a[-3:] == ["config", "--format", "json"],
              ok(merged) if merged is not None else CommandResult(1, "", "no merged config"))
    runner.on(is_docker("inspect"), lambda a: inspect_json(*[d for d in docs if d["Name"].lstrip("/") in set(a[2:])]))
    runner.on(is_docker("ps"), ok(ps))
    runner.on(is_docker("compose"), ok("done"))


def files_of(argv: list[str]) -> list[str]:
    return [argv[i + 1] for i, a in enumerate(argv) if a == "-f"]


# ── lifecycle with the files in use ────────────────────────────────────────────────────────────
async def test_super_vl_stack_lifecycle_uses_exactly_the_label_files(make, runner, compose, mock) -> None:  # type: ignore[no-untyped-def]
    docs = sv_stack(compose)
    ov = compose.parent / "docker-compose.super-vl.yml"
    wire_docs(runner, docs, merged=fx("merged_config_super_vl.json"))
    ad = make()
    ad._write_override("services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm]\n")   # active console override
    http_live(mock)
    for action in ("down", "restart", "up"):
        runner.calls.clear()
        res = await ad.run_action(action, {})
        [call] = compose_calls(runner)
        assert files_of(call) == [str(compose.resolve()), str(ov.resolve())], action     # never the console override
        assert call[call.index("--project-directory") + 1] == str(compose.parent)
        if action == "down":
            assert res.ok and call[-1] == "down" and "--profile" in call and "-v" not in call
        else:
            assert call[-2:] == ["dynamo-frontend", "dynamo-worker"] and ("--force-recreate" in call) == (action == "restart")
    # with no Dynamo container at all, the configured base applies (plus the console override, as before)
    runner.rules.clear()
    wire_docs(runner, [])
    runner.calls.clear()
    await ad.run_action("down", {})
    [call] = compose_calls(runner)
    assert files_of(call) == [str(compose.resolve()), str(ad._override_path())]


async def test_console_override_in_the_label_is_the_base_stack(make, runner, compose) -> None:  # type: ignore[no-untyped-def]
    ad = make()
    own = ad._override_path()
    own.parent.mkdir(parents=True, exist_ok=True)
    own.write_text("services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm]\n")
    wk = labelled(worker_doc(), [compose, own], compose.parent)
    wire_docs(runner, [wk])
    cf = await ad._compose_files()
    assert cf.files == (compose.resolve(),) and cf.overrides == () and cf.error is None and cf.from_label
    await ad.run_action("down", {})
    assert files_of(compose_calls(runner)[0]) == [str(compose.resolve()), str(own)]


async def test_untrusted_labels_refuse_every_lifecycle_action(make, runner, compose, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    outside = tmp_path / "elsewhere"
    outside.mkdir()
    (outside / "docker-compose.evil.yml").write_text("services: {}\n")
    (compose.parent / "escape.yml").symlink_to(outside / "docker-compose.evil.yml")
    (compose.parent / "notes.txt").write_text("x\n")
    (compose.parent / "sub").mkdir()
    (compose.parent / "sub" / "docker-compose.yml").write_text("services: {}\n")
    (compose.parent / "stray.yml").write_text("services: {}\n")
    bad_labels = [
        ([compose, compose.parent / "sub" / "docker-compose.yml"], compose.parent, "not a compose file inside"),  # deeper
        ([compose, compose.parent / "stray.yml"], compose.parent, "not a compose file inside"),   # not a compose name
        ([compose, outside / "docker-compose.evil.yml"], compose.parent, "not a compose file inside"),
        ([compose, compose.parent / "escape.yml"], compose.parent, "not a compose file inside"),   # symlink out
        ([compose, compose.parent / "gone.yml"], compose.parent, "does not exist"),
        ([compose, compose.parent / "notes.txt"], compose.parent, "not a compose file inside"),
        ("docker-compose.yml", compose.parent, "not an absolute path"),
        ([compose], outside, "project directory"),
        (",".join([str(compose)] * 9), compose.parent, "more than 8"),
    ]
    for files, workdir, why in bad_labels:
        runner.rules.clear()
        runner.calls.clear()
        wire_docs(runner, [labelled(worker_doc(), files, workdir)])
        ad = make()
        for action in ("up", "down", "restart", "up_mock"):
            res = await ad.run_action(action, {})
            assert not res.ok and res.problems[0].code == "dynamo.compose.untrusted", (files, action)
            assert why in res.output, (why, res.output)
        assert compose_calls(runner) == []                                      # nothing ran, no fallback to base
        assert not (await ad.apply_params({"max_num_seqs": 8})).ok
        assert not ad._override_path().exists()


async def test_containers_started_from_different_files_are_refused(make, runner, compose) -> None:  # type: ignore[no-untyped-def]
    ov = sv_override(compose)
    wk = labelled(worker_doc(), [compose, ov], compose.parent)
    fe = labelled(frontend_doc(), [compose], compose.parent)
    wire_docs(runner, [wk, fe])
    res = await make().run_action("down", {})
    assert not res.ok and "different compose files" in res.output and compose_calls(runner) == []


async def test_a_foreign_container_label_is_ignored(make, runner, compose) -> None:  # type: ignore[no-untyped-def]
    wk = labelled(worker_doc(), "/etc/evil.yml", "/etc")
    wk["Config"]["Labels"]["com.docker.compose.project"] = "someone-else"      # not ours: its label is not read
    wire_docs(runner, [wk])
    cf = await make()._compose_files()
    assert cf.error is None and cf.files == (compose.resolve(),) and not cf.from_label


# ── Params and the console override beside a repo override ─────────────────────────────────────
async def test_params_and_console_override_are_refused_while_an_override_file_is_in_use(make, runner, compose, mock) -> None:  # type: ignore[no-untyped-def]
    wire_docs(runner, sv_stack(compose), merged=fx("merged_config_super_vl.json"))
    http_live(mock)
    ad = make()
    res = await ad.apply_params({"max_num_seqs": 8})
    assert not res.ok and "docker-compose.yml, docker-compose.super-vl.yml" in res.detail
    assert "restates" not in res.detail and "would replace that stack's worker" in res.detail
    assert not ad._override_path().exists() and compose_calls(runner) == []
    v = await ad.validate_config("override", "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm]\n")
    assert not v.ok and any("would replace that stack's worker" in d.message for d in v.diagnostics)
    assert (await ad.validate_config("override", "")).ok                    # emptying it (revert) is always fine
    st = await ad.status()
    info = next(p for p in st.problems if p.code == "dynamo.compose.override")
    assert info.severity == "info" and "docker-compose.super-vl.yml" in info.message
    assert any("would replace that stack's worker" in w for w in ad.param_warnings({"max_num_seqs": 8}))
    post = await ad.post_apply("override", restart=True)                     # re-checked even after validation
    assert not post.ok and post.post_check == "skipped" and compose_calls(runner) == []
    mock_res = await ad.run_action("up_mock", {})
    assert not mock_res.ok and mock_res.problems[0].code == "dynamo.mock.override_in_use"


async def test_base_stack_params_still_apply(make, runner, compose, mock) -> None:  # type: ignore[no-untyped-def]
    wire_docs(runner, [labelled(worker_doc(), [compose], compose.parent)])
    ad = make()
    assert (await ad._compose_files()).overrides == ()
    assert ad._refusal(await ad._compose_files(), "Params") is None


# ── status: version, components, start fixes, GPUs ─────────────────────────────────────────────
async def test_version_is_the_dynamo_tag_never_the_gateway_digest(make, runner, compose, mock) -> None:  # type: ignore[no-untyped-def]
    wire_docs(runner, sv_stack(compose), merged=fx("merged_config_super_vl.json"))
    http_live(mock)
    st = await make().status()
    assert st.version == "1.6.0-nemotron-3.5-super-vl-dev.1"
    assert image_version({"Config": {"Image": NGINX}}) is None
    assert image_version({"Config": {"Image": "nvcr.io/nvidia/ai-dynamo/vllm-runtime@sha256:abc"}}) is None
    assert image_version({"Config": {"Image": "nvcr.io/nvidia/ai-dynamo/vllm-runtime:1.5.0"}}) == "1.5.0"
    assert image_version({"Config": {"Image": "localhost:5000/ai-dynamo/x"}}) is None
    # only the gateway left (the Dynamo containers are gone): no version at all, not nginx's
    runner.rules.clear()
    wire_docs(runner, [sv_stack(compose)[0]])
    assert (await make().status()).version is None


async def test_real_frontend_is_a_component_and_gateway_detail(make, runner, compose, mock) -> None:  # type: ignore[no-untyped-def]
    wire_docs(runner, sv_stack(compose), merged=fx("merged_config_super_vl.json"))
    http_live(mock)
    st = await make().status()
    comps = {c.name: c for c in st.components}
    assert list(comps) == [FE, SV, WK] and comps[SV].state == "running" and comps[SV].image == SV_IMAGE
    assert (comps[FE].detail or "").startswith("gateway to dynamo-sv-frontend")
    assert st.health == "up"


async def test_base_model_checks_are_not_raised_for_a_variant_worker(make, runner, compose, mock) -> None:  # type: ignore[no-untyped-def]
    # Super VL's worker runs without --dyn-enable-structural-tag and with vLLM's --reasoning-parser (live 2026-10-17);
    # those two checks were measured on the base model and their fix is Params, which a variant refuses.
    docs = sv_stack(compose)
    docs[2]["Config"]["Cmd"] = [c for c in docs[2]["Config"]["Cmd"] if c != "--dyn-enable-structural-tag"] + [
        "--reasoning-parser", "nemotron_v3"]
    wire_docs(runner, docs, merged=fx("merged_config_super_vl.json"))
    http_live(mock)
    codes = {p.code for p in (await make().status()).problems}
    assert not {"dynamo.structural_tag.off", "dynamo.parser.dialect"} & codes and "dynamo.compose.override" in codes


async def test_start_fixes_never_start_the_base_stack_over_a_variant(make, runner, compose, mock) -> None:  # type: ignore[no-untyped-def]
    docs = sv_stack(compose)
    docs[1]["State"].update(Running=False, Status="exited", ExitCode=1)      # the real frontend died behind the gateway
    wire_docs(runner, docs, merged=fx("merged_config_super_vl.json"))
    http_live(mock)
    st = await make().status()
    p = next(p for p in st.problems if p.code == "dynamo.frontend.down")
    assert SV in p.message and "use-model.sh" not in (p.fix or "") and "console action `up`" in (p.fix or "")
    assert "docker-compose.super-vl.yml" in (p.fix or "")


async def test_worker_gpus_come_from_the_merged_config_and_are_cached(make, runner, compose) -> None:  # type: ignore[no-untyped-def]
    assert merged_worker_uuids(fx("merged_config_super_vl.json")) == [CARD_B, CARD_A]
    assert merged_worker_uuids("not json") == [] and merged_worker_uuids('{"services": []}') == []
    ov = sv_override(compose)
    wk = labelled(worker_doc(), [compose, ov], compose.parent)
    wk["State"].update(Running=False, Status="exited", ExitCode=0)          # stopped: `up` checks the cards
    wk["HostConfig"]["DeviceRequests"] = None                                # no container fallback to lean on
    bolt = gpu_doc(BOLT, uuids=[CARD_B], project="ai-lab-vllm-nemotron-bolt")   # on the OTHER card only
    tts = gpu_doc(CHATTERBOX, uuids=[CARD_A], project="ai-lab-chatterbox-tts")
    pol = CardPolicy(exclusive=(BOLT,), coexisting=(CHATTERBOX,))
    wire_docs(runner, [wk, bolt, tts], ps=f"{BOLT}\n{CHATTERBOX}\n", merged=fx("merged_config_super_vl.json"))
    ad = make(policy=pol)
    res = await ad.run_action("up", {})
    assert not res.ok and res.problems[0].code == "dynamo.up.exclusive_engine" and BOLT in res.output
    assert CHATTERBOX not in res.output                                       # coexisting: allowed beside the worker
    assert res.problems[0].fix == f"stop {BOLT} first (exclusive with this stack), then run the console action `up`"
    pf = await ad.run_action("preflight", {})
    assert f"{CARD_B}, {CARD_A}" in pf.output and "docker-compose.super-vl.yml" in pf.output
    probes = [a for a in runner.argvs() if a[-3:] == ["config", "--format", "json"]]
    assert len(probes) == 1 and files_of(probes[0]) == [str(compose.resolve()), str(ov.resolve())]   # cached
    # a variant never falls back to the BASE file's single card: with no merged config and no container, refuse
    runner.rules.clear()
    wire_docs(runner, [wk, bolt], ps=f"{BOLT}\n")
    ad2 = make(policy=pol)
    res2 = await ad2.run_action("up", {})
    assert not res2.ok and "GPU UUID" in res2.output and compose_calls(runner) == []


async def test_sv_frontend_logs_are_events_and_the_gateway_is_labelled(make, runner, compose) -> None:  # type: ignore[no-untyped-def]
    docs = sv_stack(compose)
    wire_docs(runner, docs, merged=fx("merged_config_super_vl.json"))
    lines = {FE: "2026-10-17T09:00:00.000000000Z 10.89.13.2 [17/Oct/2026:09:00:00 +0000] TCP 200 1234 567 0.010\n",
             SV: "2026-10-17T09:00:01.000000000Z 2026-10-17T09:00:01.000Z INFO dynamo_llm::http::service: request done\n",
             WK: "2026-10-17T09:00:02.000000000Z INFO worker ready\n"}
    runner.rules.insert(0, (lambda a: a[:2] == ["docker", "logs"], lambda a: ok(lines.get(a[-1], ""))))
    evs = await make().recent_events(50)
    by = {(e.data["component"], e.summary.split()[0]) for e in evs}
    assert ("gateway", "10.89.13.2") in by and ("frontend", "INFO") in by and ("worker", "INFO") in by
    assert any(a[:2] == ["docker", "logs"] and a[-1] == SV for a in runner.argvs())


def test_compose_files_overrides_property(tmp_path: Path) -> None:
    base = tmp_path / "docker-compose.yml"
    base.write_text("services: {}\n")
    ov = tmp_path / "docker-compose.super-vl.yml"
    assert ComposeFiles((base.resolve(),), base).overrides == ()
    assert ComposeFiles((base.resolve(), ov), base).overrides == (ov,)
    assert ComposeFiles((base.resolve(), ov), base).names() == "docker-compose.yml, docker-compose.super-vl.yml"


async def test_holds_card_is_information_not_a_warning(make, runner, mock) -> None:  # type: ignore[no-untyped-def]
    from .test_adapter import wire
    wire(runner, [frontend_doc(), worker_doc()])
    http_live(mock)
    st = await make().status()
    p = next(p for p in st.problems if p.code == "dynamo.holds_card")
    assert p.severity == "info" and st.health == "up"
    # an empty dark list (setting) means nothing to report
    st2 = await make(policy=CardPolicy(dark=())).status()
    assert "dynamo.holds_card" not in {q.code for q in st2.problems}


async def test_preflight_beside_a_variant_names_the_engine_not_the_base_start(make, runner, compose) -> None:  # type: ignore[no-untyped-def]
    ov = sv_override(compose)
    wk = labelled(worker_doc(), [compose, ov], compose.parent)
    wk["State"].update(Running=False, Status="exited", ExitCode=0)
    bolt = gpu_doc(BOLT, uuids=[CARD_B], project="ai-lab-vllm-nemotron-bolt")
    wire_docs(runner, [wk, bolt], ps=f"{BOLT}\n", merged=fx("merged_config_super_vl.json"))
    pf = await make(policy=CardPolicy(exclusive=(BOLT,))).run_action("preflight", {})
    [p] = [p for p in pf.problems if p.code == "dynamo.card.shared"]
    assert p.fix == f"stop {BOLT} first, then run the console action `up`"
    assert "qwen3.8-27b-dynamo" not in pf.output                     # the BASE stack's start command is never offered
    assert "exclusive with this stack: docker-compose.yml, docker-compose.super-vl.yml" in pf.output
