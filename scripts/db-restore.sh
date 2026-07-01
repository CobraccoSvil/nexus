#!/usr/bin/env bash
# Ripristino DB Nexus (Postgres) da dump creato con scripts/db-backup.sh (custom format).
# - Stoppa lo stack, ripristina DB, poi riavvia.
# - Non stampa segreti in output.
#
# Uso:
#   ./scripts/db-restore.sh ./backups/postgres/nexus_YYYYmmdd_HHMMSS.dump
#   ./scripts/db-restore.sh latest
#
# Nota: in dev questo resetta il DB "nexus" (drop/create).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()    { echo -e "${CYAN}[db]${NC} $*"; }
success() { echo -e "${GREEN}[ok]${NC}  $*"; }
warn()    { echo -e "${YELLOW}[warn]${NC} $*"; }
error()   { echo -e "${RED}[err]${NC} $*" >&2; }

arg="${1:-}"
if [ -z "$arg" ]; then
  error "Uso: ./scripts/db-restore.sh <file.dump|latest>"
  exit 1
fi

BACKUP_DIR="${BACKUP_DIR:-$REPO_ROOT/backups/postgres}"

dump_path="$arg"
if [ "$arg" = "latest" ]; then
  dump_path="$(ls -1t "$BACKUP_DIR"/nexus_*.dump 2>/dev/null | head -n 1 || true)"
fi

if [ -z "${dump_path:-}" ] || [ ! -f "$dump_path" ]; then
  error "Dump non trovato: $dump_path"
  exit 1
fi

if ! command -v pg_restore >/dev/null 2>&1; then
  error "pg_restore non trovato nel PATH."
  exit 1
fi

# Config DB (override via env). Default coerente con docker-compose.local.yml.
PGHOST="${PGHOST:-127.0.0.1}"
PGPORT="${PGPORT:-5433}"
PGUSER="${PGUSER:-nexus}"
PGDATABASE="${PGDATABASE:-nexus}"
PGPASSWORD="${PGPASSWORD:-nexus}"
export PGPASSWORD
PG_CONTAINER="${PG_CONTAINER:-ideai-postgres-nexus-1}"

info "Nota: ferma i servizi che usano il DB prima del restore (Windows: deploy/deploy-local.ps1)."

info "Avvio Postgres container…"
docker compose -f docker-compose.local.yml up -d postgres-nexus >/dev/null

# Attesa readiness
info "Attesa Postgres…"
for i in $(seq 1 30); do
  if psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d postgres -tAq -c "SELECT 1;" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

if ! psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d postgres -tAq -c "SELECT 1;" >/dev/null 2>&1; then
  error "Postgres non raggiungibile su ${PGHOST}:${PGPORT}."
  exit 1
fi

info "Drop & create database 'nexus'…"
psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d postgres -v ON_ERROR_STOP=1 -q <<'SQL'
SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='nexus' AND pid <> pg_backend_pid();
DROP DATABASE IF EXISTS nexus;
CREATE DATABASE nexus OWNER nexus;
SQL

info "Ripristino dump…"
if docker inspect "$PG_CONTAINER" >/dev/null 2>&1; then
  # pg_restore dentro container (versione compatibile con server).
  if ! docker exec -i "$PG_CONTAINER" \
    pg_restore -U "$PGUSER" -d "$PGDATABASE" --no-owner --no-privileges \
    < "$dump_path" >/dev/null; then
    error "Restore fallito."
    exit 1
  fi
else
  if ! pg_restore \
    --host "$PGHOST" \
    --port "$PGPORT" \
    --username "$PGUSER" \
    --dbname "$PGDATABASE" \
    --no-owner \
    --no-privileges \
    "$dump_path" \
    >/dev/null; then
    error "Restore fallito."
    exit 1
  fi
fi

success "Restore completato."

info "Restore terminato: riavvia i servizi (Windows: deploy/deploy-local.ps1)."

success "Fatto."
exit 0

