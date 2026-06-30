#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

mkdir -p db/backups

ts="$(date +%Y%m%d-%H%M%S)"
out="db/backups/nexus-${ts}.sql.gz"

# Local dev compose uses POSTGRES_USER=nexus / POSTGRES_PASSWORD=nexus
# NB: niente -t (il TTY corrompe il pipe binario verso gzip -> dump vuoto).
docker exec ideai-postgres-nexus-1 bash -lc 'PGPASSWORD=nexus pg_dump -U nexus -d nexus' | gzip > "$out"

gzip -l "$out" >/dev/null
echo "$out"

