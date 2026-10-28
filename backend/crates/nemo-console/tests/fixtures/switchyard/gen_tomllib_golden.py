"""Generate tomllib_golden.json: CPython 3.13 `tomllib.loads` on many inputs -> repr(result) or the error text.

Run from the worktree root with the backend venv:  REF=$(node parity/ref.mjs) && "$REF/.venv/bin/python" -I \
    backend/crates/nemo-console/tests/fixtures/switchyard/gen_tomllib_golden.py
The Rust port (src/adapters/switchyard/tomllib.rs) must reproduce every line (tests/switchyard_tomllib.rs).
"""
from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
RELAY = HERE.parent / "relay"

CASES = [
    "", "\n\n", "# only a comment\n", "a = 1", "a = 1\n", "a=1", "a = 'x'", 'a = "x"', "a = \"\"\"\nx\ny\"\"\"",
    "a = '''\nx'''", "a = '''x''''", "a = '''x'''''", 'a = """x"""""', 'a = """x""""""', "a = \"\\u00e9\\U0001F600\"",
    'a = "\\b\\t\\n\\f\\r\\"\\\\"', 'a = "\\x"', 'a = "\\uZZZZ"', 'a = "\\uD800"', 'a = "unterminated', "a = 'unterminated",
    'a = """\nline \\\n   continued"""', 'a = """x \\  y"""', 'a = "tab\there"', 'a = "nl\x01"',
    "a = true", "a = false", "a = tru", "a = 1_000", "a = 1__0", "a = _1", "a = 0x1F", "a = 0xDEAD_beef", "a = 0o17",
    "a = 0b1010", "a = 0b12", "a = 0x", "a = +1", "a = -1", "a = -0", "a = 012", "a = 1.5", "a = 1.", "a = .5",
    "a = 1e5", "a = 1E-5", "a = 1.5e+3", "a = 1e", "a = 6.02_2e2_3", "a = inf", "a = -inf", "a = +inf", "a = nan",
    "a = -nan", "a = +nan", "a = 99999999999999999999999999999999999999999", "a = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
    "a = 1979-05-27", "a = 1979-05-27T07:32:00Z", "a = 1979-05-27T07:32:00", "a = 1979-05-27 07:32:00.999999",
    "a = 1979-05-27T07:32:00.1234567-07:00", "a = 1979-05-27T00:32:00+09:30", "a = 1979-05-27t07:32:00z",
    "a = 2021-02-29", "a = 2020-02-29", "a = 2021-13-01", "a = 2021-04-31", "a = 07:32:00", "a = 07:32:00.5",
    "a = 24:00:00", "a = 1979-05-27T07:32", "a = 1979-05-27 x", "a = 1979-05-27T07:32:00+25:00",
    "a = [1, 2, 3]", "a = [1, 2, 3,]", "a = [\n 1, # c\n 2\n]", "a = [1 2]", "a = [", "a = [,]", "a = []",
    "a = [[1], ['x'], {b = 1}]", "a = {}", "a = { b = 1, c = 'd' }", "a = { b = 1, }", "a = { b = 1 c = 2 }",
    "a = { b = 1, b = 2 }", "a = { b.c = 1, b.d = 2 }", "a = { b = {c = 1}, b.d = 2 }", "a = {b = [1], b.c = 1}",
    "a = { b = 1\n}", "a.b = 1", "a.b = 1\na.c = 2", "a = 1\na = 2", "a.b = 1\na = 2", "a = 1\na.b = 2",
    "[a]\nb = 1\n[a]\nc = 2", "[a]\n[a.b]\nc = 1", "[a.b]\n[a]", "[a]\nb.c = 1\n[a.b]", "[a.b.c]\n[a]\nb.d = 1",
    "a = {}\n[a.b]", "a = [1]\n[[a]]", "[[a]]\nb = 1\n[[a]]\nb = 2", "[[a]]\n[a.b]\nc = 1\n[[a]]\n[a.b]",
    "[[a]]\n[a]", "[a]\n[[a]]", "[[a.b]]\n[a]\nc = 1", "[ a . b ]\nc = 1", "[\"quoted key\".'lit']\nx = 1",
    "\"\" = 1", "'' = 1", "a b = 1", "a = 1 b = 2", "a = 1 # comment", "a = 1 #\x01", "a = 'x' # é ok",
    "[a\nx=1", "[[a]\nx = 1", "[a]]", "[]", "[[]]", "= 1", "a =", "a = ", "#\n[a]\n#x\nb=1 # y\n", "a = 1\r\nb = 2\r\n",
    "a = 1\rb = 2", "﻿a = 1", "é = 1", "a.é = 1", "a.\"é\" = 1", "a = \"é\"\nb = [", "x\n= 1",
    "[routes.x\nid = \"a\"\n", "schema_version = 1\n[routes.x\nid = \"a\"\n", "a = [1,\n2,\n# end\n]",
    "a = { b = [1, 2], c = { d = 'e' } }", "a = 'it''s'", 'a = """a""b"""', "a = 1.0", "a = -0.0", "a = 1e400",
    "a = 5e-324", "a = 0.1", "a = 100000000000000000.0", "a = 1e16", "a = 0.0001", "a = 1e-5",
    "[a.b]\n[a.b.c]\n[a.b.c.d]\nx = 1", "a.b.c = 1\n[a]\nd = 2", "[a]\nb = {c = 1}\n[a.b.d]",
    "a = '''\n'''", 'a = """\n"""', "a = ' '", "[a]\n  b = 1\n\t[a.c]\n\td = 2",
]


def main() -> None:
    files = [HERE / "routes.good.toml", HERE / "routes.badtarget.toml", HERE / "routes.dup.toml",
             RELAY / "routes.toml", RELAY / "plugins.toml", RELAY / "config.toml"]
    inputs = list(CASES) + [f.read_text() for f in files]
    out = []
    for text in inputs:
        try:
            res = {"ok": repr(tomllib.loads(text))}
        except tomllib.TOMLDecodeError as e:
            res = {"err": str(e)}
        except RecursionError as e:
            res = {"err": str(e)}
        out.append({"input": text, **res})
    (HERE / "tomllib_golden.json").write_text(json.dumps(out, indent=1, ensure_ascii=True) + "\n")
    print(f"{len(out)} cases, python {sys.version.split()[0]}")


if __name__ == "__main__":
    main()
