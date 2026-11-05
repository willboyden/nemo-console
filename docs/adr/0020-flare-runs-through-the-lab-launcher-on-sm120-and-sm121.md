# ADR-0020: NVIDIA Flare runs through the lab launcher, on sm120 and sm121 targets; the console never talks to a remote engine

Status: Accepted (0.10.0)

## Context

NVIDIA Flare (github.com/NVIDIA/spark-local-inference) is NVIDIA's research-prototype inference server in Rust. It does
not load a checkpoint: it serves a package that its preparation tool, Ignition, builds once per GPU family and
tensor-parallel size, on that family's own GPUs. A gb202 package (sm120: RTX PRO 6000, RTX 5090) fails the
architecture check on a DGX Spark (gb10, sm121), and a TP2 package cannot load as TP1. Flare can also run one engine
across several Sparks, one rank per host over RoCE.

The operator wants Flare on both chip families: the two RTX PRO 6000s on the console's own host, the RTX 5090 of a
second box, one Spark, or all four Sparks as one engine. Everything but the first is a remote host. The GPUs are
shared with production engines, so starting Flare must never stop one.

The console's own outbound HTTP is loopback-only by design (settings refuse any other base URL), which closes SSRF
through settings.

## Decision

1. **One launcher does everything: `flare-local`** (the lab's `inference/flare/flare.sh` linked onto PATH). It knows
   the targets (names, hosts, chip and tensor-parallel sizes; the console learns them from the launcher's report and passes only names it listed), reaches remote hosts over ssh, refuses to start while a target's GPU looks busy (on a remote host a running
   container with a GPU request, other than Flare's own or the DCGM exporter, or a CUDA process; on the console host
   more than 3 GiB used on a card) and never stops another engine, and reports every target as one JSON array
   (`flare-local status-json`). The console adds `flare-local` to the runner's allowlist and passes only validated
   argv: a target the launcher listed, a package name matching `[A-Za-z0-9][A-Za-z0-9._-]*`, a mode the target offers,
   checked before anything runs.
2. **The console never calls a remote engine.** Status, metrics (Flare's own `/metrics?format=json`) and probes of the
   remote targets come through the launcher. The playground talks only to the local target on loopback.
   Settings stay loopback-only.
3. **Remote targets need a serving key.** They listen on the private fabric, so Flare runs there with
   `--api-key-file` (the other fabric lanes all require a key). The key is made once on the console host (0600,
   never printed), copied to each host 0600 by `stage`, mounted read-only, and sent by the launcher through curl's
   stdin config, so it never appears in a process's arguments, and only to an engine whose `/health` answers.
   `/health` and `/metrics` stay open (upstream). The fabric hop is cleartext HTTP: the fabric is private, and the key
   only gates who may generate.
4. **Exact container names per kind**: a one-host engine is `ai-lab-flare`, the four-Spark ranks are
   `ai-lab-flare-r0..r3`, so stopping one Spark never stops a rank of the four-Spark engine, and either start refuses
   while the other kind runs on that host.
5. **Starting is a background run, one at a time**, like Model Optimizer and Gym (ADR-0017, ADR-0018): loading can take
   many minutes (for the Sparks a package transfer over the fabric too); the status shows it and "Show the last start"
   prints the launcher's output.
6. **The lane's files are read-only** in the console (compose files, the Ignition config): a bare compose start would
   skip the launcher's checks.
7. **The status check never blocks a poll.** `status-json` makes one ssh call per host with hard deadlines, in
   parallel, and reports an unreachable host as such; the console runs it at most once a minute, in the background,
   one at a time, and answers every poll from the last report.

## Consequences

- Flare appears as a thirteenth system with the generic view. Its health is `down` while no target serves, which is
  the normal state of an experiment lane; the problem says what to do next (prepare a package, or start one).
- Preparing packages and building the arm64 serving image (on a Spark: the console host has no aarch64 emulation) are
  operator steps outside the console, because they download build inputs once and hold GPUs for hours.
- The Python reference has no Flare adapter: parity deviation D-33 (`backend/PORTING.md`).
- The remote paths are written against upstream's documented multi-host manual mode; until a package has been served
  on each family they are **unverified**.

## Alternatives considered

- **Let the console call the remote engines over HTTP.** Rejected: it would end the loopback-only rule that keeps
  settings from becoming an SSRF primitive.
- **A per-host launcher on each remote box.** Rejected for now: one launcher on the console host keeps one place for
  the GPU-free checks and the key handling; the hosts only run Docker.
- **Download the community's ready packages.** Rejected: their licence is not accepted for this lab; packages are
  prepared from NVIDIA's own checkpoints.
