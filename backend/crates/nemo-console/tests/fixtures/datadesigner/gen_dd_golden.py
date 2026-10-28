"""Golden generator: the Python DataDesignerAdapter's own outputs, for the Rust port to match exactly.

* "yaml": providers(), aliases() and _config_dump() over many model_providers.yaml / model_configs.yaml texts (hand
  cases + deterministic mutations); exceptions recorded as class + message.
* "copies": a full `create` run per case through a fake runner whose `data-designer-export` writes PREDETERMINED
  bytes per (batch, extension); records the bytes of the user's dataset copy, of records.jsonl, the run verdict and
  record count, and the copy's file mode. The Rust test feeds its fake exporter the same bytes.

    REF=$(node parity/ref.mjs) && cd backend && "$REF/.venv/bin/python" -I ../backend/crates/nemo-console/tests/fixtures/datadesigner/gen_dd_golden.py

Offline; writes only dd_golden.json next to this file.
"""
from __future__ import annotations

import asyncio
import base64
import json
import os
import random
import stat
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[5] / "backend" / "src"))

from nemo_console.adapters.base import AdapterContext, CommandResult  # noqa: E402
from nemo_console.adapters.datadesigner import DataDesignerAdapter  # noqa: E402

PROVIDERS = (HERE / "model_providers.yaml").read_text()
CONFIGS = (HERE / "model_configs.yaml").read_text()
TOOLSHAPE_P = ("providers:\n- name: lab-router\n  endpoint: http://127.0.0.1:4000/v1\n  provider_type: openai\n"
               "  api_key: LITELLM_DD_KEY\n- name: nvidia\n  endpoint: https://integrate.api.nvidia.com/v1\n")
TOOLSHAPE_C = ("model_configs:\n- alias: lab-text\n  model: glm-5.3-flash-next-local\n  provider: lab-router\n"
               "  inference_parameters:\n    generation_type: chat-completion\n    temperature: 1.0\n    top_p: 0.95\n"
               "    max_tokens: 4096\n- alias: lab-lightning\n  model: nemotron-lightning\n  provider: lab-router\n")
YAML_HAND = [
    (PROVIDERS, CONFIGS), (TOOLSHAPE_P, TOOLSHAPE_C), ("", ""), ("[]", "{}"), ("x: 1", "x: [1, 2]"),
    ("- name: a\n  endpoint: 5\n  provider_type: true\n", "- alias: a\n  model: [1, 2]\n  provider: {x: 1}\n"),
    ("- name: a\n  endpoint: 2026-10-23\n", "- alias: a\n  model: 2026-10-23 10:00:00+05:30\n  temperature: .inf\n"),
    ("- name: 'bad name'\n- name: ok\n- name: ''\n- 7\n", "- alias: a\n  max_tokens: .nan\n"),
    ("p1: {endpoint: e}\np2: {name: given, endpoint: f}\np3: 7\n", "a1: {model: m, temperature: 0.5}\nA2: {alias: x}\n"),
    ("- name: a\n  endpoint: !!binary aGVsbG8=\n", "- alias: a\n  inference_parameters: {temperature: 1, top_p: 0, max_tokens: 2.7}\n"),
    ("- name: a\n", "- alias: a\n  generation_type: null\n  inference_parameters: {generation_type: chat}\n"),
    ("- name: a\n", "- alias: a\n  inference_parameters: {generation_type: 5}\n  temperature: 0.3\n"),
    ("- name: a\n", "- alias: a\n  inference_parameters: {temperature: null}\n  temperature: 0.3\n"),
    ("- name: a\n", "- alias: a\n  max_tokens: 1" + "0" * 400 + "\n"),
    ("- name: a\n", "- alias: a\n  max_tokens: -0.5\n"),
    ("- name: a\n", "- alias: a\n  max_tokens: 1e300\n"),
    ("- name: a\n", "- alias: a\n  max_tokens: 1.0e+300\n"),
    ("- name: a\n  endpoint: !!int x\n", "- alias: a\n"),
    ("- name: a\n  endpoint: !!bool maybe\n", "- alias: a\n"),
    ("- name: a\n  endpoint: !!timestamp nope\n", "- alias: a\n"),
    ("- name: a\n  endpoint: &e [*e]\n", "- alias: a\n"),
    ("- name: a\n  endpoint: !!set {x, y}\n", "- alias: a\n  model: !!omap [k: 1]\n"),
    ("- name: a\n  endpoint: {k: 'it''s', b: \"q\\\"\"}\n", "- alias: a\n  model: [1.5, ~, yes, 1e5]\n"),
    ("{ not yaml !!!", "{ not yaml !!!"), ("\x01", "\x7f"),
    ("- name: " + "n" * 200 + "\n", "- alias: " + "a" * 100 + "\n  model: " + "m" * 200 + "\n  provider: p\n"),
]
ALPHABET = list("-:?[]{},#&*!|>'\" \n.~=1a0") + ["\n  ", ": ", "- ", "  ", "name", "alias", "temperature: "]


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
    return {"raised": {"class": type(e).__name__, "message": str(e)}}


