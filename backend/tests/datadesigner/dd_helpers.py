"""Test helpers for the Data Designer adapter: a fake runner that writes real-shaped artifacts (captured live on
2026-10-23, trimmed) and a scripts tree. Everything offline: no network, no real CLI."""
from __future__ import annotations

import csv
import io
import json
import shutil
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path

import httpx

from nemo_console.adapters.base import AdapterContext, CommandResult

FIXTURES = Path(__file__).parent / "fixtures"
LAUNCHER = "data-designer-local"
EXPORTER = "data-designer-export"
PREVIEW_STAMP = "20261023_222454"       # the captured run's stamp (fixtures/preview-artifacts)


def fx(name: str) -> str:
    return (FIXTURES / name).read_text()


Responder = CommandResult | Callable[[list[str]], CommandResult | Awaitable[CommandResult]]


@dataclass
class FakeRunner:
    """Matches argv by prefix (longest first). Unmatched argv -> rc 127 so a missing stub is loud, not silent."""

    rules: list[tuple[list[str], Responder]] = field(default_factory=list)
    calls: list[list[str]] = field(default_factory=list)
    cwds: list[Path | None] = field(default_factory=list)

    def on(self, prefix: list[str], result: Responder) -> FakeRunner:
        self.rules = [r for r in self.rules if r[0] != prefix] + [(prefix, result)]
        return self

    async def run(self, argv: list[str], *, timeout: float = 15.0, max_bytes: int = 256_000,
                  stdin: bytes | None = None, cwd: Path | None = None) -> CommandResult:
        self.calls.append(list(argv))
        self.cwds.append(cwd)
        for prefix, res in sorted(self.rules, key=lambda r: -len(r[0])):
            if argv[: len(prefix)] == prefix:
                out = res(argv) if callable(res) else res
                return await out if inspect_awaitable(out) else out
        return CommandResult(returncode=127, stdout="", stderr=f"no fake for {argv}")


def inspect_awaitable(out: object) -> bool:
    import inspect
    return inspect.isawaitable(out)


def ok(stdout: str = "", rc: int = 0, stderr: str = "") -> CommandResult:
    return CommandResult(returncode=rc, stdout=stdout, stderr=stderr)


def _rows() -> list[dict[str, object]]:
    return [json.loads(line) for line in fx("dd_smoke.jsonl").splitlines() if line.strip()]


def export_responder(rc: int = 0, rows_override: list[dict[str, object]] | None = None
                     ) -> Callable[[list[str]], CommandResult]:
    """`data-designer-export IN.parquet OUT.ext`: the extension picks the format; written from the captured
    dd_smoke.jsonl (or an override for the records-cap test)."""
    def respond(argv: list[str]) -> CommandResult:
        out = Path(argv[2])
        rows = rows_override if rows_override is not None else _rows()
        if out.suffix == ".jsonl":
            out.write_text("".join(json.dumps(r) + "\n" for r in rows))
        elif out.suffix == ".csv":
            buf = io.StringIO()
            w = csv.DictWriter(buf, fieldnames=sorted(rows[0]))
            w.writeheader()
            w.writerows(rows)
            out.write_text(buf.getvalue())
        elif out.suffix == ".json":
            out.write_text(json.dumps(rows))
        else:
            out.write_bytes(b"PARQUET")
        return ok("", rc=rc)
    return respond


def writes_preview(rc: int = 0, stderr: str = "") -> Callable[[list[str]], CommandResult]:
    """`data-designer-local preview SCRIPT -n N --save-results -o DIR`: the captured preview_results_* tree."""
    def respond(argv: list[str]) -> CommandResult:
        out = Path(argv[argv.index("-o") + 1]) / f"preview_results_{PREVIEW_STAMP}"
        (out / "sample_records").mkdir(parents=True, exist_ok=True)
        shutil.copy(FIXTURES / "preview-artifacts" / "dataset.parquet", out / "dataset.parquet")
        shutil.copy(FIXTURES / "preview-artifacts" / "report.html", out / "report.html")
        for p in (FIXTURES / "sample_records").iterdir():
            shutil.copy(p, out / "sample_records" / p.name)
        return ok("preview progress lines", rc=rc, stderr=stderr)
    return respond


