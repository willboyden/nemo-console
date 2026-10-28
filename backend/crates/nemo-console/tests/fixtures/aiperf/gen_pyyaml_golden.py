"""Generate pyyaml_golden.json: what PyYAML 6.0.3 (pure-Python SafeLoader) answers for a corpus of documents, so the
Rust port in src/adapters/aiperf/pyyaml/ is pinned to it (PORTING.md risk R-2).

    REF=$(node parity/ref.mjs) && "$REF/.venv/bin/python" -I backend/crates/nemo-console/tests/fixtures/aiperf/gen_pyyaml_golden.py

Each case records either the loaded value (as a canonical tagged form) or the error: its class, str(e), the
`problem` the adapters print and the problem mark's line, plus `yaml.compose` line numbers of mapping keys and the
first line of str(e) (guardrails). Offline; reads only the fixtures next to this file.
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

CORPUS: list[str] = [
    "", "a: 1\n", "- a\n- b\n", "a: yes\nb: No\nc: on\nd: OFF\ne: y\nf: n\n", "x: ~\ny: null\nz: Null\nw:\n",
    "i: 0x1F\nj: 017\nk: 0b101\nl: -12_000\nm: 1:30\nn: +5\no: 0\np: 99999999999999999999999999999999999999999\n",
    "f: 1.5\ng: -.inf\nh: .NaN\ni: 1e5\nj: 1.0e+5\nk: 6.8523015e+5\nl: 190:20:30.15\nm: .5\nn: 1_000.5\n",
    "d: 2026-10-24\nt: 2026-10-24T11:13:31Z\nu: 2026-10-24 11:13:31.052 +02:00\nv: 2001-12-14t21:59:43.10-05:00\n",
    "s: '7'\nt: \"a\\tb\\u00e9\\x41\"\nu: 'it''s'\n", "multi: |\n  line 1\n  line 2\nfold: >-\n  a\n  b\n\n  c\n",
    "anchors:\n  base: &b {x: 1, y: 2}\n  copy: *b\n  merged:\n    <<: *b\n    y: 3\n",
    "list: [1, 2, [3, 4], {a: b}]\nmap: {k: v, 'q': \"w\"}\n", "? complex\n: value\n", "1: a\n1.0: b\ntrue: c\n",
    "set: !!set {a, b}\nomap: !!omap [{a: 1}, {b: 2}]\npairs: !!pairs [{a: 1}, {a: 2}]\nbin: !!binary aGVsbG8=\n",
    "str: !!str 123\nint: !!int '42'\nfloat: !!float '3'\n", "%YAML 1.1\n---\na: 1\n...\n", "--- >\n  folded\n  text\n",
    "a: 'unicode \u00e9 \u2603'\n", "a:  b  \n# comment\nc: d # trailing\n", "key with spaces: value with: colon\n",
    "- - nested\n  - seq\n- - x\n", "a:\n- 1\n- 2\nb:\n  - 3\n",
    # errors
    "profiles: [unclosed\n", "presets: [\n", "a: [1, 2\n", "a: b: c\n", "a:\n  - b\n c: d\n", "key: 'unterminated\n",
    "key: \"bad \\q escape\"\n", "- a\nb: c\n", "a: *undefined\n", "a: &x 1\nb: &x 2\n", "--- a\n--- b\n",
    "a: !!int abc\n", "a: !!float abc\n", "a: !!bool maybe\n", "a: !!timestamp nope\n", "a: 2026-13-01\n",
    "a: 2026-02-30 10:00:00\n", "a: !unknown x\n", "{[a]: 1}\n", "? [a]\n: 1\n", "a: !!binary '%%%'\n", "a: \x01\n",
    "\tkey: value\n", "a: |0\n  x\n", "a: @x\n", "a: `x\n", "a: {b: 1\n", "[a, b\n", "a: b\n  c: d\n",
    "models: [\n  - type: main", "a: [\n", "x: " + "a" * 100 + " {\n", "- &a [*a]\n" if False else "- &a [1]\n- *a\n",
    "a: !!int 0b_\n", "a: !!int ''\n", "a: !!float ''\n", "a: 2026-10-24 11:00:00 +25:00\n", "%TAG ! tag:x,2000:\n--- !foo a\n",
    "%YAML 2.0\n--- a\n", "%YAML 1.1\n%YAML 1.1\n--- a\n", "a: !<tag:yaml.org,2002:str> 5\n", "!!map\n",
    "]\n", "a: ]\n", "a: '\n  multi\n\n  line'\n", "a: \"x\n  y\\\n  z\"\n", "- -1\n- +.5\n- 0o17\n- 08\n- 1__0\n",
]
for name in ("profiles.yaml",):
    CORPUS.append((FIX / "aiperf" / name).read_text())
for rel in ("guardrails/config-tree/lab-input-output/config.yml", "guardrails/config-tree/lab-input-output/prompts.yml",
            "openshell/runner/policy.yaml", "openshell/runner/profile-lab-router.yaml", "openshell/runner/docker-compose.yml"):
    CORPUS.append((FIX / rel).read_text())


def canon(v: object) -> object:
    if v is None:
        return ["null"]
    if isinstance(v, bool):
        return ["bool", v]
    if isinstance(v, int):
        return ["int", str(v)]
    if isinstance(v, float):
        return ["float", repr(v)]
    if isinstance(v, str):
        return ["str", v]
    if isinstance(v, bytes):
        return ["bytes", v.hex()]
    if isinstance(v, datetime.datetime):
        return ["datetime", str(v), repr(v)]
    if isinstance(v, datetime.date):
        return ["date", str(v), repr(v)]
    if isinstance(v, tuple):
        return ["tuple", [canon(x) for x in v]]
    if isinstance(v, list):
        return ["list", [canon(x) for x in v]]
    if isinstance(v, set):
        return ["set", sorted(json.dumps(canon(x)) for x in v)]
    if isinstance(v, dict):
        return ["dict", [[canon(k), canon(x)] for k, x in v.items()]]
    raise TypeError(type(v).__name__)


def err(e: BaseException) -> dict[str, object]:
    mark = getattr(e, "problem_mark", None)
    return {"class": type(e).__name__, "str": str(e), "problem": str(getattr(e, "problem", e)),
            "line": (mark.line + 1) if mark is not None else None, "yaml_error": isinstance(e, yaml.YAMLError)}


def key_lines(text: str) -> list[list[object]] | None:
    try:
        root = yaml.compose(text)
    except yaml.YAMLError:
        return None
    out: list[list[object]] = []
    if isinstance(root, yaml.MappingNode):
        for k, _v in root.value:
            if isinstance(k, yaml.ScalarNode):
                out.append([k.value, k.start_mark.line + 1, k.start_mark.column + 1])
    return out


def main() -> None:
    cases = []
    for text in CORPUS:
        case: dict[str, object] = {"text": text}
        try:
            case["value"] = canon(yaml.safe_load(text))
        except Exception as e:  # noqa: BLE001 - every exception class is part of the golden
            case["error"] = err(e)
        case["key_lines"] = key_lines(text)
        cases.append(case)
    dumps = []
    for data in ({"credentials": [{"name": "litellm_dd_key", "type": "bearer", "scopes": ["read", "chat"],
                                   "injected_via": "LITELLM_DD_KEY", "target_hosts": ["172.16.0.5"]}]},
                 {"credentials": [{"name": "k", "type": "bearer", "scopes": [], "injected_via": "X",
                                   "target_hosts": ["host.example", "10.0.0.1"]}]},
                 {"a": ["on", "yes", "1.5", "null", "", " lead", "trail ", "x: y", "#c", "- d", "it's", "é", "a\tb",
                        "-", "?", ":x", "x:", "x #y", "*a", "&a", "!t", "%p", "@p", "`p", "1e5", "0x1f", "~"]}):
        dumps.append({"data": canon(data), "yaml": yaml.safe_dump(data, sort_keys=False)})
    out = {"generator": "backend/crates/nemo-console/tests/fixtures/aiperf/gen_pyyaml_golden.py",
           "pyyaml": yaml.__version__, "python": sys.version.split()[0], "cases": cases, "dumps": dumps}
    (HERE / "pyyaml_golden.json").write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n")
    # a seeded random corpus of YAML-ish fragments (differential fuzzing; 40 000 such cases matched while porting)
    rnd = random.Random(20261027)
    atoms = ["a", "b", "key", ":", ": ", "- ", "-", "[", "]", "{", "}", ",", " ", "  ", "\n", "\n  ", "\n- ", "#", " #c",
             "'", '"', "\\", "|", ">", "&x ", "*x", "!!str ", "!t ", "?", "? ", "1", "0x1", "1.5", "yes", "~", "null",
             "---", "...", "\t", "%", "@", "`", "\u00e9", "2026-10-24", "1:2", "<<: ", "=", "\r\n", "\\n", "\\x4", "a b",
             "|-\n  x\n", ">+\n  y\n\n"]
    fuzz = []
    while len(fuzz) < 1000:
        text = "".join(rnd.choice(atoms) for _ in range(rnd.randint(1, 30)))
        case = {"text": text}
        try:
            case["value"] = canon(yaml.safe_load(text))
        except RecursionError:
            continue
        except Exception as e:  # noqa: BLE001
            case["error"] = err(e)
        case["key_lines"] = key_lines(text)
        fuzz.append(case)
    (HERE / "pyyaml_fuzz_golden.json").write_text(json.dumps({"cases": fuzz, "dumps": []}, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
