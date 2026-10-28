"""Golden generator: the Python SkillEvaluatorAdapter's report parsing and report texts over hostile inputs.

* "summaries": summarize_tier1 / failed_checks / summarize_tier3 / last_json_object over the captured reports with
  fields randomly replaced by odd JSON values (deterministic seed).
* "reports": stored runs (run.json metas with odd values, optional markdown report and Tier 3 result tree) read back
  through run_action("report") — the exact text the UI shows — or the exception it raises.

    REF=$(node parity/ref.mjs) && cd backend && "$REF/.venv/bin/python" -I ../backend/crates/nemo-console/tests/fixtures/skillevaluator/gen_se_golden.py

Offline; writes only se_golden.json next to this file.
"""
from __future__ import annotations

import asyncio
import copy
import json
import random
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[5] / "backend" / "src"))

from nemo_console.adapters import skillevaluator as se  # noqa: E402
from nemo_console.adapters.base import AdapterContext  # noqa: E402

T1 = json.loads((HERE / "tier1_incomplete.json").read_text())
T3 = json.loads((HERE / "tier3_result.json").read_text())
POOL = [None, True, False, 0, 7, -3, 1.5, 0.1597, 1e300, "", "x", "UPPER", "a b", "succeeded", "failed", "passed",
        "critical", "high", "medium", "low", "info", "Bearer sk-abcdefghijklmnop", [], {}, [1, "x"], {"a": 1},
        ["skillspector", 5], {"message": "m", "title": "t", "severity": "high"}, "m" * 500]


def paths(d: object, prefix: tuple = ()) -> list[tuple]:
    out = [prefix]
    if isinstance(d, dict):
        for k, v in d.items():
            out += paths(v, prefix + (k,))
    elif isinstance(d, list):
        for i, v in enumerate(d[:6]):
            out += paths(v, prefix + (i,))
    return out


def put(d: object, path: tuple, v: object) -> object:
    if not path:
        return v
    cur = d
    for p in path[:-1]:
        cur = cur[p]
    cur[path[-1]] = v
    return d


def mutated(rng: random.Random, base: dict, n: int) -> list:
    ps = paths(base)
    out = []
    for _ in range(n):
        d = copy.deepcopy(base)
        for _ in range(rng.randint(1, 4)):
            p = rng.choice(ps)
            try:
                d = put(d, p, copy.deepcopy(rng.choice(POOL)))
            except (KeyError, IndexError, TypeError):
                pass
        out.append(d)
    return out


def exc(e: BaseException) -> dict:
    return {"raised": {"class": type(e).__name__, "message": str(e)}}


def call(f, *a):  # noqa: ANN001,ANN202
    try:
        return f(*a)
    except Exception as e:  # noqa: BLE001
        return exc(e)


def summaries(rng: random.Random) -> list:
    out = []
    for d in [T1, *mutated(rng, T1, 60)]:
        out.append({"kind": "tier1", "input": d, "summary": call(se.summarize_tier1, d),
                    "failed": call(se.failed_checks, d)})
    for d in [T3, *mutated(rng, T3, 80)]:
        if isinstance(d, dict):
            out.append({"kind": "tier3", "input": d, "summary": call(se.summarize_tier3, d)})
    for text in ["", "{}", "x\n{\"a\": 1}", "{\"a\": [1,\n  2]}\n", "a\n{\n  \"job\": 1\n}\ntrailing", "{bad",
                 "[1]", "{\"a\": 1}\n{\"b\": 2}"]:
        out.append({"kind": "last_json", "input": text, "summary": call(se.last_json_object, text)})
    return out


async def reports(rng: random.Random) -> list:
    base1 = {"run_id": "R", "skill": "s", "tiers": "tier1", "ts": 1792841333.0, "model_calls": True,
             "launcher": "skillevaluator-local", "tier1": se.summarize_tier1(T1), "failed_checks": se.failed_checks(T1),
             "verdict": "incomplete", "tier2_status": "passed", "duration_ms": 1200}
    t3 = se.summarize_tier3(T3)
    base3 = {"run_id": "R", "skill": "s", "tiers": "tier3", "ts": 1792841333.0, "verdict": "succeeded",
             "model_calls": True, "launcher": "skillevaluator-local",
             "live": {"agent": "opencode", "agent_model": "a", "judge_model": "j", "attempts": 2, "dataset": "auto",
                      "skip_baseline": False},
             "duration_ms": 5, "container_exit": 0, "job": "J", "conclusions": t3.pop("conclusions"),
             "recommendations": t3.pop("recommendations"), "tier3": t3,
             "result": "calculator/20261017_091635_7_46d5ccd742f5/result.json"}
    metas = [base1, base3, *mutated(rng, base1, 50), *mutated(rng, base3, 70)]
    for m in metas[2:30]:
        if isinstance(m, dict):
            m["verdict"] = rng.choice(["error", "running", "interrupted", "failed", "succeeded"])
    out = []
    with tempfile.TemporaryDirectory() as td:
        for i, meta in enumerate(metas):
            if not isinstance(meta, dict):
                continue
            tmp = Path(td) / f"r{i}"
            run_id = f"20261017T091635-{i:03d}-s"
            run_dir = tmp / "state" / "skillevaluator" / "runs" / run_id
            run_dir.mkdir(parents=True)
            meta = {**meta, "run_id": run_id}
            (run_dir / "run.json").write_text(json.dumps(meta))
            md = i % 3 == 0
            if md:
                (run_dir / "skillevaluator-tier1.md").write_text("# Report\n\n| a | b |\n")
            tree = i % 2 == 0
            if tree:
                rd = run_dir / "calculator" / "20261017_091635_7_46d5ccd742f5"
                rd.mkdir(parents=True)
                (rd / "result.json").write_text(json.dumps(T3))
                (rd / "report.html").write_text("<html></html>\n")
            ctx = AdapterContext(runner=None, http=None, urls={}, paths={}, state_dir=tmp / "state")  # type: ignore[arg-type]
            a = se.SkillEvaluatorAdapter(ctx)
            try:
                res = await a.run_action("report", {"run_id": run_id})
                text = res.output.replace(str(run_dir), "<RUN_DIR>")
                rep = {"ok": res.ok, "output": text}
            except Exception as e:  # noqa: BLE001
                rep = exc(e)
            out.append({"meta": meta, "md": md, "tree": tree, "report": rep})
    return out


async def main() -> int:
    rng = random.Random(777)
    data = {"summaries": summaries(rng), "reports": await reports(rng)}
    (HERE / "se_golden.json").write_text(json.dumps(data, indent=0, ensure_ascii=True, allow_nan=False) + "\n")
    print(f"{len(data['summaries'])} summaries, {len(data['reports'])} reports", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
