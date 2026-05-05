#!/usr/bin/env bash
# Clona i database da server-remoto () al Docker locale.
# Usare solo in LAN — non richiede internet.
# Uso: ./scripts/clone-db-from-server-remoto.sh [--yes]

set -euo pipefail

# ── Config ─────────────────────────────────────────────────────────────────────
DEV101_HOST="${REMOTE_DB_HOST:-}"
DEV101_USER="administrator"
SSH_KEY="$HOME/.ssh/id_ed25519"
SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=no -o ConnectTimeout=5 -o BatchMode=yes"

# "nome_db:container_locale:porta_locale:utente_locale:porta_remota:esiste_su_server-remoto"
declare -a DBS=(
  "ai_orchestrator:ideai-postgres-1:5432:postgres:5432:yes"
  "nexus:ideai-postgres-nexus-1:5433:nexus:5432:yes"
)

# Credenziali remote — admin è il superuser su server-remoto
PGUSER_REMOTE="${PGUSER_REMOTE:-admin}"
PGPASSWORD_REMOTE="${PGPASSWORD_REMOTE:-N3tm3d42dc2}"
PGPASSWORD_REMOTE_ALT="${PGPASSWORD_REMOTE_ALT:-ZAQ!xsw2}"

AUTO_YES=false
[[ "${1:-}" == "--yes" ]] && AUTO_YES=true

# ── Helpers ────────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERR]${NC}   $*" >&2; }

confirm() {
  if $AUTO_YES; then return 0; fi
  echo -e "${YELLOW}[?]${NC} $* [y/N] "
  read -r ans
  [[ "${ans,,}" == "y" ]]
}

ssh_exec() {
  # shellcheck disable=SC2086
  ssh $SSH_OPTS "${DEV101_USER}@${DEV101_HOST}" "$@"
}

row_count() {
  local container="$1" user="$2" db="$3"
  docker exec -i "$container" \
    psql -U "$user" -d "$db" -tAq \
    -c "SELECT sum(reltuples::bigint) FROM pg_class JOIN pg_namespace ON pg_namespace.oid = relnamespace WHERE relkind='r' AND nspname NOT IN ('pg_catalog','information_schema');" \
    2>/dev/null || echo "?"
}

remote_dump() {
  local db="$1" port="$2"
  # Usa formato plain (-Fp) + psql per evitare incompatibilità di versione pg_restore
  # pg_dump gira su server-remoto via SSH — postgres ascolta solo su localhost remoto
  if ssh_exec "PGPASSWORD='$PGPASSWORD_REMOTE' pg_dump -h localhost -p $port -U '$PGUSER_REMOTE' -d '$db' --no-acl --no-owner"; then
    return 0
  fi
  warn "[$db] prima password fallita — tento password alternativa"
  if ssh_exec "PGPASSWORD='$PGPASSWORD_REMOTE_ALT' pg_dump -h localhost -p $port -U '$PGUSER_REMOTE' -d '$db' --no-acl --no-owner"; then
    return 0
  fi
  error "[$db] Dump fallito. user=$PGUSER_REMOTE  port=$port"
  return 1
}

# ── Controllo connettività LAN ─────────────────────────────────────────────────
info "Verifico connettività SSH verso server-remoto ($DEV101_HOST)…"
if ! ssh_exec "echo ok" &>/dev/null; then
  error "Impossibile raggiungere server-remoto. Sei connesso alla LAN?"
  exit 1
fi
info "server-remoto raggiungibile."

# ── Verifica Docker locale ─────────────────────────────────────────────────────
for entry in "${DBS[@]}"; do
  IFS=: read -r _db container _lport _luser _rport _exists <<< "$entry"
  if ! docker inspect "$container" &>/dev/null; then
    error "Container locale '$container' non trovato. Avvia i servizi con docker compose up -d."
    exit 1
  fi
done

# ── Conferma ──────────────────────────────────────────────────────────────────
echo ""
warn "ATTENZIONE: questa operazione SOVRASCRIVE i database locali con quelli di server-remoto."
for entry in "${DBS[@]}"; do
  IFS=: read -r db_name container local_port _luser remote_port exists <<< "$entry"
  if [[ "$exists" == "yes" ]]; then
    echo "  $db_name  →  $container (:$local_port)  ←  server-remoto:$remote_port"
  else
    echo "  $db_name  →  SALTATO (non presente su server-remoto)"
  fi
done
echo ""
confirm "Procedere?" || { info "Annullato."; exit 0; }

# ── Clonazione ────────────────────────────────────────────────────────────────
ERRORS=0

for entry in "${DBS[@]}"; do
  IFS=: read -r db_name container _local_port local_user remote_port exists <<< "$entry"

  if [[ "$exists" != "yes" ]]; then
    warn "[$db_name] non presente su server-remoto — saltato."
    echo ""
    continue
  fi

  info "[$db_name] Dump in corso…"
  rows_before=$(row_count "$container" "$local_user" "$db_name")

  docker exec -i "$container" \
    psql -U "$local_user" -d postgres -c \
    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='$db_name' AND pid <> pg_backend_pid();" \
    &>/dev/null || true

  docker exec -i "$container" \
    psql -U "$local_user" -d postgres -c \
    "DROP DATABASE IF EXISTS $db_name;" &>/dev/null

  docker exec -i "$container" \
    psql -U "$local_user" -d postgres -c \
    "CREATE DATABASE $db_name OWNER $local_user;" &>/dev/null

  # Dump plain SQL → psql (evita incompatibilità pg_restore tra PG17 e PG16)
  if remote_dump "$db_name" "$remote_port" \
    | docker exec -i "$container" \
        psql -U "$local_user" -d "$db_name" -q 2>&1; then

    rows_after=$(row_count "$container" "$local_user" "$db_name")
    info "[$db_name] OK — righe prima: ${rows_before}, dopo: ${rows_after}"
  else
    error "[$db_name] FALLITO."
    ERRORS=$((ERRORS + 1))
  fi

  echo ""
done

# ── Risultato ─────────────────────────────────────────────────────────────────
if [[ $ERRORS -eq 0 ]]; then
  info "Clonazione completata senza errori."
else
  error "Clonazione completata con $ERRORS errori. Controlla l'output sopra."
  exit 1
fi
