#!/usr/bin/env bash
# Ship gate for the browser end-to-end suite (frontend/e2e/run.mjs).
#
#   scripts/e2e-gate.sh run           run the suite; on a pass with a clean backend/ and frontend/, record it
#   scripts/e2e-gate.sh check <rev>   exit 0 only if the suite passed on exactly <rev>'s backend/ and frontend/
#
# The record is the pair of git tree ids of backend/ and frontend/, kept inside .git (never committed). A docs-only
# commit after a pass needs no rerun; any change to the code does. The pre-push hook (.githooks/pre-push, enabled by
# `make hooks`) calls `check` for every commit or tag being pushed. CI runs the suite again on every push.
# No `set -e`: every failure below is handled and reported on purpose.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
STAMP="$(git rev-parse --git-path nemo-console-e2e.pass)" || { echo "e2e-gate: not a git checkout" >&2; exit 2; }

# "<backend tree> <frontend tree>" of a commit (an annotated tag is peeled to its commit).
trees() {
  local b f
  b=$(git rev-parse --verify --quiet "$1^{commit}:backend") || return 1
  f=$(git rev-parse --verify --quiet "$1^{commit}:frontend") || return 1
  echo "$b $f"
}

case "${1:-}" in
  run)
    # A Chrome that is not on PATH can be named once per checkout: git config nemo-console.chrome /path/to/chrome
    if [ -z "${CHROME_BIN:-}" ] && chrome=$(git config --get nemo-console.chrome); then export CHROME_BIN="$chrome"; fi
    node frontend/e2e/run.mjs || exit 1
    # The suite tested the working tree, so a pass only counts for a commit when backend/ and frontend/ match it.
    if [ -n "$(git status --porcelain -- backend frontend)" ]; then
      echo "e2e-gate: passed, but backend/ or frontend/ has uncommitted changes, so no pass was recorded." >&2
      echo "e2e-gate: commit, then run 'make e2e' again before pushing." >&2
      exit 0
    fi
    want=$(trees HEAD) || { echo "e2e-gate: cannot read HEAD's trees" >&2; exit 1; }
    echo "$want" > "$STAMP" || exit 1
    echo "e2e-gate: pass recorded for $(git rev-parse --short HEAD)"
    ;;
  check)
    rev="${2:-}"
    [ -n "$rev" ] || { echo "usage: scripts/e2e-gate.sh check <rev>" >&2; exit 2; }
    want=$(trees "$rev") || { echo "e2e-gate: $rev has no backend/ and frontend/ trees" >&2; exit 1; }
    [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$want" ]
    ;;
  *)
    echo "usage: scripts/e2e-gate.sh run | check <rev>" >&2
    exit 2
    ;;
esac
