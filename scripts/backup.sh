#!/usr/bin/env sh
# Back up the RRouter data directory: consistent SQLite snapshots plus every other file
# the process owns (master key, dashboard JWT key, settings, extension assets).
#
#   ./scripts/backup.sh [output-dir]           # default ./backups/<timestamp>
#   RROUTER_DATA_DIR=/var/lib/rrouter ./scripts/backup.sh /var/backups/rrouter
#
# Why the whole directory: `rrouter.db` holds connections (encrypted), keys and history,
# `master.key` decrypts the connections, `settings.db` holds branding/Token Saver/update
# options, and `jwt.key` signs dashboard sessions. A snapshot missing any of them is not
# restorable — the earlier two-file version silently lost settings and sessions.
#
# Live SQLite files are copied with `VACUUM INTO`, not `cp`: under WAL a plain copy can
# miss the newest pages. Everything else is copied with its mode. The output directory is
# a secret (it contains `master.key`); keep it off cheap storage and out of git.
#
# Output: every file, plus `MANIFEST` (metadata + sizes + hashes) and `SHA256SUMS`
# (`shasum -a 256 -c` / `sha256sum -c` compatible). The script verifies its own output
# before exiting, so a truncated backup fails here instead of at restore time.
set -eu

DATA="${RROUTER_DATA_DIR:-./data}"
OUT="${1:-./backups/$(date -u +%Y%m%dT%H%M%SZ)}"
DB="$DATA/rrouter.db"
SETTINGS="$DATA/settings.db"

die() {
  echo "backup: $*" >&2
  exit 1
}

[ -f "$DB" ] || die "no database at $DB (set RROUTER_DATA_DIR)"
command -v sqlite3 >/dev/null 2>&1 ||
  die "sqlite3 is required for a consistent snapshot"

mkdir -p "$OUT" || die "cannot create $OUT"
OUT_ABS=$(cd "$OUT" && pwd)
DATA_ABS=$(cd "$DATA" && pwd)
# A snapshot inside the data directory would be copied by the next run and served from
# the same volume it is meant to survive; refuse it outright.
case "$OUT_ABS/" in
  "$DATA_ABS/"*)
    rmdir "$OUT_ABS" 2>/dev/null || true
    die "output directory must be outside the data directory ($DATA_ABS)"
    ;;
esac
# Validate before touching the mode: a pre-existing, non-empty directory is someone
# else's (e.g. `backup.sh /var/backups`) and must not be chmod-ed on the way to an error.
if [ -n "$(ls -A "$OUT_ABS" 2>/dev/null)" ]; then
  die "$OUT_ABS is not empty; choose a new directory so one snapshot cannot mix with another"
fi
# The snapshot is a secret before a single file lands in it: 0700 means a concurrent
# local user cannot list the names inside, and nothing readable exists before this point.
chmod 700 "$OUT" || die "cannot restrict $OUT"

# Hash command: GNU coreutils first, then the macOS/BSD spelling.
if command -v sha256sum >/dev/null 2>&1; then
  HASH_CMD=sha256sum
elif command -v shasum >/dev/null 2>&1; then
  HASH_CMD="shasum -a 256"
else
  die "need sha256sum or shasum to checksum the backup"
fi

hash_of() {
  # shellcheck disable=SC2086 # HASH_CMD carries its own flags on purpose.
  $HASH_CMD "$1" | awk '{print $1}'
}

# --- 1. Live databases: consistent copies ------------------------------------------
# `.timeout` waits for a writer instead of failing SQLITE_BUSY on a busy gateway.
snapshot_db() {
  src=$1
  dst=$2
  sqlite3 -cmd ".timeout 5000" "$src" "VACUUM INTO '$dst'" ||
    die "cannot snapshot $src"
  # VACUUM INTO creates the file with the ordinary umask (0644 on most systems). A
  # backup of encrypted credentials and hashed keys must not be world-readable.
  chmod 600 "$dst" || die "cannot restrict $dst"
}

snapshot_db "$DB" "$OUT_ABS/rrouter.db"
if [ -f "$SETTINGS" ]; then
  snapshot_db "$SETTINGS" "$OUT_ABS/settings.db"
fi

