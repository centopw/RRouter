#!/bin/sh
# Block until the CI workflow has finished on <sha>, and fail unless it succeeded.
# release.yml uses this so a tag only publishes a commit that already passed CI, and so the
# image CI built for that commit can be re-tagged instead of rebuilt.
#
#   GH_TOKEN=... scripts/wait_for_ci.sh <sha> [timeout-seconds]
set -eu

sha="${1:?usage: wait_for_ci.sh <sha> [timeout-seconds]}"
timeout="${2:-1800}"
deadline=$(( $(date +%s) + timeout ))

while :; do
  # Newest push run of ci.yml for exactly this commit (a tag and its main push race, so
  # the run may not exist for the first few polls).
  row="$(gh run list --workflow ci.yml --event push --commit "$sha" --limit 1 \
    --json status,conclusion,url --jq '.[0] | "\(.status) \(.conclusion) \(.url)"' 2>/dev/null || true)"
  case "$row" in
    "completed success "*) echo "CI passed: ${row#completed success }"; exit 0 ;;
    "completed "*) echo "CI did not pass on $sha: $row" >&2; exit 1 ;;
  esac
  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "timed out after ${timeout}s waiting for CI on $sha (last state: ${row:-no run found})" >&2
    exit 1
  fi
  echo "waiting for CI on $sha (${row:-no run yet})"
  sleep 15
done
