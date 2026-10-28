"""Golden generator: the Python EvaluatorAdapter's suites.yaml handling over many candidate files.

For each candidate text it records (a) `validate_config("suites.yaml", text)` with the wrapper absent (schema only),
(b) the same with a cached-datasets view {gsm8k: False, mmlu: True} (prefetch warnings), and (c) `suites()` reading
the text from the suites file (read_text: universal newlines). Exceptions are recorded as class + message. The Rust
test tests/evaluator_ev_golden.rs replays every case.

    REF=$(node parity/ref.mjs) && cd backend && "$REF/.venv/bin/python" -I ../backend/crates/nemo-console/tests/fixtures/evaluator/gen_ev_golden.py

Offline; writes only ev_golden.json next to this file.
"""
from __future__ import annotations

import asyncio
import json
import random
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[5] / "backend" / "src"))

from nemo_console.adapters.base import AdapterContext, CommandResult  # noqa: E402
from nemo_console.adapters.evaluator import EvaluatorAdapter  # noqa: E402

GOOD = (HERE / "suites_good.yaml").read_text()
HAND = [
    GOOD, "", "~", "[]", "- a", "version: 1", "version: 1\nsuites: {}\n", "version: 1\nsuites: []\n",
    "version: 1.0\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
    "version: true\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
    "version: '1'\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
    "version: 1\nsuites:\n  1: {model: m, benchmarks: [{bench: gsm8k, max_problems: 5}]}\n",
    "version: 1\nsuites:\n  2026-01-01: {model: m, benchmarks: [{bench: gsm8k, max_problems: 5}]}\n",
    "version: 1\nsuites:\n  ~: {model: m, benchmarks: [{bench: gsm8k, max_problems: 5}]}\n",
    "version: 1\nsuites:\n  yes: {model: m, benchmarks: [{bench: gsm8k, max_problems: 5}]}\n",
    "version: 1\nsuites:\n  1.5: {model: m, benchmarks: [{bench: gsm8k, max_problems: 5}]}\n",
    "version: 1\nsuites:\n  a: [1]\n",
    "version: 1\nsuites:\n  a:\n    model: 5\n    description: [x]\n    benchmarks: [1, 'x', {bench: 5}]\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    description: " + "d" * 201 + "\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n" + "".join(f"      - {{bench: b{i}, max_problems: 1}}\n" for i in range(10)),
    "version: 1\nsuites:\n" + "".join(f"  s{i}: {{model: m, benchmarks: [{{bench: gsm8k, max_problems: 1}}]}}\n" for i in range(22)),
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5, max_tokens: 8}\n      - {bench: mmlu, max_problems: 5, temperature: 3}\n      - {bench: drop, max_problems: 5, temperature: .nan}\n      - {bench: math500, max_problems: 5, temperature: true, repeats: 0}\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 123456789012345678901234567890}\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5, temperature: 1e400}\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5, temperature: 0x1}\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5, temperature: 1" + "0" * 400 + "}\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 4000, repeats: 3}\n      - {bench: mmlu, max_problems: 4000, repeats: 3}\n",
    "version: 1\nextra: 1\n2: 3\nsuites:\n  a: {model: m, benchmarks: [{bench: gpqa, max_problems: 5}, {bench: mgsm, max_problems: 5}, {bench: xstest, max_problems: 1}]}\n",
    "version: 1\nsuites:\n  a: &a {model: m, benchmarks: [{bench: gsm8k, max_problems: 5}]}\n  b: *a\n",
    "version: 1\nsuites:\n  a:\n    <<: {model: m}\n    benchmarks: [{bench: gsm8k, max_problems: 5}]\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks: [{bench: !!str gsm8k, max_problems: !!int '5'}]\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks: [{bench: gsm8k, max_problems: !!int x}]\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks: [{bench: gsm8k, max_problems: !!bool maybe}]\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks: [{bench: gsm8k, max_problems: 2026-13-01}]\n",
    "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks: [{bench: gsm8k, max_problems: !foo 5}]\n",
    "version: 1\nsuites:\n  a:\r\n    model: m\r\n    benchmarks:\r\n      - {bench: gsm8k, max_problems: 5}\r\n",
    "version: 1\rsuites:\r  a:\r    model: m\r    benchmarks:\r      - {bench: humaneval, max_problems: 5}\r",
    "version: 1\nsuites:\n  a\u2028b: {model: m, benchmarks: [{bench: gsm8k, max_problems: 5}]}\n",
    "version: 1\nsuites: [unclosed\n", "suites: [\n", "\x01", "version: 1\nsuites:\n  a: {model: m\n",
    "version: 1\nsuites:\n  'x-y': {model: 'a b', benchmarks: [{bench: gsm8k, max_problems: 5}]}\n",
    "version: 1\nsuites:\n  ab:\n    model: m\n    benchmarks:\n      - bench: humaneval\n        max_problems: 5\n      - bench: gsm8k\n",
]
ALPHABET = list("-:?[]{},#&*!|>'\" \n.~=1a0x") + ["\n  ", ": ", "- ", "  ", "bench", "max_problems: ", "\r"]


