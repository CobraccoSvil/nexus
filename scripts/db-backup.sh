#!/usr/bin/env bash
# Backup logico del DB Nexus (Postgres) per ambiente dev/WSL.
# - Usa pg_dump (custom format) per restore affidabile.
# - Non stampa segreti in output.
#
# Uso:
#   ./scripts/db-backup.sh
#   BACKUP_DIR=./backups/postgres KEEP_LAST=40 ./scripts/db-backup.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()    { echo -e "${CYAN}[db]${NC} $*"; }
success() { echo -e "${GREEN}[ok]${NC}  $*"; }
warn()    { echo -e "${YELLOW}[warn]${NC} $*"; }
error()   { echo -e "${RED}[err]${NC} $*" >&2; }

# Config DB (override via env). Default coerente con docker-compose.local.yml.
PGHOST="${PGHOST:-127.0.0.1}"
PGPORT="${PGPORT:-5433}"
PGUSER="${PGUSER:-nexus}"
PGDATABASE="${PGDATABASE:-nexus}"
# In dev il default è 'nexus'. Se cambi la password, esporta PGPASSWORD prima di eseguire lo script.
PGPASSWORD="${PGPASSWORD:-nexus}"
PG_CONTAINER="${PG_CONTAINER:-ideai-postgres-nexus-1}"

BACKUP_DIR="${BACKUP_DIR:-$REPO_ROOT/backups/postgres}"
KEEP_LAST="${KEEP_LAST:-40}"
mkdir -p "$BACKUP_DIR"

ts="$(date +%Y%m%d_%H%M%S)"
out="$BACKUP_DIR/nexus_${ts}.dump"

info "Eseguo backup DB (pg_dump custom)…"

# pg_dump locale può non combaciare con la versione del server (container usa PG17).
# Preferiamo pg_dump dentro il container Postgres.
if docker inspect "$PG_CONTAINER" >/dev/null 2>&1; then
  if ! docker exec -i "$PG_CONTAINER" \
    pg_dump -U "$PGUSER" -d "$PGDATABASE" \
    --format=custom --no-owner --no-acl \
    > "$out" 2>/dev/null; then
    rm -f "$out" || true
    error "Backup fallito."
    exit 1
  fi
else
  export PGPASSWORD
  if ! pg_dump \
    --host "$PGHOST" \
    --port "$PGPORT" \
    --username "$PGUSER" \
    --dbname "$PGDATABASE" \
    --format=custom \
    --no-owner \
    --no-acl \
    --file "$out" \
    >/dev/null 2>&1; then
    rm -f "$out" || true
    error "Backup fallito."
    exit 1
  fi
fi

size_bytes="$(wc -c < "$out" 2>/dev/null || echo 0)"
if [ "${size_bytes:-0}" -lt 1024 ]; then
  warn "Dump creato ma sembra troppo piccolo (${size_bytes} bytes). Controlla la connettività DB."
else
  success "Backup creato: $out (${size_bytes} bytes)"
fi

# ── Retention ──
if [[ "$KEEP_LAST" =~ ^[0-9]+$ ]] && [ "$KEEP_LAST" -gt 0 ]; then
  mapfile -t dumps < <(ls -1t "$BACKUP_DIR"/nexus_*.dump 2>/dev/null || true)
  if [ "${#dumps[@]}" -gt "$KEEP_LAST" ]; then
    warn "Retention: tengo ultimi $KEEP_LAST dump, rimuovo ${#dumps[@]}→$KEEP_LAST…"
    for ((i=KEEP_LAST; i<${#dumps[@]}; i++)); do
      rm -f "${dumps[$i]}" || true
    done
  fi
fi

exit 0