def writes_create(*, rc: int = 0, stderr: str = "", batches: int = 1, records: int | None = None,
                  with_metadata: bool = True, with_file_paths: bool = True) -> Callable[[list[str]], CommandResult]:
    """`data-designer-local create SCRIPT --num-records N --dataset-name NAME --artifact-path DIR --no-tui`:
    writes DIR/<NAME>/ with the captured metadata.json (+ batch parquets). `records`/`batches` override
    actual_num_records / the number of batch files (a generated metadata.json, same shape)."""
    def respond(argv: list[str]) -> CommandResult:
        out = Path(argv[argv.index("--artifact-path") + 1]) / argv[argv.index("--dataset-name") + 1]
        (out / "parquet-files").mkdir(parents=True, exist_ok=True)
        shutil.copy(FIXTURES / "parquet-files" / "batch_00000.parquet", out / "parquet-files" / "batch_00000.parquet")
        for i in range(1, batches):
            shutil.copy(FIXTURES / "parquet-files" / "batch_00000.parquet", out / "parquet-files" / f"batch_{i:05d}.parquet")
        if with_metadata:
            md: dict[str, object] = json.loads(fx("create-artifacts/metadata.json"))
            if records is not None:
                md["actual_num_records"] = records
            if not with_file_paths:
                md["file_paths"] = {}
            else:
                md["file_paths"] = {"parquet-files": [f"parquet-files/batch_{i:05d}.parquet" for i in range(batches)]}
            (out / "metadata.json").write_text(json.dumps(md))
        shutil.copy(FIXTURES / "create-artifacts" / "builder_config.json", out / "builder_config.json")
        return ok("create progress lines", rc=rc, stderr=stderr)
    return respond


def standard_runner(launcher: str = LAUNCHER) -> FakeRunner:
    r = FakeRunner()
    r.on([launcher, "--version"], ok("0.9.3\n"))
    r.on([launcher, "preview"], writes_preview())
    r.on([launcher, "create"], writes_create())
    r.on([launcher, "check-models"], ok("All 2 models reachable.\nlab-router: ok\nlab-embed: ok\n"))
    r.on([launcher, "validate"], ok("Config is valid.\n"))
    r.on([EXPORTER], export_responder())
    return r


def build_scripts(tmp: Path) -> Path:
    """Idempotent: a scripts dir with two valid scripts plus every kind of rejected entry."""
    root = tmp / "home" / "Datasets" / "data-designer" / "scripts"
    if root.is_dir():
        return root
    root.mkdir(parents=True)
    (root / "reviews_smoke.py").write_text(
        '"""Smoke config (fixture)."""\n\n\ndef load_config_builder():\n    raise NotImplementedError\n')
    (root / "tiny.yaml").write_text("columns:\n  - name: product_category\n    column_type: sampler\n")
    (root / "UPPER.py").write_text("# uppercase names are outside the core's id shape\n")
    (root / "notes.txt").write_text("wrong extension\n")
    (root / "subdir.py").mkdir()                          # a directory, not a file
    (root / "linked.py").symlink_to(tmp / "outside.py")   # a symlink is never offered
    (tmp / "outside.py").write_text("secret = True\n")
    return root


def make_ctx(tmp: Path, runner: object, http: httpx.AsyncClient, *, scripts: bool = True, providers: bool = True,
             repo: Path | None = None, key_file: bool = True) -> AdapterContext:
    paths: dict[str, Path] = {"datasets_root": tmp / "home" / "Datasets",
                              "dd_config_dir": tmp / "home" / ".data-designer",
                              "dd_key_file": tmp / "home" / ".config" / "data-designer" / "litellm.key"}
    if scripts:
        paths["scripts_dir"] = build_scripts(tmp)
    if providers:
        paths["dd_config_dir"].mkdir(parents=True, exist_ok=True)
        (paths["dd_config_dir"] / "model_providers.yaml").write_text(fx("model_providers.yaml"))
        (paths["dd_config_dir"] / "model_configs.yaml").write_text(fx("model_configs.yaml"))
    if key_file:
        paths["dd_key_file"].parent.mkdir(parents=True, exist_ok=True)
        paths["dd_key_file"].write_text("sk-FAKE-not-a-real-key-0123456789\n")   # EXISTENCE only; never read
        paths["dd_key_file"].chmod(0o600)
    if repo is not None:
        paths["repo"] = repo
    return AdapterContext(runner=runner, http=http, urls={"litellm": "http://127.0.0.1:4000"},  # type: ignore[arg-type]
                          paths=paths, state_dir=tmp / "state")
