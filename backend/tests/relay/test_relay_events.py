from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path

from relay_helpers import fx

from nemo_console.adapters.relay import parse_atof_line

CANARY = "CANARY-SECRET-PROMPT-TEXT with spaces and sk-abcdefghijklmnopqrstuvwxyz0123"


def _sample_events():  # type: ignore[no-untyped-def]
    return [e for e in (parse_atof_line(ln.encode()) for ln in fx("atof_sample.jsonl").splitlines()) if e]


def test_golden_sample_parses_metadata_only() -> None:
    evs = _sample_events()
    assert len(evs) == 39
    assert {e.kind for e in evs} == {"relay.atof"}
    assert {e.system for e in evs} == {"relay"}
    end = next(e for e in evs if e.data["name"] == "anthropic.messages" and e.data.get("phase") == "end")
    assert end.data["model"] == "nemotron-lightning"
    assert end.data["input_tokens"] == 56 and end.data["output_tokens"] == 400
    assert end.data["stop_reason"] == "max_tokens" and end.data["gateway_path"] == "/v1/messages"
    dec = next(e for e in evs if e.data["name"] == "switchyard.routing.decision")
    assert dec.data["algorithm"] == "stage_router" and dec.data["served_model"] == "nemotron-lightning"
    assert dec.data["fallback_used"] is False
    tok = next(e for e in evs if e.data["name"] == "switchyard.routing.llm_tokens")
    assert tok.data["input_tokens"] == 56 and tok.data["model"] == "nemotron-lightning"
    err = next(e for e in evs if e.data["name"] == "switchyard.routing.error")
    assert err.data["upstream_status"] == 400 and err.data["error_category"] == "upstream_http"
    assert err.data["severity"] == "error"
    assert all(e.data["size_bytes"] > 0 for e in evs)


def test_no_text_can_reach_an_event() -> None:
    line = {
        "atof_version": "0.1", "kind": "scope", "name": "claude-code-turn", "scope_category": "start", "category": "custom",
        "timestamp": "2026-09-24T01:02:03.123456789+00:00", "uuid": "u-1",
        "category_profile": {"annotated_request": {"messages": [{"content": CANARY}]}, "model_name": CANARY},
        "metadata": {"session_id": CANARY, "gateway_path": CANARY, "agent_kind": CANARY},
        "data": {"prompt": CANARY, "content": {"messages": [{"role": "user", "content": CANARY}], "model": CANARY},
                 "model": CANARY, "outcome": CANARY, "headers": {"x-nemo-relay-session-id": CANARY, "authorization": CANARY},
                 "choices": [{"finish_reason": CANARY, "message": {"content": CANARY}}],
                 "usage": {"input_tokens": 5, "output_tokens": 7}},
    }
    ev = parse_atof_line(json.dumps(line).encode())
    assert ev is not None
    blob = ev.model_dump_json()
    assert "CANARY" not in blob and "sk-" not in blob
    assert ev.data["input_tokens"] == 5 and ev.data["output_tokens"] == 7
    assert ev.ts > 0 and ev.summary.startswith("claude-code-turn")


def test_tool_name_name_with_text_is_dropped_and_bad_lines_are_none() -> None:
    assert parse_atof_line(b"not json") is None
    assert parse_atof_line(b"") is None
    assert parse_atof_line(b'{"name":"ok","timestamp":"garbage"}') is None
    assert parse_atof_line(json.dumps({"name": "two words secret", "timestamp": "2026-09-24T01:02:03Z"}).encode()) is None
    assert parse_atof_line(b"x" * (2 * 1024 * 1024)) is None


