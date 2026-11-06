#!/usr/bin/env bash
# Build RRouter from scratch and verify it end to end.
#
#   ./scripts/verify.sh              # build + all gates
#   ./scripts/verify.sh --serve      # also leave a live instance on :20128
#
# Every step prints PASS/FAIL; the script exits non-zero on the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }
pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; }

step "1/6 release build"
cargo build --release --locked 2>&1 | tail -1
pass "binary $(du -h target/release/rrouter | cut -f1)"

step "2/6 test suite"
cargo test --workspace --quiet 2>&1 | grep -E "^test result" | awk -F'[ ;]' '{s+=$4; f+=$6} END {printf "  %d passed, %d failed\n", s, f; exit f>0}'
pass "all crates"

step "3/6 lints"
cargo clippy --workspace --all-targets --quiet -- -D warnings
pass "clippy -D warnings"

step "4/6 config hardening (must refuse)"
out=$(RROUTER_CORS_ORIGINS='*' ./target/release/rrouter 2>&1 || true)
grep -q "not allowed" <<<"$out" && pass "wildcard CORS rejected"
out=$(RROUTER_HOST=0.0.0.0 ./target/release/rrouter 2>&1 || true)
grep -q "RROUTER_ALLOW_REMOTE_ADMIN" <<<"$out" && pass "non-loopback bind without opt-in refused"

step "5/6 end-to-end (mock upstream + real binary)"
python3 scripts/e2e_smoke.py | tail -3

step "6/6 done"
echo "  Verified. Run './scripts/verify.sh --serve' to keep an instance on :20128."

if [[ "${1:-}" == "--serve" ]]; then
  echo
  echo "Starting on http://127.0.0.1:20128 — Ctrl-C to stop."
  exec ./target/release/rrouter
fi
