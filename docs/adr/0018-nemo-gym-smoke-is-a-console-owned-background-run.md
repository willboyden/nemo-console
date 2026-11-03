# ADR-0018: a NeMo Gym smoke run is a background run the console owns; containment is part of health

Status: Accepted (2026-11-03, addendum 10.10)

## Context

NVIDIA NeMo Gym is a framework for reinforcement-learning environments: resource servers that verify an answer and
return a reward, agents that roll tasks out against a model, and the collection of rollouts that a trainer (NeMo RL)
consumes. The lab installs it as a contained client (`clients/nemo-gym/`, the `gym-local` wrapper, NeMo Gym 0.7.0
pinned to a scanned commit). Every Gym command runs in a client container on ONE internal network,
`ai-lab-nemo-gym`, whose only way out is a router broker that holds the wrapper's own router key; the client never
holds a key. `gym env start` always starts a local Ray cluster inside that container.

The wrapper's `smoke` subcommand is the one useful thing to drive from a console: one real run of the `mcqa`
environment through the router (servers + Ray + N rollouts, a few minutes), printing the rollout count, the rewards
and a final `RESULT:` line. Unlike Model Optimizer's wrapper (ADR-0017), it runs in the FOREGROUND and leaves no
machine-readable record outside its Docker volume (which holds prompts and completions).

## Decision

1. **The console owns the run, like an AIPerf load run.** The `smoke` action starts the wrapper in a background task
   and returns at once; one run at a time (a second start is refused, never queued). The task writes the console's
   own `<state>/gym/runs/<run-id>/run.json` (0600): `running` at the start, then the verdict, the duration and the
   numbers parsed from the wrapper's output (`rollouts`, `rewarded`, `mean_reward`, `pass_at_1` when the aggregate
   prints it, the reward list). A run a restart left at `running` reads as `interrupted`; a console stop writes
   `interrupted` itself.
2. **Only numbers are kept.** The rollouts (prompts and completions) stay in the wrapper's volume. The console
   stores no model text; a failed run keeps the wrapper's `RESULT:` line, redacted and capped.
3. **Containment is part of health.** Status comes from `gym-local status` (read-only Docker queries, cached
   15 s because the overview polls). A client network that exists but is not internal makes the system **down**
   with `gym.not_contained`, and the smoke action refuses to start. A network that does not exist yet is not a
   problem: the wrapper's first command creates it from the compose file, internal. A stopped broker is the normal
   resting state (it starts on demand).
4. **A smoke run is disruptive.** It spends router tokens on a shared engine and starts containers; `verify` (the
   wrapper's read-only network check) and `report` are safe. The model alias is validated and the task count is
   capped at 20 (the wrapper's own cap is 50).
5. **No editable file.** The Gym environment configuration belongs to the lab wrapper; the console has no Configure
   tab for it. No playground.

## Consequences

- The console needs `gym-local` on its PATH (the runner runs bare allowlisted names only). Until the lab links it
  (the wrapper must then resolve its own symlink to find its compose file), the page shows a clean **down** state
  with that fix, which is also what a clean clone and CI see.
- Training (NeMo RL consuming the rollouts) is out of scope: there is nothing to start or read for it yet.
- The parsing follows the wrapper's printed format; if the wrapper changes it, the numbers go absent (never
  invented) and the verdict still follows the `RESULT:` line and the exit code.

## Alternatives rejected

- **Let the wrapper background itself and write a run.json (ADR-0017's shape).** Cleaner, but it needs a change to
  the lab wrapper; the console can own the run today with the AIPerf pattern it already has. If the wrapper gains
  that record, the adapter can switch to reading it.
- **Read the rollouts from the wrapper's Docker volume.** It would put prompts and completions in the console's
  reach; the numbers the wrapper prints are what the page needs.
- **Treat a stopped broker as degraded.** It would show a healthy idle install as a problem.
