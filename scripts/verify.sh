#!/usr/bin/env bash
# Build RRouter from scratch and run every gate the release depends on.
#
#   ./scripts/verify.sh                 # full gate: fmt, lints, tests, release, UI, smokes
#   ./scripts/verify.sh --quick         # skip the release build and the live smokes
#   ./scripts/verify.sh --no-ui         # skip the dashboard (only with a known-good ui/dist)
#   ./scripts/verify.sh --serve         # full gate, then leave an instance on :20128
#
# Every step prints PASS/FAIL; the script stops at the first failure and exits non-zero,
# so it is safe to run as a single CI/pre-merge gate. A missing tool is a failure, not a
# skip — the only way to opt out of a component is an explicit flag.
set -euo pipefail
cd "$(dirname "$0")/.."

QUICK=0
NO_UI=0
SERVE=0
for arg in "$@"; do
  case "$arg" in
    --quick) QUICK=1 ;;
    --no-ui) NO_UI=1 ;;
    --serve) SERVE=1 ;;
    -h | --help)
      sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "verify: unknown flag '$arg' (try --help)" >&2
      exit 2
      ;;
  esac
done

BIN=target/release/rrouter
STEPS_RUN=0

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }
pass() {
  printf '  \033[32mPASS\033[0m %s\n' "$1"
  STEPS_RUN=$((STEPS_RUN + 1))
}
fail() {
  printf '  \033[31mFAIL\033[0m %s\n' "$1" >&2
  exit 1
}

# Run one command, failing the whole gate with its name on a non-zero exit.
run() {
  local label=$1
  shift
  echo "  \$ $*"
  "$@" || fail "$label"
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "required tool '$1' is not installed"
}

need cargo
need python3

step "1/8 formatting"
run "rustfmt check" cargo fmt --all --check
pass "cargo fmt --all --check"

step "2/8 lints (workspace, all targets, all features)"
run "clippy" cargo clippy --workspace --all-targets --all-features -- -D warnings
pass "clippy -D warnings"

step "3/8 test suite"
run "cargo test" cargo test --workspace --locked
pass "workspace tests"

if [ "$QUICK" -eq 1 ]; then
  step "4/8 release build (skipped: --quick)"
  if [ ! -x "$BIN" ]; then
    fail "--quick needs an existing $BIN"
  fi
else
  step "4/8 release build"
  run "release build" cargo build --release --locked
  pass "binary $(du -h "$BIN" | cut -f1)"
fi

if [ "$NO_UI" -eq 1 ]; then
  step "5/8 dashboard (skipped: --no-ui)"
  printf '  \033[33mSKIP\033[0m using the committed ui/dist\n'
else
  step "5/8 dashboard (typecheck + embedded build)"
  need npm
  if [ ! -d ui/node_modules ]; then
    run "npm ci" npm --prefix ui ci --no-audit --no-fund
  fi
  run "tsc typecheck" npm --prefix ui run typecheck
  run "vite build" npm --prefix ui run build
  pass "tsc + vite build"
fi

step "6/8 config hardening (the binary must refuse)"
TMP_DATA=$(mktemp -d)
trap 'rm -rf "$TMP_DATA"' EXIT
out=$(RROUTER_DATA_DIR="$TMP_DATA" RROUTER_CORS_ORIGINS='*' "$BIN" 2>&1 || true)
grep -q "not allowed" <<<"$out" || fail "wildcard CORS was not rejected: ${out:0:200}"
out=$(RROUTER_DATA_DIR="$TMP_DATA" RROUTER_HOST=0.0.0.0 "$BIN" 2>&1 || true)
grep -q "RROUTER_ALLOW_REMOTE_ADMIN" <<<"$out" ||
  fail "non-loopback bind without opt-in was not refused: ${out:0:200}"
pass "wildcard CORS and unguarded remote bind refused"

if [ "$QUICK" -eq 1 ]; then
  step "7/8 live smokes (skipped: --quick)"
  step "8/8 backup/restore drill (skipped: --quick)"
else
  step "7/8 live end-to-end (mock upstream + real binary)"
  run "e2e smoke" python3 scripts/e2e_smoke.py
  run "oauth smoke" python3 scripts/oauth_smoke.py
  pass "e2e + oauth flows"

  step "8/8 backup/restore drill"
  run "restore drill" python3 scripts/backup_restore_smoke.py
  pass "snapshot restores credentials and settings"
fi

printf '\n\033[1m== done\033[0m\n'
printf '  %d gates passed. Run ./scripts/verify.sh --serve to keep an instance on :20128.\n' "$STEPS_RUN"

if [ "$SERVE" -eq 1 ]; then
  echo
  echo "Starting on http://127.0.0.1:20128 — Ctrl-C to stop."
  exec "$BIN"
fi
