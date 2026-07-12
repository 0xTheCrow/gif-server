#!/usr/bin/env bash
# Restore a backup produced by ./deploy/backup.sh (GIF storage + Postgres).
# Run on the server: ./deploy/restore.sh <timestamp|latest|path>
#
#   ./deploy/restore.sh 20260711-134501   # a folder under BACKUP_DIR
#   ./deploy/restore.sh latest            # most recent backup
#   ./deploy/restore.sh /path/to/backup   # an explicit folder
#
# Restores BOTH halves from the SAME folder: the DB references GIF files by
# hash, so mixing a dump from one run with files from another leaves rows
# pointing at files that do not exist.
#
# Reaches Postgres with `docker compose exec`, which runs psql inside the
# ALREADY-RUNNING db container. It never creates, recreates, restarts or stops a
# container — stopping and starting the app is left to you. It refuses to run
# while anything is still connected to the database.
#
# Destructive to CURRENT state: the database is dropped and recreated. The
# current GIF storage dir is NOT deleted — it is moved aside to
# <storage>.pre-restore-<stamp>. Remove it yourself once you have verified.
#
# Overridable via the environment (same knobs as backup.sh):
#   COMPOSE_DIR        compose project dir (holds docker-compose.yml and .env)
#   STORAGE_HOST_PATH  host dir bind-mounted to the container's storage
#   BACKUP_DIR         where backup.sh wrote its timestamped folders
#   FORCE=1            skip the confirmation prompt
set -euo pipefail

COMPOSE_DIR="${COMPOSE_DIR:-/srv/gif-server}"
BACKUP_DIR="${BACKUP_DIR:-/srv/gif-backups}"

set -a
# shellcheck disable=SC1091
[ -f "$COMPOSE_DIR/.env" ] && . "$COMPOSE_DIR/.env"
set +a

STORAGE_DIR="${STORAGE_HOST_PATH:-/srv/gif-data}"
PG_USER="${POSTGRES_USER:-gifserver}"
PG_DB="${POSTGRES_DB:-gifserver}"

if [ $# -lt 1 ]; then
  echo "usage: $0 <timestamp|latest|path>" >&2
  exit 2
fi

case "$1" in
  # Only timestamp-shaped dirs: a lexical sort over *everything* would let a
  # stray name (BROKEN, old, tmp) sort last and hijack "latest".
  latest) src="$(ls -1d "$BACKUP_DIR"/[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-[0-9][0-9][0-9][0-9][0-9][0-9]/ 2>/dev/null | sort | tail -1 || true)" ;;
  */*)    src="$1" ;;
  *)      src="$BACKUP_DIR/$1" ;;
esac
src="${src%/}"

if [ -z "$src" ] || [ ! -d "$src" ]; then
  echo "restore: no such backup folder: ${src:-<none found>}" >&2
  exit 1
fi

tarball="$src/gif-storage.tgz"
dump="$src/gifserver-db.sql.gz"

# Validate before destroying anything: a restore that finds a corrupt archive
# halfway through is worse than one that never started.
for f in "$tarball" "$dump"; do
  [ -f "$f" ] || { echo "restore: missing $f" >&2; exit 1; }
done
echo "verifying archives ..."
gzip -t "$tarball" || { echo "restore: $tarball is corrupt — aborting" >&2; exit 1; }
gzip -t "$dump"    || { echo "restore: $dump is corrupt — aborting" >&2; exit 1; }
if ! gunzip -c "$dump" | tail -20 | grep -q 'PostgreSQL database dump complete'; then
  echo "restore: $dump is truncated (no completion marker) — aborting" >&2
  exit 1
fi

cd "$COMPOSE_DIR"

if [ "$(docker compose ps --status running --services 2>/dev/null | grep -cx db)" -eq 0 ]; then
  echo "restore: the db container is not running — start it, then re-run:" >&2
  echo "    cd $COMPOSE_DIR && docker compose up -d db" >&2
  exit 1
fi

psql_db() {
  docker compose exec -T -e PGPASSWORD="${POSTGRES_PASSWORD:-}" db \
    psql -U "$PG_USER" -v ON_ERROR_STOP=1 "$@"
}

# Active connections mean the app is still up: it would write mid-restore and
# reconnect straight after the DROP. Refuse rather than race it.
conns="$(psql_db -d postgres -tAc \
  "SELECT count(*) FROM pg_stat_activity WHERE datname = '$PG_DB';" | tr -d '[:space:]')"
if [ "${conns:-0}" -gt 0 ]; then
  cat >&2 <<EOF
restore: $conns active connection(s) to "$PG_DB" — the app is still running.
Stop it first, then re-run:

    cd $COMPOSE_DIR && docker compose stop app

EOF
  exit 1
fi

cat <<EOF

About to restore from: $src
  GIF storage -> $STORAGE_DIR   (current dir moved aside, not deleted)
  database    -> $PG_DB   (DROPPED and recreated — current data is LOST)

EOF
if [ "${FORCE:-}" != "1" ]; then
  printf 'Type "restore" to continue: '
  read -r reply
  [ "$reply" = "restore" ] || { echo "aborted."; exit 1; }
fi

stamp="$(date -u +%Y%m%d-%H%M%S)"

# Move aside rather than extract over: a plain extract merges, leaving files
# that exist now but were not in the backup.
if [ -d "$STORAGE_DIR" ]; then
  aside="$STORAGE_DIR.pre-restore-$stamp"
  echo "moving current storage aside -> $aside"
  mv "$STORAGE_DIR" "$aside"
fi
echo "restoring GIF files to $STORAGE_DIR ..."
mkdir -p "$STORAGE_DIR"
tar xzf "$tarball" -C "$STORAGE_DIR"

# backup.sh writes a plain pg_dump (no --clean), so it must be replayed into an
# empty database; replaying over an existing schema errors on every CREATE.
echo "dropping and recreating database $PG_DB ..."
psql_db -d postgres -c "DROP DATABASE IF EXISTS $PG_DB;"
psql_db -d postgres -c "CREATE DATABASE $PG_DB OWNER $PG_USER;"

echo "replaying dump into $PG_DB ..."
gunzip -c "$dump" | psql_db -d "$PG_DB" >/dev/null

echo
echo "restore complete from $src"
[ -n "${aside:-}" ] && echo "previous GIF storage preserved at: $aside"
cat <<EOF
start the app again:

    cd $COMPOSE_DIR && docker compose up -d app

then verify, and remove the preserved dir yourself.
EOF
