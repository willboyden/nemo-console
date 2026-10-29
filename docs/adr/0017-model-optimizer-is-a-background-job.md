# ADR-0017: Model Optimizer is a background job the console starts and then only reads

Status: Accepted (2026-10-29, addendum 10.9)

## Context

NVIDIA Model Optimizer compresses a checkpoint: quantization, Minitron/Puzzletron layer pruning, or REAP expert
pruning. A job holds a GPU for hours. The console is an HTTP UI on one workstation: a request that blocks for
hours is a request whose client gives up, and a browser tab is the wrong place to watch a compression.

The other CLI-driven systems (AIPerf, NeMo Evaluator) fit the console's existing shape because they finish in
minutes and are bounded by a router. Model Optimizer is not: the work is minutes-to-hours of GPU time and it
writes a new checkpoint.

The independent audit of the wrapper (`llmcompressor` 0.15.0 and `compressed-tensors` 0.20.1, both wheels only and
sha256-pinned) came back ALLOW-WITH-CHANGES: the container must have no egress, no `trust_remote_code`, no
`HF_TOKEN`, and the model store is a read-only mount. `auto-round` is the one compiled-binary supply-chain edge;
`nvidia-modelopt` does not cover REAP expert pruning (its pruning is Minitron/Puzzletron and needs the NeMo
container plus Megatron-Bridge).

## Decision

The console is a **starter and a reader**, never the owner of a job.

1. **The wrapper owns the job.** The lab's `modelopt-local` wrapper starts the work in the background and
   returns at once. `modelopt-local run --job-id ID --recipe NAME --run-dir DIR` returns immediately and leaves
   the verdict to a file: `DIR/run.json` with `job_id`, `recipe`, `method`, `base_model`, `output`,
   `verdict` (`running` | `succeeded` | `failed` | `error`), `ts`, `duration_ms`, `metrics`
   (`output_bytes`, `base_bytes`, `ratio`) and `error`.
2. **The job directory is the contract.** The console owns the directory layout (`<state>/modelopt/jobs/<job-id>/`)
   and reads each `run.json` from it, read-only, size-capped. It never writes a job's own files.
3. **Every metric is derived, never invented.** `mo.output_bytes`, `mo.compression_ratio` and `mo.duration_s`
   come from a finished job's `run.json`; `mo.jobs`, `mo.live_running` and `mo.jobs_by_verdict` are counted from
   the job directories. If there is no job, the console reports no job: it does not estimate.
4. **`recipes.yaml` is the only editable file**, and it is the console's own schema (`version: 1`, one entry
   per recipe, `method` in `reap` | `quantize` | `minitron`). It is validated before it is written and the
   candidate is refused with line numbers. Which methods a lab actually implements is the lab's business: the
   console never ships the compression code itself.
5. **A run is disruptive and asks first.** The `run` action is classified `disruptive` (it takes a GPU for
   hours); `report` is `safe`. There is no console action that stops, cancels or rewrites a running job: the
   console does not own the container's lifecycle.
6. **The console never holds the model store.** It reads the recipe file and the job directories; the base
   checkpoint is a read-only mount inside the container, and no weight path is a console setting.

## Consequences

- A job started in the console keeps running after the browser tab is closed, and the console shows it as
  `running` until the wrapper rewrites the verdict. That is the honest state: the console cannot know more than
  the file says.
- The console learns the size and the ratio of a finished checkpoint only after the wrapper has written them.
  A job that dies before it writes a verdict stays `running` until the wrapper's own timeout marks it `error`.
- The audit's conditions are the wrapper's conditions, so the console's trust story is the wrapper's: if the
  wrapper's container gained egress, the console would not notice. The console's own promise stays small: it
  starts a local CLI and reads a file.
- REAP expert pruning is out of reach for the stock `nvidia-modelopt` package. A recipe may name the method;
  whether the lab's wrapper can serve it is a lab question, and the console does not pretend otherwise.

## Alternatives

- **Run the job in the foreground and stream its log.** Rejected: it needs an HTTP request that lives for
  hours and it puts the console in charge of a container's lifetime.
- **Let the console call the compression library itself.** Rejected: it would put GPU code, model-store access
  and the dependency surface (llm-compressor, compressed-tensors, auto-round) inside the console's trust
  boundary. The wrapper keeps that surface in a container with no egress.
- **A separate job service with a queue.** Rejected for now: it is a second service to run and secure for one
  user; the wrapper plus a job directory is the same design with fewer moving parts.