class Runner:
    """--version ok; create writes metadata listing N batches; the exporter writes the case's bytes."""

    def __init__(self, case: dict) -> None:
        self.case = case

    async def run(self, argv, *, timeout=15.0, max_bytes=256_000, stdin=None, cwd=None):  # noqa: ANN001,ANN201
        if argv[1:] == ["--version"]:
            return CommandResult(returncode=0, stdout="0.9.3\n", stderr="")
        if argv[1] == "create":
            out = Path(argv[argv.index("--artifact-path") + 1]) / argv[argv.index("--dataset-name") + 1]
            (out / "parquet-files").mkdir(parents=True, exist_ok=True)
            n = len(self.case["batches"])
            for i in range(n):
                (out / "parquet-files" / f"batch_{i:05d}.parquet").write_bytes(b"PAR1" + bytes([i]) * 7)
            md = {"actual_num_records": self.case["records"],
                  "file_paths": {"parquet-files": [f"parquet-files/batch_{i:05d}.parquet" for i in range(n)]}}
            (out / "metadata.json").write_text(json.dumps(md))
            return CommandResult(returncode=0, stdout="", stderr="")
        if argv[0] == "data-designer-export":
            i = int(Path(argv[1]).stem.split("_")[1])
            ext = Path(argv[2]).suffix.lstrip(".")
            data = self.case["batches"][i].get(ext)
            if data is None:
                return CommandResult(returncode=2, stdout="", stderr=f"no {ext}")
            Path(argv[2]).write_bytes(base64.b64decode(data))
            return CommandResult(returncode=0, stdout="", stderr="")
        return CommandResult(returncode=127, stdout="", stderr="no fake")


def b64(b: bytes) -> str:
    return base64.b64encode(b).decode()


def jl(rows: list) -> bytes:
    return "".join(json.dumps(r) + "\n" for r in rows).encode()


def copy_cases() -> list[dict]:
    r2 = [{"product_category": "Books", "review": "a, \"b\""}, {"product_category": "E", "review": "x\ny"}]
    csv2 = b'product_category,review\r\nBooks,"a, ""b"""\r\nE,"x\ny"\r\n'
    odd = b'  {"a": 1}\r\n\n\x0b{"a": 2}\x0c \n   \n{"a": 3}'
    big = jl([{"i": i} for i in range(1500)])
    cases = []
    for fmt in ("parquet", "csv", "jsonl", "json"):
        cases.append({"name": f"{fmt}-one", "format": fmt, "records": 2,
                      "batches": [{"jsonl": b64(jl(r2)), "csv": b64(csv2)}]})
        cases.append({"name": f"{fmt}-three", "format": fmt, "records": 6,
                      "batches": [{"jsonl": b64(jl(r2)), "csv": b64(csv2)}] * 3})
        cases.append({"name": f"{fmt}-odd-lines", "format": fmt, "records": 3,
                      "batches": [{"jsonl": b64(odd), "csv": b64(b"h\r\n1\r\n2")}, {"jsonl": b64(odd), "csv": b64(b"h\n3\n")}]})
        cases.append({"name": f"{fmt}-empty", "format": fmt, "records": 0,
                      "batches": [{"jsonl": b64(b""), "csv": b64(b"")}, {"jsonl": b64(b"\n \n"), "csv": b64(b"h\n")}]})
        cases.append({"name": f"{fmt}-cap", "format": fmt, "records": 3000,
                      "batches": [{"jsonl": b64(big), "csv": b64(b"i\n" + b"".join(b"%d\n" % i for i in range(1500)))}] * 2
                      + [{"jsonl": b64(jl([{"late": 1}])), "csv": b64(b"i\nlate\n")}]})
        cases.append({"name": f"{fmt}-export-fails", "format": fmt, "records": 2,
                      "batches": [{"jsonl": b64(jl(r2))}, {"csv": b64(csv2)}]})
    return cases


