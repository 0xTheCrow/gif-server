#!/usr/bin/env bash
# Manual backup of the GIF storage (host bind mount) and the Postgres database.
# Run on the server: ./deploy/backup.sh
#
# Writes one timestamped folder under BACKUP_DIR containing:
#   gif-storage.tgz     — tar of the GIF files
#   gifserver-db.sql.gz — pg_dump of the database
# It never deletes anything; old backups are left for you to prune.
#
# Reaches Postgres with `docker compose exec`, which runs pg_dump inside the
# ALREADY-RUNNING db container. It never creates, recreates, restarts or stops a
# container, so it cannot disturb the running service.
#
# Overridable via the environment (or edit the defaults below):
#   COMPOSE_DIR        compose project dir (holds docker-compose.yml and .env)
#   STORAGE_HOST_PATH  host dir bind-mounted to the container's storage
#   BACKUP_DIR         where the timestamped backup folder is written
set -euo pipefail

COMPOSE_DIR="${COMPOSE_DIR:-/srv/gif-server}"
BACKUP_DIR="${BACKUP_DIR:-/srv/gif-backups}"

# Pull STORAGE_HOST_PATH / POSTGRES_* from the deployed .env if present.
set -a
# shellcheck disable=SC1091
[ -f "$COMPOSE_DIR/.env" ] && . "$COMPOSE_DIR/.env"
set +a

STORAGE_DIR="${STORAGE_HOST_PATH:-/srv/gif-data}"
PG_USER="${POSTGRES_USER:-gifserver}"
PG_DB="${POSTGRES_DB:-gifserver}"

if [ ! -d "$STORAGE_DIR" ]; then
  echo "backup: storage dir $STORAGE_DIR does not exist — aborting" >&2
  exit 1
fi

cd "$COMPOSE_DIR"

# exec needs the db container already up; do NOT start it (that would mutate
# container state). Fail with a clear message instead.
if [ "$(docker compose ps --status running --services 2>/dev/null | grep -cx db)" -eq 0 ]; then
  echo "backup: the db container is not running — start it, then re-run:" >&2
  echo "    cd $COMPOSE_DIR && docker compose up -d db" >&2
  exit 1
fi

stamp="$(date -u +%Y%m%d-%H%M%S)"
dest="$BACKUP_DIR/$stamp"
mkdir -p "$dest"

echo "backing up GIF files from $STORAGE_DIR ..."
tar czf "$dest/gif-storage.tgz" -C "$STORAGE_DIR" .

echo "exporting database $PG_DB (read-only copy via pg_dump) ..."
docker compose exec -T -e PGPASSWORD="${POSTGRES_PASSWORD:-}" db \
  pg_dump -U "$PG_USER" "$PG_DB" | gzip >"$dest/gifserver-db.sql.gz"

echo "verifying archives ..."
gzip -t "$dest/gif-storage.tgz"
gzip -t "$dest/gifserver-db.sql.gz"

# Directory entries in a tar listing end in "/"; drop them or every healthy
# backup looks short next to `find -type f`.
tar tzf "$dest/gif-storage.tgz" | grep -v '/$' | sed 's|^\./||' | sort >"$dest/.in-tar"
find "$STORAGE_DIR" -type f -printf '%P\n' | sort >"$dest/.on-disk"
if diff -q "$dest/.in-tar" "$dest/.on-disk" >/dev/null; then
  echo "  GIF files: $(wc -l <"$dest/.in-tar") file(s), archive matches $STORAGE_DIR"
else
  echo "backup: WARNING — archive does not match $STORAGE_DIR:" >&2
  diff "$dest/.in-tar" "$dest/.on-disk" >&2 || true
  echo "backup: a GIF uploaded while tar was running can cause this; re-run to confirm." >&2
fi
rm -f "$dest/.in-tar" "$dest/.on-disk"

# The marker is not the final line (pg_dump follows it with "--", a blank line,
# and on newer versions a \unrestrict line), so scan the tail.
if ! gunzip -c "$dest/gifserver-db.sql.gz" | tail -20 | grep -q 'PostgreSQL database dump complete'; then
  echo "backup: ERROR — db dump is truncated (no completion marker) — do not trust it" >&2
  exit 1
fi
echo "  database: dump complete marker present"

echo "backup complete: $dest"
ls -lh "$dest"
