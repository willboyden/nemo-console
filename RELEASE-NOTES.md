# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.10.0 — 2026-11-05

An NVIDIA Flare page, for both GPU families the lab runs: sm120 and sm121.

- **NVIDIA Flare** is NVIDIA's research-prototype inference server in Rust. It serves a package prepared once per GPU
  family and tensor-parallel size, on that family. The new page shows every target the lab launcher defines (local
  RTX PRO 6000s, a remote RTX 5090, one DGX Spark, or several Sparks as one engine) with its chip, packages, image and
  whether its GPUs are free, plus Flare's own counters while it serves.
- **Guarded starts:** every action runs the launcher, which refuses to start while a target's GPUs look busy (a GPU counts as busy when the launcher sees, on a remote host, a running container with a GPU request (other than Flare's own or the DCGM exporter) or a CUDA process, and on the console host more than 3 GiB of memory used on a card) and never stops another engine. The console passes only names the launcher listed and a mode the target
  offers. Remote engines need a serving key, and the console never calls them itself.
- **Start in the background, one at a time;** then Probe sends one real completion. The lane's files are read-only,
  with field help.
- **Not yet proven on hardware:** no package has been prepared or served yet, because production engines held the
  GPUs. The start paths follow upstream's documentation.

Upgrade: `git pull`, `make release`, then `backend/target/release/nemo-console stop` and `... open` (or `make run`).
Then link the launcher once: `ln -s <lab>/inference/flare/flare.sh ~/.local/bin/flare-local`.
