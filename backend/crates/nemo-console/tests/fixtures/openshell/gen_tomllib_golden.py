"""Generate tomllib_golden.json: what CPython 3.13's tomllib answers for a corpus of documents (valid and invalid), so
the Rust port in src/adapters/openshell/tomllib.rs is pinned to it (PORTING.md risk R-3).

    REF=$(node parity/ref.mjs) && "$REF/.venv/bin/python" -I backend/crates/nemo-console/tests/fixtures/openshell/gen_tomllib_golden.py

Values are written as a canonical tagged form; errors as (class, str(e)). Offline; reads only the fixtures here.
"""
from __future__ import annotations

import datetime
import json
import random
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent

CORPUS: list[str] = [
    "", "a = 1\n", "[openshell\n", "a = \n", "x = 1\ny = 'two'\nz = \"th\\u00e9ree\"\n", "[a]\nb = 1\n[a]\nc = 2\n",
    "[a.b]\nc = 1\n[a]\nd = 2\n", "a = {b = 1, c = [1, 2]}\n", "a = {b = 1,}\n", "a = [1, 2,]\n", "a = [\n  1,\n  # c\n  2\n]\n",
    "t = true\nf = false\n", "d = 1979-05-27\nt = 07:32:00.999999\ndt = 1979-05-27T07:32:00Z\nlt = 1979-05-27 07:32:00.5\n",
    "o = 1979-05-27T00:32:00-07:00\np = 1979-05-27T00:32:00+00:00\n", "bad = 1979-02-30\n", "bad = 0000-01-01\n",
    "h = 0xDEAD_beef\no = 0o755\nb = 0b1101\nn = -17\np = +3\nu = 1_000\n", "f = 6.626e-34\ng = -0.0\nh = 1e400\ni = inf\nj = -nan\n",
    "s = '''\nmulti\nline'''\nb = \"\"\"\\\n  folded \\\n  text\"\"\"\n", "q = \"\"\"a\"\"\"\"\"\n", "x = \"\\e\"\n", "x = \"\\q\"\n",
    "x = \"tab\there\"\n", "x = \"ctrl\x01\"\n", "x = 'unterminated\n", "x = \"unterminated\n", "a.b.c = 1\n[a]\nx = 1\n",
    "a = 1\na = 2\n", "a = {b = 1}\n[a]\nc = 1\n", "[[arr]]\nx = 1\n[[arr]]\nx = 2\n", "arr = []\n[[arr]]\n",
    "a = [1]\n[a.b]\n", "a = []\n[a.b]\n", "[a]\n[[a]]\n", "1234 = 'num key'\n'quoted key' = 1\n\"dq\" = 2\n",
    "= 1\n", "a b = 1\n", "a = 1 b = 2\n", "a = 01\n", "a = 1__0\n", "a = .5\n", "a = 5.\n", "a = 1e\n", "a = tru\n",
    "[a] # comment\nb = 1 # c\n", "# only comment\n", "a = \"\\uD800\"\n", "a = \"\\U0010FFFF\"\n", "a = \"\\u00G0\"\n",
    "a = {b = {c = 1}, b.d = 2}\n", "a = {b = 1, b = 2}\n", "x = 1\r\ny = 2\r\n", "[a]\nb.c = 1\n[a.b]\n", "a.b = 1\na = 2\n",
    "\ta = 1\n", "[ a . b ]\nc = 1\n", "k = \"\"\"\nx\n\"\"\"\"\n", "k = '''a''''\n", "k = 1979-05-27T25:00:00\n", "k = 24:00:00\n",
    "[x]\n[x.y]\n[x]\n", "[[x]]\n[x.y]\n[[x]]\n", "a = [[1], [2, [3]]]\n", "a = {}\n", "a = [ ]\n", "a=1\nb=-0x1\n",
]
CORPUS.append((HERE / "runner" / "config" / "gateway.toml").read_text())


def canon(v: object) -> object:
    if isinstance(v, bool):
        return ["bool", v]
    if isinstance(v, int):
        return ["int", str(v)]
    if isinstance(v, float):
        return ["float", repr(v)]
    if isinstance(v, str):
        return ["str", v]
    if isinstance(v, datetime.datetime):
        return ["datetime", str(v)]
    if isinstance(v, datetime.date):
        return ["date", str(v)]
    if isinstance(v, datetime.time):
        return ["time", str(v)]
    if isinstance(v, list):
        return ["array", [canon(x) for x in v]]
    if isinstance(v, dict):
        return ["table", [[k, canon(x)] for k, x in v.items()]]
    raise TypeError(type(v).__name__)


def run(text: str) -> dict[str, object]:
    case: dict[str, object] = {"text": text}
    try:
        case["value"] = canon(tomllib.loads(text))
    except Exception as e:  # noqa: BLE001 - every exception class is part of the golden
        case["error"] = {"class": type(e).__name__, "str": str(e)}
    return case


def main() -> None:
    cases = [run(t) for t in CORPUS]
    rnd = random.Random(20261027)
    atoms = ["a", "b", "=", " = ", " ", "\n", "[", "]", "[[", "]]", "{", "}", ",", ".", '"', "'", "\\", "#", "1", "0x", "1.5",
             "true", "1979-05-27", "T07:32:00", "Z", "+", "-", "_", "e", "inf", "\t", "\"\"\"", "'''", "x", "\\u00", "é"]
    fuzz = []
    while len(fuzz) < 1500:
        fuzz.append(run("".join(rnd.choice(atoms) for _ in range(rnd.randint(1, 25)))))
    out = {"generator": "backend/crates/nemo-console/tests/fixtures/openshell/gen_tomllib_golden.py",
           "python": sys.version.split()[0], "cases": cases, "fuzz": fuzz}
    (HERE / "tomllib_golden.json").write_text(json.dumps(out, indent=0, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
