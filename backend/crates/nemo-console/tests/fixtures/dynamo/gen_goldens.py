"""Generate yaml_golden.json and shlex_golden.json: PyYAML 6 `yaml.safe_load` and CPython 3.13 `shlex.split` on the
inputs the Dynamo adapter meets (compose files, overrides, edge cases). The Rust ports (src/adapters/dynamo/pyyaml.rs,
shlex.rs) must reproduce every value; YAML error cases must also fail (their wording comes from yaml-rust2).

Run from the worktree root:  REF=$(node parity/ref.mjs) && "$REF/.venv/bin/python" -I backend/crates/nemo-console/tests/fixtures/dynamo/gen_goldens.py
"""
from __future__ import annotations

import json
import shlex
from pathlib import Path

import yaml

HERE = Path(__file__).resolve().parent

YAML_CASES = [
    "", "~", "a: 1", "a: yes\nb: No\nc: on\nd: OFF\ne: y\nf: n", "a: null\nb: Null\nc: ~\nd:", "a: 0x1F\nb: 0o17\nc: 017\nd: 08\ne: 0b101",
    "a: 1_000\nb: +12\nc: -0\nd: 1:30\ne: 190:20:30", "a: 1.5\nb: 1e5\nc: 1.0e+5\nd: .5\ne: 1.\nf: .inf\ng: -.Inf\nh: .NaN\ni: 1_0.5",
    "a: 2026-01-02\nb: 2026-01-02T03:04:05Z\nc: 2026-01-02 03:04:05.5 +02:00\nd: 2026-1-2t3:04:05", "a: '8'\nb: \"yes\"\nc: !!str 12\nd: !!int '7'",
    "- a\n- [b, c]\n- {d: e}", "x: &c {a: 1, b: 2}\ny:\n  <<: *c\n  b: 3", "x: &a {k: 1}\nz: &b {k: 2, m: 3}\ny:\n  <<: [*a, *b]\n",
    "a: 1\na: 2", "1: a\ntrue: b", "a: |\n  line1\n  line2\n", "a: >\n  folded\n  text\n\n  para\n", "a: >-\n  x\n  y\n",
    "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm, --max-num-seqs, '4']\n",
    "services:\n  dynamo-worker:\n    privileged: true\n", "services:\n  evil:\n    command: [x]\n",
    "services:\n  dynamo-frontend:\n    environment:\n      DYN_LOGGING_JSONL: \"${LITELLM_API_KEY}\"\n",
    "a: [", "services:\n  x: [y\n", "a: b: c", "- a\nb: c", "a: \"x", "a: *nope", "{a", "a: 1\n---\nb: 2", "? [1]\n: 2",
    "key: 'it''s'", "key: \"tab\\tnew\\nline\"", "e: =", "=: 1", "a: !!python/object:os.system x",
]
SHLEX_CASES = [
    "a b  c", "'a b' \"c d\"", "a\\ b", "\"a\\\"b\"", "\"a\\$b\"", "\"a\\\\b\"", "'a\\b'", "x '' y", "#c d", "a#b",
    "--speculative-config '{\"method\":\"mtp\",\"num_speculative_tokens\":2}'", "a\tb\nc\rd", "", "   ",
    "'unclosed", "trailing\\", "a\"b c\"d", "\\\"", "\"\"", "'' ''",
]


def main() -> None:
    files = [HERE / "base_compose.yml", HERE / "golden_override.yml"]
    ycases = YAML_CASES + [f.read_text() for f in files]
    yout = []
    for text in ycases:
        try:
            yout.append({"input": text, "ok": repr(yaml.safe_load(text))})
        except yaml.YAMLError as e:
            yout.append({"input": text, "err": type(e).__name__, "problem": str(getattr(e, "problem", e))})
        except ValueError as e:
            yout.append({"input": text, "err": type(e).__name__, "problem": str(e)})
    base = yaml.safe_load((HERE / "base_compose.yml").read_text())
    scases = SHLEX_CASES + [base["services"]["dynamo-worker"]["command"]]
    sout = []
    for text in scases:
        try:
            sout.append({"input": text, "ok": shlex.split(text)})
        except ValueError as e:
            sout.append({"input": text, "err": str(e)})
    (HERE / "yaml_golden.json").write_text(json.dumps(yout, indent=1) + "\n")
    (HERE / "shlex_golden.json").write_text(json.dumps(sout, indent=1) + "\n")
    print(f"{len(yout)} yaml, {len(sout)} shlex cases")


if __name__ == "__main__":
    main()
