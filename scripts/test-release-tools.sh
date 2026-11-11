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
python3 -c 'import ast, sys
for path in sys.argv[1:]:
    ast.parse(open(path, encoding="utf-8").read())' \
  scripts/sbom.py scripts/release.py scripts/release-notes.py \
  || fail "a release script has a syntax error"
pass "bash and Python release tools parse"

python3 - <<'PY' || exit 1
import importlib.util
spec = importlib.util.spec_from_file_location("release_tool", "scripts/release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
# Patch numbers are ordinary integers, not a two-digit field.
assert release.bump_version("0.9.121", "patch") == "0.9.122"
assert release.bump_version("0.9.121", "minor") == "0.10.0"
assert release.bump_version("0.9.121", "major") == "1.0.0"
assert release.normalize("v0.9.121") == "0.9.121"
PY
pass "SemVer bump handles patch numbers above 100"

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

step "release metadata: one version source"
RELROOT="$TMP/relroot"
mkdir -p "$RELROOT/scripts" "$RELROOT/deploy/helm/rrouter" "$RELROOT/docs/deploy"
cp scripts/release.py scripts/release-notes.py "$RELROOT/scripts/"
cat >"$RELROOT/Cargo.toml" <<'EOF'
[workspace]
members = ["a"]

[workspace.package]
version = "0.9.0"
edition = "2021"
EOF
cat >"$RELROOT/Cargo.lock" <<'EOF'
[[package]]
name = "external"
version = "0.9.0"

[[package]]
name = "rrouter-auth"
version = "0.9.0"
EOF
cat >"$RELROOT/CHANGELOG.md" <<'EOF'
# Changelog

## 0.9.0 — 2026-01-01

### Changed
- Old.
EOF
cat >"$RELROOT/README.md" <<'EOF'
RROUTER_VERSION=v0.9.0
`install.sh` downloads a published release; `v0.9.0` is the newest.
EOF
cat >"$RELROOT/deploy/helm/rrouter/Chart.yaml" <<'EOF'
apiVersion: v2
name: rrouter
version: 0.1.4
appVersion: "0.9.0"
EOF
cat >"$RELROOT/docs/ROADMAP.md" <<'EOF'
**0.9.0** is the current release, and `CHANGELOG.md` → `0.9.0` lists changes.
EOF
cat >"$RELROOT/docs/deploy/helm.md" <<'EOF'
helm upgrade rrouter --set image.tag=0.9.0
EOF
git -C "$RELROOT" init -q
git -C "$RELROOT" config user.email test@example.test
git -C "$RELROOT" config user.name Test
git -C "$RELROOT" add -A
git -C "$RELROOT" commit -qm "chore: initial"
git -C "$RELROOT" tag -a v0.9.0 -m v0.9.0
printf 'a\n' >"$RELROOT/a"
git -C "$RELROOT" add a
git -C "$RELROOT" commit -qm "fix(ui): make a key field selectable"
printf 'b\n' >>"$RELROOT/a"
git -C "$RELROOT" add a
git -C "$RELROOT" commit -qm "feat(api): add provider-qualified ids"

python3 scripts/release.py --root "$RELROOT" >/dev/null \
  || fail "auto release from a feat commit failed"
grep -q 'version = "0.10.0"' "$RELROOT/Cargo.toml" || fail "Cargo.toml not bumped"
grep -A1 'name = "rrouter-auth"' "$RELROOT/Cargo.lock" | grep -q '0.10.0' \
  || fail "Cargo.lock workspace member not bumped"
grep -q 'version = "0.9.0"' "$RELROOT/Cargo.lock" || fail "external lock row was rewritten"
grep -q 'appVersion: "0.10.0"' "$RELROOT/deploy/helm/rrouter/Chart.yaml" \
  || fail "Helm appVersion not bumped"
grep -q 'v0.10.0' "$RELROOT/README.md" || fail "README pin not bumped"
grep -q '### Features' "$RELROOT/CHANGELOG.md" || fail "feat group missing from notes"
grep -q '### Bug Fixes' "$RELROOT/CHANGELOG.md" || fail "fix group missing from notes"
python3 scripts/release.py --root "$RELROOT" --check >/dev/null \
  || fail "consistent release metadata was rejected"
if python3 scripts/release.py 0.10.1 --root "$RELROOT" >/dev/null 2>&1; then
  fail "a patch version was accepted for feat commits"
fi
if python3 scripts/release.py --bump patch --root "$RELROOT" >/dev/null 2>&1; then
  fail "an explicit patch bump was accepted for feat commits"
fi

git -C "$RELROOT" add -A
git -C "$RELROOT" commit -qm "chore(release): v0.10.0"
git -C "$RELROOT" tag -a v0.10.0 -m v0.10.0
printf 'c\n' >>"$RELROOT/a"
git -C "$RELROOT" add a
git -C "$RELROOT" commit -qm "refactor(core)!: remove the legacy path"
python3 scripts/release.py --root "$RELROOT" >/dev/null \
  || fail "auto release from a breaking commit failed"
grep -q 'version = "1.0.0"' "$RELROOT/Cargo.toml" || fail "breaking commit did not bump major"
grep -q '### Breaking Changes' "$RELROOT/CHANGELOG.md" || fail "breaking group missing"

python3 - "$RELROOT/deploy/helm/rrouter/Chart.yaml" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
path.write_text(path.read_text(encoding="utf-8").replace('1.0.0', '0.9.9'), encoding="utf-8")
PY
if python3 scripts/release.py 1.0.0 --root "$RELROOT" --check >/dev/null 2>&1; then
  fail "drifted Helm appVersion passed the check"
fi
pass "semver derived from commits, lockfile scoping, notes generation, drift detection"

printf '\nrelease tooling: all checks passed\n'
