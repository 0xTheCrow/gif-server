#!/usr/bin/env bash
# Manual backup of the GIF storage (host bind mount) and the Postgres database.
# Run on the server: ./deploy/backup.sh
#
# Writes one timestamped folder under BACKUP_DIR containing:
#   gif-storage.tgz     — tar of the GIF files
#   gifserver-db.sql.gz — pg_dump of the database
# It never deletes anything; old backups are left for you to prune.
#
# Overridable via the environment (or edit the defaults below):
#   COMPOSE_DIR        compose project dir (holds docker-compose.yml and .env)
#   STORAGE_HOST_PATH  host dir bind-mounted to the container's storage
#   BACKUP_DIR         where the timestamped backup folder is written
set -euo pipefail

COMPOSE_DIR="${COMPOSE_DIR:-/srv/gif-server}"
BACKUP_DIR="${BACKUP_DIR:-/srv/gif-backups}"

# Pull STORAGE_HOST_PATH / POSTGRES_PASSWORD from the deployed .env if present.
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

stamp="$(date -u +%Y%m%d-%H%M%S)"
dest="$BACKUP_DIR/$stamp"
mkdir -p "$dest"

echo "backing up GIF files from $STORAGE_DIR ..."
tar czf "$dest/gif-storage.tgz" -C "$STORAGE_DIR" .

echo "exporting database $PG_DB (read-only copy via pg_dump) ..."
cd "$COMPOSE_DIR"
docker compose exec -T -e PGPASSWORD="${POSTGRES_PASSWORD:-}" db \
  pg_dump -U "$PG_USER" "$PG_DB" | gzip >"$dest/gifserver-db.sql.gz"

echo "backup complete: $dest"
ls -lh "$dest"
