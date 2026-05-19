#!/usr/bin/env bash
# Start the Postgres dependency, wait until it's accepting connections,
# then run the gif-server. Pass extra args straight through to cargo run.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Starting Postgres (docker compose db)..."
docker compose up -d db

echo "Waiting for Postgres to become healthy..."
for _ in $(seq 1 30); do
  status="$(docker compose ps --format '{{.Health}}' db 2>/dev/null || true)"
  if [ "$status" = "healthy" ]; then
    echo "Postgres is healthy."
    break
  fi
  sleep 2
done

if [ "${status:-}" != "healthy" ]; then
  echo "Postgres did not become healthy in time. Check: docker compose logs db" >&2
  exit 1
fi

echo "Starting gif-server..."
exec cargo run "$@"
