#!/usr/bin/env sh
# RRouter installer.
#
#   curl -fsSL https://raw.githubusercontent.com/centopw/RRouter/main/install.sh | sh
#
# Environment overrides:
#   RROUTER_VERSION      tag to install (default: latest)
#   RROUTER_INSTALL_DIR  destination directory (default: /usr/local/bin, else ~/.local/bin)
#   RROUTER_REPO         owner/repo to download from (default: centopw/RRouter)
#   RROUTER_BASE_URL     release base URL (default: https://github.com/<repo>/releases)
#   RROUTER_DRY_RUN=1    resolve and print, install nothing
#
# Every failure exits non-zero with a one-line reason.

set -eu

REPO="${RROUTER_REPO:-centopw/RRouter}"
VERSION="${RROUTER_VERSION:-latest}"
INSTALL_DIR="${RROUTER_INSTALL_DIR:-}"
DRY_RUN="${RROUTER_DRY_RUN:-0}"
BASE_URL="${RROUTER_BASE_URL:-https://github.com/$REPO/releases}"

say() { printf '%s\n' "$*"; }
warn() { printf 'rrouter: %s\n' "$*" >&2; }
die() { warn "$*"; exit 1; }

for tool in curl tar uname; do
  command -v "$tool" >/dev/null 2>&1 || die "required command not found: $tool"
done

os=$(uname -s)
case "$os" in
  Linux) os=linux ;;
  Darwin) os=darwin ;;
  *) die "unsupported OS '$os': use Docker or build from source" ;;
esac

arch=$(uname -m)
case "$arch" in
  x86_64 | amd64) arch=amd64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) die "unsupported architecture '$arch': build from source" ;;
esac

asset="rrouter-$os-$arch.tar.gz"

if [ "$VERSION" = latest ]; then
  tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1) || true
  [ -n "${tag:-}" ] || die "no published release for $REPO yet"
else
  tag="$VERSION"
fi

url="$BASE_URL/download/$tag/$asset"
say "rrouter $tag ($os/$arch)"

if [ "$DRY_RUN" = "1" ]; then
  say "would download $url"
  say "would verify  $url.sha256"
  say "would install to ${INSTALL_DIR:-<auto>}"
  exit 0
fi

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t rrouter)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

curl -fsSL "$url" -o "$tmp/$asset" || die "download failed: $url"
curl -fsSL "$url.sha256" -o "$tmp/$asset.sha256" || die "checksum download failed: $url.sha256"

# The checksum file names the release asset, so verification works from $tmp.
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp" && sha256sum -c "$asset.sha256" >/dev/null) || die "checksum mismatch for $asset"
elif command -v shasum >/dev/null 2>&1; then
  (cd "$tmp" && shasum -a 256 -c "$asset.sha256" >/dev/null) || die "checksum mismatch for $asset"
else
  die "no sha256 tool found (need sha256sum or shasum)"
fi

tar -xzf "$tmp/$asset" -C "$tmp" || die "cannot extract $asset"
bin="$tmp/${asset%.tar.gz}/rrouter"
[ -f "$bin" ] || die "archive did not contain a rrouter binary"

if [ -n "$INSTALL_DIR" ]; then
  dir="$INSTALL_DIR"
elif [ "$(id -u)" = "0" ]; then
  dir=/usr/local/bin
elif [ -w /usr/local/bin ] 2>/dev/null; then
  dir=/usr/local/bin
else
  dir="$HOME/.local/bin"
fi

mkdir -p "$dir" || die "cannot create $dir"
dest="$dir/rrouter"

if command -v install >/dev/null 2>&1; then
  install -m 0755 "$bin" "$dest"
else
  cp "$bin" "$dest" && chmod 0755 "$dest"
fi

say "installed $dest"

if "$dest" --version >/dev/null 2>&1; then
  say "version: $("$dest" --version)"
fi

case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    say ""
    say "note: $dir is not on PATH. Add it with:"
    say "  export PATH=\"$dir:\$PATH\""
    ;;
esac

say ""
say "next: RROUTER_PORT=20128 $dest   # dashboard at http://127.0.0.1:20128"
