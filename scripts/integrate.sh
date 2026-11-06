#!/usr/bin/env bash
# Merge worker worktrees into main by crate directory. Run from anywhere.
# Usage: scripts/integrate.sh [T2 T3 T4 T5 T6]   (default: all)
set -euo pipefail

REPO=/Users/cento/git/personal/RRouter
WS=/Users/cento/orca/workspaces/RRouter
declare -A MAP=(
  [T2]=rrouter-translate
  [T3]=rrouter-auth
  [T4]=rrouter-store
  [T5]=rrouter-router
  [T6]=rrouter-gateway
)
TASKS=("${@:-T2 T3 T4 T5 T6}")
cd "$REPO"
git status --porcelain | grep -v '^?? target/' | head -5 &&
  { echo "main worktree is dirty; commit or stash first" >&2; exit 1; } || true

for t in ${TASKS[@]}; do
  crate=${MAP[$t]}
  src="$WS/rr-$t-${crate#rrouter-}"
  [ -d "$src/crates/$crate" ] || { echo "skip $t: no $src/crates/$crate" >&2; continue; }
  rsync -a --delete "$src/crates/$crate/src/" "$REPO/crates/$crate/src/"
  [ -d "$src/crates/$crate/tests" ] && rsync -a "$src/crates/$crate/tests/" "$REPO/crates/$crate/tests/"
  cp "$src/crates/$crate/Cargo.toml" "$REPO/crates/$crate/Cargo.toml"
  echo "merged $t -> crates/$crate"
done