# --- 2. Everything else, verbatim --------------------------------------------------
# The live DB files are replaced by the snapshots above; WAL/SHM belong to the live
# connection, and a staged `*.part` download is a half-written update.
find "$DATA_ABS" -path "$OUT_ABS" -prune -o -type f \
  ! -name rrouter.db ! -name rrouter.db-wal ! -name rrouter.db-shm \
  ! -name settings.db ! -name settings.db-wal ! -name settings.db-shm \
  ! -name '*.part' -print | while IFS= read -r file; do
  rel=${file#"$DATA_ABS"/}
  case "$rel" in
    MANIFEST | SHA256SUMS) continue ;;
  esac
  dir=$(dirname "$rel")
  [ "$dir" = "." ] || mkdir -p "$OUT_ABS/$dir"
  cp -p "$file" "$OUT_ABS/$rel" || die "cannot copy $file"
done

# Every path in a snapshot is secret: directories 0700, files 0600, whatever the umask
# was. Run after the copies (a crash must not leave a readable half-snapshot) and again
# after MANIFEST/SHA256SUMS are written.
harden_snapshot() {
  chmod 700 "$OUT_ABS" || die "cannot restrict $OUT_ABS"
  find "$OUT_ABS" -type d -exec chmod 700 {} + || die "cannot restrict directories"
  find "$OUT_ABS" -type f -exec chmod 600 {} + || die "cannot restrict files"
}
harden_snapshot

# --- 3. Manifest and checksums -----------------------------------------------------
# The file list is every regular file in the snapshot except the two metadata files.
: >"$OUT_ABS/.filelist"
find "$OUT_ABS" -type f ! -name SHA256SUMS ! -name MANIFEST ! -name .filelist -print |
  while IFS= read -r file; do
    printf '%s\n' "${file#"$OUT_ABS"/}"
  done | sort >"$OUT_ABS/.filelist"

(
  cd "$OUT_ABS"
  while IFS= read -r rel; do
    # shellcheck disable=SC2086
    $HASH_CMD "$rel"
  done <.filelist >SHA256SUMS
) || die "cannot checksum the snapshot"

{
  echo "# RRouter backup manifest"
  echo "# created_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "# source_dir=$DATA_ABS"
  echo "# checksum=$HASH_CMD"
  echo "# file<TAB>bytes<TAB>sha256"
  (
    cd "$OUT_ABS"
    while IFS= read -r rel; do
      bytes=$(wc -c <"$rel" | tr -d ' ')
      printf '%s\t%s\t%s\n' "$rel" "$bytes" "$(hash_of "$rel")"
    done <.filelist
  )
} >"$OUT_ABS/MANIFEST" || die "cannot write MANIFEST"
rm -f "$OUT_ABS/.filelist"

# --- 4. Self-check ----------------------------------------------------------------
[ "$(wc -l <"$OUT_ABS/SHA256SUMS" | tr -d ' ')" -ge 1 ] ||
  die "SHA256SUMS is empty"
(
  cd "$OUT_ABS"
  # shellcheck disable=SC2086
  $HASH_CMD -c SHA256SUMS >/dev/null
) || die "snapshot failed checksum verification"

# MANIFEST and SHA256SUMS were written after the first pass; a checksum file that lists
# the secrets must not itself be world-readable, and the directory mode is re-asserted
# in case anything in between widened it.
harden_snapshot

# --- 5. Report --------------------------------------------------------------------
echo "backup written to $OUT_ABS"
awk -F'\t' '!/^#/ {printf "  %8s  %s\n", $2, $1}' "$OUT_ABS/MANIFEST"
for expected in rrouter.db master.key settings.db jwt.key; do
  [ -f "$OUT_ABS/$expected" ] ||
    echo "  WARN $expected absent from the data dir; manifest records what existed"
done
if [ -z "${RROUTER_MASTER_KEY:-}" ] && [ ! -f "$OUT_ABS/master.key" ]; then
  echo "  WARN no master.key in the snapshot and RROUTER_MASTER_KEY is unset:" >&2
  echo "       encrypted provider credentials will not be restorable" >&2
fi
echo "verify: ( cd $OUT_ABS && $HASH_CMD -c SHA256SUMS )"
echo "restore: stop rrouter, copy the snapshot contents back into \$RROUTER_DATA_DIR, start rrouter"
echo "note: if the deployment uses RROUTER_MASTER_KEY instead of the key file, the key is not in the snapshot."