class Runner:
    async def run(self, argv, *, timeout=15.0, max_bytes=256_000, stdin=None, cwd=None):  # noqa: ANN001,ANN201
        return CommandResult(returncode=127, stdout="", stderr="not installed")


def mutate(rng: random.Random, s: str) -> str:
    chars = list(s)
    for _ in range(rng.randint(1, 3)):
        op = rng.random()
        pos = rng.randint(0, len(chars))
        if op < 0.45 or not chars:
            chars[pos:pos] = list(rng.choice(ALPHABET))
        elif op < 0.8:
            del chars[min(pos, len(chars) - 1)]
        else:
            chars[min(pos, len(chars) - 1)] = rng.choice(ALPHABET)
    return "".join(chars)


def exc(e: BaseException) -> dict:
    return {"class": type(e).__name__, "message": str(e)}


async def one(tmp: Path, text: str) -> dict:
    edir = tmp / "repo" / "clients" / "nemo-evaluator"
    edir.mkdir(parents=True, exist_ok=True)
    spath = edir / "suites.yaml"
    spath.write_bytes(text.encode("utf-8", "surrogatepass"))
    ctx = AdapterContext(runner=Runner(), http=None, urls={"litellm": "http://127.0.0.1:9"},  # type: ignore[arg-type]
                         paths={"evaluator_dir": edir, "evaluator_suites": spath}, state_dir=tmp / "state")
    a = EvaluatorAdapter(ctx)
    out: dict = {"input": text}
    try:
        out["validate"] = (await a.validate_config("suites.yaml", text)).model_dump(mode="json")
    except Exception as e:  # noqa: BLE001
        out["validate"] = {"raised": exc(e)}
    a._cached_cache = (a._cached_cache[0], {"gsm8k": False, "mmlu": True})
    try:
        out["validate_cached"] = (await a.validate_config("suites.yaml", text)).model_dump(mode="json")
    except Exception as e:  # noqa: BLE001
        out["validate_cached"] = {"raised": exc(e)}
    try:
        suites, errs = a.suites()
        out["suites"] = {"suites": {k: {"model": str(v.get("model")), "benches": a._suite_benches(v)}
                                    for k, v in suites.items()}, "errors": errs}
    except Exception as e:  # noqa: BLE001
        out["suites"] = {"raised": exc(e)}
    return out


async def main() -> int:
    rng = random.Random(4242)
    inputs = list(HAND)
    seeds = [GOOD, HAND[-1], HAND[19]]
    for i in range(700):
        inputs.append(mutate(rng, seeds[i % len(seeds)]))
    seen: set[str] = set()
    cases = []
    with tempfile.TemporaryDirectory() as td:
        for i, text in enumerate(inputs):
            if text in seen:
                continue
            seen.add(text)
            cases.append(await one(Path(td) / str(i), text))
    (HERE / "ev_golden.json").write_text(json.dumps({"cases": cases}, indent=0, ensure_ascii=True) + "\n")
    print(f"{len(cases)} cases", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