async def run_copy(tmp: Path, case: dict) -> dict:
    scripts = tmp / "scripts"
    scripts.mkdir(parents=True)
    (scripts / "s.py").write_text("x\n")
    root = tmp / "Datasets"
    ctx = AdapterContext(runner=Runner(case), http=None, urls={"litellm": "http://127.0.0.1:9"},  # type: ignore[arg-type]
                         paths={"scripts_dir": scripts, "datasets_root": root}, state_dir=tmp / "state")
    a = DataDesignerAdapter(ctx)
    res = await a.run_action("create", {"script": "s.py", "num_records": 5, "format": case["format"],
                                        "dataset_name": "ds", "destination_subdir": "out"})
    assert res.ok, res.output
    assert a._live is not None
    await a._live.task
    run_dir = next((tmp / "state" / "datadesigner" / "runs").iterdir())
    meta = json.loads((run_dir / "run.json").read_text())
    target = root / "out" / f"ds.{case['format']}"
    rec = run_dir / "records.jsonl"
    copies = sorted(p.name for p in (root / "out").iterdir()) if (root / "out").exists() else []
    return {"verdict": meta["verdict"], "records": meta.get("records"), "batches": meta.get("batches"),
            "target": b64(target.read_bytes()) if target.is_file() else None,
            "target_mode": oct(stat.S_IMODE(os.stat(target).st_mode)) if target.is_file() else None,
            "records_jsonl": b64(rec.read_bytes()) if rec.is_file() else None,
            "records_mode": oct(stat.S_IMODE(os.stat(rec).st_mode)) if rec.is_file() else None,
            "dest_files": copies}


async def run_yaml(tmp: Path, prov: str, conf: str) -> dict:
    cfg = tmp / ".data-designer"
    cfg.mkdir(parents=True)
    (cfg / "model_providers.yaml").write_text(prov)
    (cfg / "model_configs.yaml").write_text(conf)
    ctx = AdapterContext(runner=None, http=None, urls={}, paths={"dd_config_dir": cfg},  # type: ignore[arg-type]
                         state_dir=tmp / "state")
    a = DataDesignerAdapter(ctx)
    out: dict = {"providers_yaml": prov, "configs_yaml": conf}
    try:
        out["providers"] = a.providers()
    except Exception as e:  # noqa: BLE001
        out["providers"] = exc(e)
    try:
        # floats as their repr (inf / nan have no JSON spelling)
        out["aliases"] = [{**x, **{k: (repr(x[k]) if x[k] is not None else None)
                                   for k in ("temperature", "top_p", "max_tokens")}} for x in a.aliases()]
    except Exception as e:  # noqa: BLE001
        out["aliases"] = exc(e)
    try:
        out["dump"] = a._config_dump()
    except Exception as e:  # noqa: BLE001
        out["dump"] = exc(e)
    return out


async def main() -> int:
    rng = random.Random(9001)
    pairs = list(YAML_HAND)
    for i in range(400):
        base = YAML_HAND[i % 3]
        pairs.append((mutate(rng, base[0]), mutate(rng, base[1])))
    yaml_cases = []
    copies = []
    with tempfile.TemporaryDirectory() as td:
        for i, (p, c) in enumerate(pairs):
            yaml_cases.append(await run_yaml(Path(td) / f"y{i}", p, c))
        for i, case in enumerate(copy_cases()):
            copies.append({**case, "out": await run_copy(Path(td) / f"c{i}", case)})
    text = json.dumps({"yaml": yaml_cases, "copies": copies}, indent=0, ensure_ascii=True, allow_nan=False)
    (HERE / "dd_golden.json").write_text(text + "\n")
    print(f"{len(yaml_cases)} yaml cases, {len(copies)} copy cases", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
