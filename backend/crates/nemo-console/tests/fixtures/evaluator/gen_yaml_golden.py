"""Golden generator for the Rust port of `yaml.safe_load` (src/adapters/evaluator/pyyaml.rs).

Runs PyYAML (the Python backend's own pinned copy) over hand-picked documents plus deterministic random mutations of
them, and records either the constructed object (a typed encoding, see `enc`) or the error (class, str(e),
problem, problem_mark.line). The Rust test tests/evaluator_yaml_golden.rs replays every case.

    REF=$(node parity/ref.mjs) && cd backend && "$REF/.venv/bin/python" -I ../backend/crates/nemo-console/tests/fixtures/evaluator/gen_yaml_golden.py

Offline, no network, writes only yaml_golden.json next to this file.
"""
from __future__ import annotations

import datetime
import json
import random
import sys
from pathlib import Path

import yaml

HERE = Path(__file__).resolve().parent
FIX = HERE.parent

HAND = [
    "", "a", "~", "null", "Null", "NULL", "-", "--- a", "---\n...\n", "%YAML 1.1\n--- x", "%YAML 2.0\n--- x",
    "%YAML 1.1\n%YAML 1.1\n--- x", "%TAG !e! tag:example.com,2000:\n--- !e!foo x", "%FOO bar baz\n--- x",
    "yes", "No", "on", "OFF", "y", "n", "True", "tRue",
    "1", "-1", "+1", "0", "-0", "0o17", "017", "08", "0b101", "0b_", "0x_", "0x1F", "-0x1f", "1_000", "1:30",
    "-1:30:00", "190:20:30", "123456789012345678901234567890123456789012", "0b" + "1" * 200,
    "1.5", "1.", ".5", "-.5", "1e5", "1.0e5", "1.0e+5", "1_0.5", ".inf", "-.Inf", "+.INF", ".nan", "-.nan", ".NaN",
    "1:30.5", "-1:30.5", "1.5e-3", "6.8523015e+5", "685.230_15e+03", "685_230.15",
    "2026-10-23", "2026-13-45", "2026-02-30", "0000-01-01", "2001-12-14t21:59:43.10-05:00", "2001-12-14 21:59:43.10",
    "2001-12-14 21:59:43.1234567Z", "2001-12-15 2:59:43.10", "2001-12-14 21:59:43.10 +5", "2026-01-01 10:00:00+99",
    "2026-01-01 24:00:00", "2026-01-01 10:60:00", "2026-01-01 10:00:60", "2026-01-01 10:00:00-24:00",
    "2026-1-1 1:00:00", "2026-1-1",
    "<<", "=", "!", "&a", "*a", "&a x", "[*a]", "a: &x 1\nb: *x", "a: &x [1, 2]\nb: *x\nc: *x",
    "&a [*a]", "&a {x: *a}", "&a {<<: *a}", "a: &a 1\nb: &a 2",
    "x: !!int 12", "x: !!int abc", "x: !!int ''", "x: !!int '-'", "x: !!int ' 12 '", "x: !!int 0o17", "x: !!int 1:-5",
    "x: !!float abc", "x: !!float ''", "x: !!float 1e5", "x: !!float nan", "x: !!float ' 2.5 '",
    "x: !!bool maybe", "x: !!bool YES", "x: !!null abc", "x: !!str 12", "x: !!timestamp foo",
    "x: !!timestamp {=: 2020-01-01}", "x: !!str {=: abc}", "x: !!str [1]", "x: !!seq abc", "x: !!map abc",
    "x: !!binary abc", "x: !!binary 'a='", "x: !!binary aGVsbG8=", "x: !!binary 'aGVs bG8='", "x: !!binary 'é'",
    "x: !!binary 'éé'", "x: !!set {a, b, c}", "x: !!set [a]", "x: !!omap [a: 1, b: 2]", "x: !!omap [a: 1, b]",
    "x: !!omap [{a: 1, b: 2}]", "x: !!omap a", "x: !!pairs [a: 1, a: 2]", "x: !foo bar", "x: !<tag:yaml.org,2002:str> 1",
    "x: !!python/object:os.system x", "x: ! 123", "x: ! '123'", "x: ! \"123\n\"", "x: ! '!'",
    "x: !e!foo bar", "x: !%e9 bar", "x: !!str%ff a", "x: !<a", "x: !a!b!c d",
    "{a: 1, a: 2}", "{1: a, 1.0: b, true: c}", "{.nan: a, .nan: b}", "{[1]: a}", "{{a: 1}: b}", "? [1]\n: a",
    "{2026-01-01: a, 2026-01-01 00:00:00: b}", "{2026-01-01 10:00:00Z: a, 2026-01-01 11:00:00+01:00: b}",
    "a: b: c", "- a\nb: c", "a:\n  - b\n  c: d", "a:\tb", "\t- a", "a: [1, 2", "a: {b: 1", "a: [1, 2]]", "]", "}",
    "[a, b]]", "a: 'unterminated", "a: \"unterminated", "key: \"\\q\"", "key: \"\\x4\"", "key: \"\\u00e9\"",
    "key: \"\\U0001F600\"", "key: \"\\UFFFFFFFF\"", "key: \"a\\\n  b\"", "'a''b'", "\"a\\\"b\"", "'multi\n  line\n\n  text'",
    "\"multi\n  line\"", "a: |\n  x\n  y\n", "a: |-\n  x\n\n", "a: |+\n  x\n\n", "a: >\n  x\n  y\n\n  z\n", "a: |2\n   x\n",
    "a: |0\n  x", "a: |x\n  x", "a: | # c\n  x", "a: | junk\n  x", "a: >-\n  folded\n   more\n  text\n",
    "plain\n  continued\n\n  para", "a: x # comment\nb: y", "# only a comment\n", "a: x\n# c\nb: y",
    "\ufeffa: 1", "a: \ufeff1", "\x01", "a: \x7f", "a: \x85b", "a\u2028b: c", "a: b\r\nc: d\r\n", "a: b\rc: d",
    "- - a\n  - b\n- c", "- a: 1\n  b: 2\n- c: 3", "a:\n- 1\n- 2", "? a\n? b\n: c", "?\n: x", ": x", "- ? a\n  : b",
    "[a: 1, b]", "[? a]", "{a, b: c}", "{? a}", "[a, ]", "{a: , b: }", "[,]", "{,}", "a: [b, c]: d",
    "a: 'x'y", "a: \"x\" y", "a: &anchor", "a: !!str", "a: *undefined", "a: & b", "a: * b", "a: &a* b",
    "--- a\n--- b", "a\n---\nb", "--- |\n  x\n--- y", "...\n", "--- >\n  a\n...\n--- b",
    "x: 'a\n---\nb'", "x: \"a\n...\n  b\"", "a: b\n---", "%YAML 1.1 extra\n--- x", "%YAML 1.x\n--- x", "%YAML\n--- x",
    "%\n--- x", "%TAG ! \n--- x", "%TAG !a tag:x\n--- x",
    "key: " + "v" * 2000, "k" * 1100 + ": v", "x: [" + "a, " * 300 + "a]",
    "a:\n  b:\n    c:\n      d: 1\n   e: 2", "a:\n  b: 1\n c: 2", "  a: 1\nb: 2", "a: 1\n  b: 2",
    "- a\n -b", "-a", "--", "---a", "a: -", "a: - b", "a: ? b", "a: @b", "a: `b", "a: %b", "@a", "`a", "|", ">",
    "a: |\n", "a: >", "a: 'b'c'", "a: [b, 'c]", "x: {a: [1, {b: 2}], c: d}", "&a a: 1", "!!str &a a: 1",
    "a: !!map\n  b: 1", "a: !!seq\n- 1", "!!set\n? a\n? b", "!!omap\n- a: 1\n- b: 2",
    "a: b\nc", "a: b\nc d", "a\nb", "[\na,\nb\n]", "{\na: 1\n}", "[a\n,b]", "a: [b\n c]",
    "x: 1:2:3:4:5:6:7:8:9:10:11:12:13:14:15:16:17:18:19:20:21:22:23:24:25",
    "x: 1:2:3:4:5:6:7:8:9:10:11:12:13:14:15:16:17:18:19:20:21:22:23:24:25.5",
]

