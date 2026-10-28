"""Generate the gzipped ATOF rotations the Relay tests read (Python's own gzip writer, deterministic mtime).

Run from the worktree root:  REF=$(node parity/ref.mjs) && "$REF/.venv/bin/python" -I backend/crates/nemo-console/tests/fixtures/relay/gen_gz_fixtures.py
The Python tests build the same files on the fly with gzip.open(path, "wb"); the Rust tests copy these instead.
"""
from __future__ import annotations

import gzip
import io
from pathlib import Path

HERE = Path(__file__).resolve().parent


def with_name(text: str, name: str) -> bytes:
    """Like gzip.open(path, "wb"): the header carries the file name (FNAME)."""
    buf = io.BytesIO()
    with gzip.GzipFile(filename=name, mode="wb", fileobj=buf, mtime=0) as gz:
        gz.write(text.encode())
    return buf.getvalue()


def main() -> None:
    sample = (HERE / "atof_sample.jsonl").read_text()
    lines = sample.splitlines()
    out = {
        "atof_sample.jsonl.gz": gzip.compress(sample.encode(), mtime=0),
        "atof_head.jsonl.gz": with_name("\n".join(lines[:-5]) + "\n", "relay-events.jsonl.2"),
        "atof_all.jsonl.gz": with_name("\n".join(lines) + "\n", "relay-events.jsonl.2"),
        "atof_line2.jsonl.gz": with_name(lines[2] + "\n", "outside.jsonl"),
        "two_members.gz": gzip.compress(b"first member\n", mtime=0) + b"\x00\x00" + gzip.compress(b"second\n", mtime=0),
    }
    for name, data in out.items():
        (HERE / name).write_bytes(data)
        assert gzip.decompress(data)
    print(", ".join(f"{k} ({len(v)} B)" for k, v in out.items()))


if __name__ == "__main__":
    main()
