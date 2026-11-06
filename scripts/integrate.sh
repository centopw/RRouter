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
# Accept both `T2 T3` and `integrate.sh T2 T3` forms; array slicing of "$@" was
# splitting the whole target string into single characters.
if [ "$#" -gt 0 ]; then TASKS=("$@"); else TASKS=(T2 T3 T4 T5 T6); fi
cd "$REPO"
if [ -n "$(git status --porcelain | grep -v '^?? target/')" ]; then
  echo "main worktree is dirty; commit or stash first" >&2
  exit 1
fi

for t in "${TASKS[@]}"; do
  crate=${MAP[$t]}
  src="$WS/rr-$t-${crate#rrouter-}/crates/$crate"
  [ -d "$src" ] || { echo "skip $t: no $src" >&2; continue; }
  # Sync ONLY this crate: an unquoted/array-expanded target could otherwise
  # rewrite sibling crates with stale stubs from the worker's branch.
  # Every top-level entry of the crate except Cargo.toml and Rust sources, so
  # embedded assets (include_str!) and any future fixture dirs come along too.
  rsync -a --delete --exclude 'Cargo.toml' \
        --exclude 'src/***' --exclude 'tests/***' \
        "$src/" "$REPO/crates/$crate/"
  rsync -a --delete "$src/src/" "$REPO/crates/$crate/src/"
  if [ -d "$src/tests" ]; then
    mkdir -p "$REPO/crates/$crate/tests"
    rsync -a --delete "$src/tests/" "$REPO/crates/$crate/tests/"
  fi
  cp "$src/Cargo.toml" "$REPO/crates/$crate/Cargo.toml"
  echo "merged $t -> crates/$crate"
done