async def test_recent_events_newest_file_skips_chunks_and_bounds(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    evs = await a.recent_events(limit=5)
    assert len(evs) == 5
    assert [e.ts for e in evs] == sorted(e.ts for e in evs)
    assert all(e.data["name"] != "llm.chunk" for e in evs)
    everything = await a.recent_events(limit=1000)
    assert len(everything) == 38          # 39 sample lines minus the one llm.chunk
    assert (await a.recent_events(limit=0)) != []   # clamped to 1


async def test_recent_events_ignores_symlinks_and_garbage_and_partial_first_line(make_adapter, tmp_path: Path) -> None:  # type: ignore[no-untyped-def]
    good = fx("atof_sample.jsonl").splitlines()[2]
    a, _ = make_adapter(atof_lines="garbage-not-json\n" + good + "\n{truncated")
    atof = a.ctx.paths["relay_atof_dir"]
    secret = tmp_path / "outside.jsonl"
    secret.write_text(good + "\n")
    os.symlink(secret, atof / "relay-events.jsonl.0")      # a symlink in the dir must never be followed
    evs = await a.recent_events(limit=10)
    assert len(evs) == 1


async def test_recent_events_missing_dir_is_empty(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    a.ctx.paths["relay_atof_dir"] = a.ctx.paths["relay_atof_dir"] / "nope"
    assert await a.recent_events() == []


async def test_follow_events_tails_appends_and_terminates_on_cancel(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    active = a.ctx.paths["relay_atof_dir"] / "relay-events.jsonl"
    lines = fx("atof_sample.jsonl").splitlines()
    got: list[str] = []

    async def consume() -> None:
        async for ev in a.follow_events():
            got.append(ev.data["name"])

    task = asyncio.create_task(consume())
    await asyncio.sleep(0.05)
    with active.open("a") as fh:
        fh.write(lines[2] + "\n" + lines[3][:40])       # one full line + a partial one
    await asyncio.sleep(0.1)
    assert len(got) == 1
    with active.open("a") as fh:
        fh.write(lines[3][40:] + "\n")
    await asyncio.sleep(0.1)
    assert len(got) == 2
    task.cancel()
    await asyncio.wait([task], timeout=1)
    assert task.done()


async def test_follow_events_since_ts_replays_newer_only(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    all_evs = await a.recent_events(limit=1000)
    cutoff = all_evs[-3].ts
    seen = []
    agen = a.follow_events(since_ts=cutoff)
    async for ev in agen:
        seen.append(ev)
        if len(seen) == 2:
            break
    await agen.aclose()
    assert all(e.ts > cutoff for e in seen)


# ── gzipped rotations (live 2026-10-17: relay-events.jsonl, .1 and .2.gz) ──────────────────────────
def _gz(path: Path, text: str) -> None:
    import gzip
    with gzip.open(path, "wb") as fh:
        fh.write(text.encode())


async def test_recent_events_read_a_gzipped_rotation_when_newer_files_are_short(make_adapter) -> None:  # type: ignore[no-untyped-def]
    lines = fx("atof_sample.jsonl").splitlines()
    a, _ = make_adapter(atof_lines="\n".join(lines[-5:]) + "\n")            # .1 holds only the newest five lines
    atof = a.ctx.paths["relay_atof_dir"]
    _gz(atof / "relay-events.jsonl.2.gz", "\n".join(lines[:-5]) + "\n")       # the older ones were rotated away
    names = [p.name for p in a._atof_files_newest_first()]
    assert names == ["relay-events.jsonl", "relay-events.jsonl.1", "relay-events.jsonl.2.gz"]
    evs = await a.recent_events(limit=1000)
    assert len(evs) == 38                                                   # the same 38 events as the plain sample
    assert [e.ts for e in evs] == sorted(e.ts for e in evs)


async def test_gzip_rotation_caps_symlinks_and_garbage(make_adapter, tmp_path: Path, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    from nemo_console.adapters import relay as relay_mod
    lines = fx("atof_sample.jsonl").splitlines()
    a, _ = make_adapter(atof_lines="")
    atof = a.ctx.paths["relay_atof_dir"]
    _gz(atof / "relay-events.jsonl.2.gz", "\n".join(lines) + "\n")
    (atof / "relay-events.jsonl.3.gz").write_bytes(b"not gzip at all")      # unreadable: skipped, no crash
    outside = tmp_path / "outside.jsonl.gz"
    _gz(outside, lines[2] + "\n")
    os.symlink(outside, atof / "relay-events.jsonl.4.gz")                   # never followed
    assert len(await a.recent_events(limit=1000)) == 38
    monkeypatch.setattr(relay_mod, "MAX_GZ_BYTES", 1024)                    # a "bomb": decompresses past the cap
    assert await a.recent_events(limit=1000) == []
    monkeypatch.setattr(relay_mod, "MAX_GZ_BYTES", 256 * 1024 * 1024)
    monkeypatch.setattr(relay_mod, "MAX_GZ_FILE_BYTES", 10)                 # compressed file too large: not read
    assert await a.recent_events(limit=1000) == []


async def test_follow_never_tails_a_gzipped_rotation(make_adapter) -> None:  # type: ignore[no-untyped-def]
    a, _ = make_adapter()
    atof = a.ctx.paths["relay_atof_dir"]
    (atof / "relay-events.jsonl").unlink()
    _gz(atof / "relay-events.jsonl.gz", fx("atof_sample.jsonl"))            # rotation 0, but compressed
    state: dict[str, object] = {}
    assert a._follow_step(state) == [] and state.get("ino") is not None     # the plain .1 is the active file
