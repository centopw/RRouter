#!/usr/bin/env bash
# Local checks for the release tooling. No tag, no network, no GitHub token.
#
#   ./scripts/test-release-tools.sh
#
# It exercises the parts that can be wrong without a real release: the SBOM generator's
# determinism and shape, and `verify-release.sh`'s offline checks (checksums, SBOM,
# reproducibility) plus the failure modes a release must never pass.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

step() { printf '\n== %s\n' "$1"; }
pass() { printf '  PASS %s\n' "$1"; }
fail() {
  printf '  FAIL %s\n' "$1" >&2
  exit 1
}

step "syntax"
bash -n scripts/verify-release.sh || fail "verify-release.sh has a syntax error"
python3 -c 'import ast, sys; ast.parse(open(sys.argv[1], encoding="utf-8").read())' \
  scripts/sbom.py || fail "sbom.py has a syntax error"
pass "verify-release.sh parses (bash -n); sbom.py parses (ast)"

step "SBOM determinism and shape"
python3 scripts/sbom.py --artifact rrouter-linux-amd64 --version 0.0.0-test \
  --commit deadbeef --output "$TMP/a.json" >/dev/null
python3 scripts/sbom.py --artifact rrouter-linux-amd64 --version 0.0.0-test \
  --commit deadbeef --output "$TMP/b.json" >/dev/null
cmp -s "$TMP/a.json" "$TMP/b.json" || fail "SBOM output is not byte-identical across runs"
python3 - "$TMP/a.json" <<'PY' || fail "SBOM does not have the expected CycloneDX shape"
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    bom = json.load(handle)
assert bom["bomFormat"] == "CycloneDX", "bomFormat"
assert bom["specVersion"] == "1.5", "specVersion"
assert bom["serialNumber"].startswith("urn:uuid:"), "serialNumber"
assert bom["metadata"]["component"]["version"] == "0.0.0-test", "root version"
assert bom["components"], "components"
print(f"       {len(bom['components'])} components, {bom['serialNumber']}")
PY
pass "deterministic CycloneDX 1.5, artifact/version recorded"

step "offline release verification (checksums, SBOM, reproducibility)"
REL="$TMP/release"
SRC="$TMP/src"
mkdir -p "$REL" "$SRC/rrouter-linux-amd64"
printf 'binary\n' >"$SRC/rrouter-linux-amd64/rrouter"
tar -C "$SRC" -czf "$REL/rrouter-linux-amd64.tar.gz" rrouter-linux-amd64
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$REL" && sha256sum rrouter-linux-amd64.tar.gz > rrouter-linux-amd64.tar.gz.sha256)
else
  (cd "$REL" && shasum -a 256 rrouter-linux-amd64.tar.gz > rrouter-linux-amd64.tar.gz.sha256)
fi
python3 scripts/sbom.py --artifact rrouter-linux-amd64 --version 0.0.0-test \
  --commit deadbeef --output "$REL/rrouter-linux-amd64.tar.gz.sbom.cdx.json" >/dev/null

verify() {
  bash scripts/verify-release.sh v0.0.0-test --dir "$REL" --skip-download \
    --no-attestation --expect linux-amd64 "$@"
}
verify --commit deadbeef >/dev/null || fail "a clean release directory did not verify"
pass "checksum, SBOM shape and SBOM reproducibility verified"

step "failure modes are refused"
cp "$REL/rrouter-linux-amd64.tar.gz" "$TMP/original.tar.gz"
printf 'tamper\n' >>"$REL/rrouter-linux-amd64.tar.gz"
if verify >/dev/null 2>&1; then fail "a tampered tarball passed verification"; fi
pass "tampered tarball refused"
cp "$TMP/original.tar.gz" "$REL/rrouter-linux-amd64.tar.gz"

python3 - "$REL/rrouter-linux-amd64.tar.gz.sbom.cdx.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    bom = json.load(handle)
bom["metadata"]["component"]["version"] = "9.9.9"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(bom, handle)
PY
if verify >/dev/null 2>&1; then fail "an SBOM naming the wrong version passed"; fi
pass "SBOM with the wrong release version refused"

python3 scripts/sbom.py --artifact rrouter-linux-amd64 --version 0.0.0-test \
  --commit deadbeef --output "$REL/rrouter-linux-amd64.tar.gz.sbom.cdx.json" >/dev/null
if verify --commit 0000000 >/dev/null 2>&1; then fail "an SBOM from another commit passed"; fi
pass "SBOM not reproducible from the claimed commit refused"

rm -f "$REL/rrouter-linux-amd64.tar.gz.sbom.cdx.json"
if verify >/dev/null 2>&1; then fail "a release without an SBOM passed"; fi
pass "missing SBOM refused"

printf '\nrelease tooling: all checks passed\n'
