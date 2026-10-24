# Release notes

Notes for the most recent release only; history lives in CHANGELOG.md.

## 0.2.0 — 2026-10-17

Nemo Console 0.2.0 adds NVIDIA SkillEvaluator as a fourth managed system and, with it, live agent
evaluation: a console page that scores agent skills (`SKILL.md` folders) for quality, security and
duplication, including Tier 3 "Skill Lift" runs (an agent runs the skill's eval cases with and
without the skill in a sandboxed runner container; the difference is the lift).

- **SkillEvaluator**: tiers `static` / `tier1` (LLM judge) / `tier1+2` (embedding dedup), every run
  kept as JSON + Markdown in the state dir (newest 200), `se.*` metrics and `skilleval.run` events.
- **Tier 3 live evaluation** (one at a time, background, up to ~1 h): `skillevaluator-local tier3`
  through the lab wrapper only — plain `skillevaluator` is refused because its stock sandboxes have
  open network. Verified live 2026-10-17 (+0.240 lift, 2/2 attempts scored); the HTTP-API and UI
  paths of Tier 3 were mock-tested only.
- **Dynamo** lifecycle now uses the stack's own compose files (never `scripts/use-model.sh`), with a
  preflight that names what shares the GPU.
- Known limitations: Relay's config validator is a proxy (stated in the UI); trust/policy files are
  read-only by design; the topology diagram's Mermaid sources in the docs are unvalidated.
