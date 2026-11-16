#!/usr/bin/env bash
# Reclaim development build output without touching source or the committed dashboard.
#
#   ./scripts/clean.sh           # print size, then cargo clean
#   ./scripts/clean.sh --dry-run # print size only
#   ./scripts/clean.sh --ui      # also remove ui/node_modules
#
# `target/` grows for every profile/target combination and is safe to delete; a clean
# costs a rebuild, not source. `ui/dist` is tracked, so this script deliberately leaves it.
set -euo pipefail
cd "$(dirname "$0")/.."

DRY_RUN=0
CLEAN_UI=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --ui) CLEAN_UI=1 ;;
    -h | --help)
      sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "clean: unknown flag '$arg' (try --help)" >&2
      exit 2
      ;;
  esac
done

size_of() {
  if [ -e "$1" ]; then du -sh "$1" 2>/dev/null | awk '{print $1}'; else printf 'absent'; fi
}

echo "Rust target:       $(size_of target)"
if [ -d target ]; then
  du -sh target/* 2>/dev/null | sort -h | tail -5 || true
fi
echo "UI node_modules:   $(size_of ui/node_modules)"
echo "UI dist (tracked): $(size_of ui/dist)"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "clean: dry run; nothing removed"
  exit 0
fi

if [ -d target ]; then
  cargo clean
else
  echo "clean: target/ is already absent"
fi

if [ "$CLEAN_UI" -eq 1 ] && [ -d ui/node_modules ]; then
  rm -rf ui/node_modules
  echo "clean: removed ui/node_modules"
fi

echo "clean: done; next build will repopulate target/"