ALPHABET = list("-:?[]{},#&*!|>'\"%@` \n\t.~=<1a0") + ["\r", "\n  ", ": ", "- ", "  "]


def enc(v, seen=None):
    """A typed, order-preserving encoding (sets sorted by element encoding, as Python's set order is hash-based)."""
    seen = seen or set()
    if isinstance(v, (list, dict, set)) and id(v) in seen:
        return ["R"]
    if v is None:
        return ["N"]
    if isinstance(v, bool):
        return ["B", v]
    if isinstance(v, int):
        return ["I", str(v)]
    if isinstance(v, float):
        return ["F", repr(v)]
    if isinstance(v, str):
        return ["S", v]
    if isinstance(v, bytes):
        return ["Y", v.hex()]
    if isinstance(v, datetime.datetime):
        return ["T", repr(v), str(v)]
    if isinstance(v, datetime.date):
        return ["D", repr(v), str(v)]
    if isinstance(v, tuple):
        return ["U", [enc(x, seen) for x in v]]
    inner = seen | {id(v)}
    if isinstance(v, list):
        return ["L", [enc(x, inner) for x in v]]
    if isinstance(v, dict):
        return ["M", [[enc(k, inner), enc(x, inner)] for k, x in v.items()]]
    if isinstance(v, set):
        return ["E", sorted((enc(x, inner) for x in v), key=lambda e: json.dumps(e))]
    return ["?", type(v).__name__]


