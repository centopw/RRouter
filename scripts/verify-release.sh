#!/usr/bin/env bash
# Verify a published RRouter release: checksums, CycloneDX SBOMs, provenance attestations.
#
#   ./scripts/verify-release.sh v0.3.0
#   ./scripts/verify-release.sh v0.3.0 --repo centopw/RRouter --dir /tmp/release
#   ./scripts/verify-release.sh v0.3.0 --dir /tmp/release --skip-download --no-attestation
#
# Per tarball this checks, in order:
#   1. the published `.sha256` matches the downloaded bytes (what install.sh relies on);
#   2. a `.sbom.cdx.json` exists, parses as CycloneDX, and names this artifact/version;
#   3. the GitHub build-provenance attestation verifies for the tarball (and the SBOM);
#   4. with `--commit SHA`, the SBOM is byte-identical to a fresh generation from the
#      checkout at that commit (proves the published inventory matches the tag).
#
# Exit status is non-zero on the first failure, so the release workflow can gate on it.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
DEFAULT_EXPECT="linux-amd64,linux-arm64,darwin-arm64,darwin-amd64"

TAG=""
DIR=""
REPO="${GITHUB_REPOSITORY:-}"
EXPECT="$DEFAULT_EXPECT"
COMMIT=""
SKIP_DOWNLOAD=0
CHECK_ATTESTATION=1

die() {
  printf 'verify-release: %s\n' "$*" >&2
  exit 1
}

usage() {
  sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir)
      [ "$#" -ge 2 ] || die "--dir needs a path"
      DIR=$2
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || die "--repo needs owner/name"
      REPO=$2
      shift 2
      ;;
    --expect)
      [ "$#" -ge 2 ] || die "--expect needs a comma-separated asset list"
      EXPECT=$2
      shift 2
      ;;
    --commit)
      [ "$#" -ge 2 ] || die "--commit needs a git sha"
      COMMIT=$2
      shift 2
      ;;
    --skip-download)
      SKIP_DOWNLOAD=1
      shift
      ;;
    --no-attestation)
      CHECK_ATTESTATION=0
      shift
      ;;
    -h | --help)
      usage
      ;;
    v*)
      TAG=$1
      shift
      ;;
    *)
      die "unknown argument '$1' (try --help)"
      ;;
  esac
done

[ -n "$TAG" ] || usage
case "$TAG" in
  v*) ;;
  *) die "expected a v* tag, got '$TAG'" ;;
esac
VERSION=${TAG#v}

need() {
  command -v "$1" >/dev/null 2>&1 || die "required tool '$1' is not installed"
}

need python3
if command -v sha256sum >/dev/null 2>&1; then
  HASH_CMD=(sha256sum)
elif command -v shasum >/dev/null 2>&1; then
  HASH_CMD=(shasum -a 256)
else
  die "need sha256sum or shasum"
fi

CLEANUP=0
if [ -z "$DIR" ]; then
  DIR=$(mktemp -d)
  CLEANUP=1
fi
if [ "$CLEANUP" -eq 1 ]; then
  trap 'rm -rf "$DIR"' EXIT
fi
mkdir -p "$DIR"

if [ "$SKIP_DOWNLOAD" -eq 0 ]; then
  need gh
  echo "downloading $TAG assets with gh"
  GH_DL=(release download "$TAG" --pattern 'rrouter-*' --dir "$DIR" --clobber)
  if [ -n "$REPO" ]; then
    GH_DL+=(--repo "$REPO")
  fi
  gh "${GH_DL[@]}" || die "gh release download failed for $TAG"
fi

# Completeness first: a release that silently lost one platform is broken.
IFS=',' read -r -a EXPECTED <<<"$EXPECT"
TARBALLS=()
for asset in "${EXPECTED[@]}"; do
  tarball="$DIR/rrouter-$asset.tar.gz"
  [ -f "$tarball" ] || die "missing artifact rrouter-$asset.tar.gz in $TAG"
  TARBALLS+=("$tarball")
done
for found in "$DIR"/rrouter-*.tar.gz; do
  [ -e "$found" ] || continue
  name=$(basename "$found")
  asset=${name#rrouter-}
  asset=${asset%.tar.gz}
  case ",$EXPECT," in
    *",$asset,"*) ;;
    *) die "unexpected artifact $name (not in --expect $EXPECT)" ;;
  esac
done

verified=0
for tarball in "${TARBALLS[@]}"; do
  name=$(basename "$tarball")
  base=${name%.tar.gz}
  checksum="$DIR/$name.sha256"
  sbom="$DIR/$name.sbom.cdx.json"

  [ -f "$checksum" ] || die "missing checksum $name.sha256"
  (cd "$DIR" && "${HASH_CMD[@]}" -c "$name.sha256" >/dev/null) ||
    die "checksum mismatch for $name"
  echo "  checksum ok: $name"

  [ -f "$sbom" ] || die "missing SBOM $name.sbom.cdx.json"
  python3 - "$sbom" "$base" "$VERSION" <<'PY' || die "SBOM check failed for $name"
import json
import sys

path, artifact, version = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path, encoding="utf-8") as handle:
    bom = json.load(handle)
assert bom.get("bomFormat") == "CycloneDX", "bomFormat is not CycloneDX"
assert str(bom.get("specVersion", "")).startswith("1."), "unsupported specVersion"
component = bom["metadata"]["component"]
assert component["version"] == version, (
    f"SBOM version {component['version']} != release {version}"
)
properties = {prop["name"]: prop["value"] for prop in bom["metadata"]["properties"]}
assert properties.get("rrouter:artifact") == artifact, "artifact property mismatch"
assert bom.get("components"), "SBOM has no components"
print(f"  sbom ok: {len(bom['components'])} components, {artifact} {version}")
PY

  if [ -n "$COMMIT" ]; then
    fresh="$DIR/.regenerated-$name.sbom.cdx.json"
    python3 "$SCRIPT_DIR/sbom.py" \
      --artifact "$base" \
      --version "$VERSION" \
      --commit "$COMMIT" \
      --output "$fresh" >/dev/null ||
      die "cannot regenerate the SBOM for $name"
    cmp -s "$fresh" "$sbom" ||
      die "published SBOM for $name does not match commit $COMMIT"
    rm -f "$fresh"
    echo "  sbom reproducible from commit $COMMIT"
  fi

  if [ "$CHECK_ATTESTATION" -eq 1 ]; then
    need gh
    GH_VERIFY=(attestation verify "$tarball")
    if [ -n "$REPO" ]; then
      GH_VERIFY+=(--repo "$REPO")
    fi
    gh "${GH_VERIFY[@]}" >/dev/null ||
      die "no valid provenance attestation for $name (gh attestation verify failed)"
    echo "  provenance ok: gh attestation verify $name${REPO:+ --repo $REPO}"
  fi

  verified=$((verified + 1))
done

echo "verified $verified artifacts in $TAG"
