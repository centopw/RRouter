#!/usr/bin/env sh
# Back up the RRouter data directory: a consistent SQLite snapshot plus the master key file.
#
#   ./scripts/backup.sh [output-dir]           # default ./backups/<timestamp>
#   RROUTER_DATA_DIR=/var/lib/rrouter ./scripts/backup.sh /var/backups/rrouter
#
# `VACUUM INTO` writes a consistent copy while the gateway is running; copying rrouter.db
# directly while WAL mode is active can miss recent pages. The master key is included so the
# snapshot is restorable — treat the output as a secret, and keep it off cheap storage.
set -eu

DATA="${RROUTER_DATA_DIR:-./data}"
OUT="${1:-./backups/$(date -u +%Y%m%dT%H%M%SZ)}"
DB="$DATA/rrouter.db"
KEY="$DATA/master.key"

if [ ! -f "$DB" ]; then
  echo "backup: no database at $DB (set RROUTER_DATA_DIR)" >&2
  exit 1
fi
if ! command -v sqlite3 >/dev/null 2>&1; then
  echo "backup: sqlite3 is required for a consistent snapshot" >&2
  exit 1
fi

mkdir -p "$OUT"
sqlite3 "$DB" "VACUUM INTO '$OUT/rrouter.db'"
if [ -f "$KEY" ]; then
  cp "$KEY" "$OUT/master.key"
  chmod 600 "$OUT/master.key"
fi
( cd "$OUT" && shasum -a 256 rrouter.db master.key 2>/dev/null > SHA256SUMS || sha256sum rrouter.db master.key > SHA256SUMS )

echo "backup written to $OUT"
echo "restore: stop rrouter, copy rrouter.db and master.key back into \$RROUTER_DATA_DIR, start rrouter"
echo "note: if the deployment uses RROUTER_MASTER_KEY instead of the key file, the key is not in the snapshot."