def run(text: str) -> dict:
    try:
        v = yaml.safe_load(text)
    except Exception as e:  # noqa: BLE001 - every exception class is part of the golden
        mark = getattr(e, "problem_mark", None)
        return {"err": {"class": type(e).__name__, "text": str(e), "problem": getattr(e, "problem", None),
                        "line": mark.line if mark is not None else None, "yaml": isinstance(e, yaml.YAMLError)}}
    try:
        return {"ok": enc(v)}
    except RecursionError:
        return {"skip": "recursion while encoding"}


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


def main() -> int:
    seeds = [
        (FIX / "evaluator" / "suites_good.yaml").read_text(),
        (FIX / "datadesigner" / "model_configs.yaml").read_text(),
        (FIX / "datadesigner" / "model_providers.yaml").read_text(),
        "providers:\n- name: lab-router\n  endpoint: http://127.0.0.1:4000/v1\n  provider_type: openai\n",
        "version: 1\nsuites:\n  a:\n    model: m\n    benchmarks:\n      - {bench: gsm8k, max_problems: 5}\n",
        "a: [1, {b: 'c', d: \"e\"}, &x f, *x]\n? complex\n: value\n",
        "- |\n  block\n- >-\n  folded\n- 'q'\n- \"d\\tq\"\n",
    ]
    rng = random.Random(20261027)
    inputs = list(HAND) + seeds
    for i in range(2500):
        base = seeds[i % len(seeds)] if i % 3 else rng.choice(HAND + seeds)
        inputs.append(mutate(rng, base))
    seen: set[str] = set()
    cases = []
    for text in inputs:
        if text in seen:
            continue
        seen.add(text)
        out = run(text)
        if "skip" in out:
            continue
        cases.append({"input": text, **out})
    (HERE / "yaml_golden.json").write_text(json.dumps({"pyyaml": yaml.__version__, "cases": cases}, indent=0,
                                                      ensure_ascii=True) + "\n")
    print(f"{len(cases)} cases", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
