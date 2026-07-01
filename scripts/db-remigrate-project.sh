#!/usr/bin/env bash
# Ri-migrazione FRESCA dei dati per-progetto dal meta-DB (nexus, 5433) al DB
# per-progetto <slug>_nexus (cluster app, 5434). Da eseguire SUBITO PRIMA del flip
# (separazione DB per-progetto): popola il DB del progetto con lo stato AVANTI del
# meta, cosi' a flag ON la cronologia storica e' visibile.
#
# Idempotente: TRUNCATE delle tabelle dati del progetto + ricopia. Ri-eseguibile.
# Backfilla anche nexus_data_routing (session + run) nel meta.
#
# ROBUSTEZZA:
#   - Copia SOLO le colonne comuni meta ∩ progetto (intersezione), quindi tollera
#     divergenze di schema (colonne aggiunte da un lato).
#   - Bulk-load con session_replication_role='replica' (richiede superuser sul
#     cluster app) -> FK/trigger bypassati durante l'INSERT; i dati vengono dal meta
#     gia' referenzialmente coerente. Nessun problema di ordine (FK con cicli/self-ref).
#   - DRY-RUN di default: mostra i conteggi che copierebbe. Serve --apply.
#   - NON tocca lo schema: assume il DB progetto gia' provisionato (23 tab dati).
#   - Le tabelle presenti solo da un lato (es. nexus_agent_clarifications droppata dal
#     meta in 0497, langgraph_checkpoints legacy vuota) vengono saltate senza errore.
#
# Uso:
#   PGPASSWORD=nexus ./scripts/db-remigrate-project.sh <PROJECT_ID>            # dry-run
#   PGPASSWORD=nexus ./scripts/db-remigrate-project.sh <PROJECT_ID> --apply    # esegue

set -euo pipefail

PSQL="${PSQL:-/c/Program Files/PostgreSQL/17/bin/psql.exe}"

# ── Meta-DB (sorgente) ──
META_HOST="${META_HOST:-localhost}"; META_PORT="${META_PORT:-5433}"
META_USER="${META_USER:-nexus}"; META_DB="${META_DB:-nexus}"; export PGPASSWORD="${PGPASSWORD:-nexus}"

# ── Cluster app (destinazione) admin/superuser per il bulk-load ──
APP_HOST="${APP_HOST:-localhost}"; APP_PORT="${APP_PORT:-5434}"
APP_ADMIN_USER="${APP_ADMIN_USER:-nexus_admin}"; APP_ADMIN_PWD="${APP_ADMIN_PWD:-nexus_admin_secret}"

PID="${1:-}"; MODE="${2:-dry-run}"
if [ -z "$PID" ]; then echo "Uso: $0 <PROJECT_ID> [--apply]"; exit 2; fi

qmeta() { "$PSQL" -h "$META_HOST" -p "$META_PORT" -U "$META_USER" -d "$META_DB" -tAc "$1"; }
qapp()  { PGPASSWORD="$APP_ADMIN_PWD" "$PSQL" -h "$APP_HOST" -p "$APP_PORT" -U "$APP_ADMIN_USER" -d "$1" -tAc "$2"; }

# ── Risolvi il DB metadati del progetto dalla connessione nexus_metadata ──
URL="$(qmeta "SELECT convert_from(connection_secret,'UTF8') FROM project_database_config WHERE project_id='$PID' AND connection_role='nexus_metadata' ORDER BY updated_at DESC LIMIT 1")"
if [ -z "$URL" ]; then echo "ABORT: nessuna connessione connection_role='nexus_metadata' per il progetto $PID"; exit 1; fi
# postgresql://user:pwd@host:port/db
rest="${URL#postgresql://}"; DB_NAME="${rest##*/}"
echo "== Ri-migrazione fresca meta -> $DB_NAME (progetto $PID, mode: $MODE) =="

# ── Verifica provisioning (>= 20 tabelle dati) ──
ntab="$(qapp "$DB_NAME" "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' AND table_name<>'_sqlx_migrations'")"
if [ "${ntab:-0}" -lt 20 ]; then echo "ABORT: $DB_NAME ha $ntab tabelle dati (<20): non provisionato correttamente."; exit 1; fi

# ── Tabelle scopate per project_id diretto ──
PROJ_TABLES=(
  chat_sessions chat_messages agent_runs
  agent_processes jobs project_open_sessions nexus_subagent_runs
  ai_response_feedback prompt_corrections chat_message_attachments
  nexus_session_worklog nexus_session_worklog_events
  orchestrator_runs nexus_agent_plans nexus_agent_todos
)
# ── Tabelle scopate via run_id -> agent_runs.project_id ──
RUN_TABLES=(
  agent_steps nexus_agent_meta_steps nexus_agent_traces
  nexus_agent_verifier_runs nexus_graph_checkpoints orchestrator_audit_events
)

scope_clause() { # $1=tabella -> stampa la WHERE per lo scope al progetto
  local t="$1"
  for p in "${PROJ_TABLES[@]}"; do [ "$p" = "$t" ] && { echo "project_id='$PID'"; return; }; done
  for r in "${RUN_TABLES[@]}"; do [ "$r" = "$t" ] && { echo "run_id IN (SELECT id FROM agent_runs WHERE project_id='$PID')"; return; }; done
  echo ""; # sconosciuta
}

# ── DRY-RUN: conteggi ──
echo "-- conteggi righe da copiare (meta -> progetto):"
tot=0
for t in "${PROJ_TABLES[@]}" "${RUN_TABLES[@]}"; do
  exists_meta="$(qmeta "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema='public' AND table_name='$t')")"
  if [ "$exists_meta" != "t" ]; then printf "   %-32s (assente nel meta, skip)\n" "$t"; continue; fi
  c="$(qmeta "SELECT count(*) FROM $t WHERE $(scope_clause "$t")")"
  printf "   %-32s %8s\n" "$t" "$c"; tot=$((tot + c))
done
echo "   -------------------------------------------------"
printf "   %-32s %8s\n" "TOTALE" "$tot"

if [ "$MODE" != "--apply" ]; then
  echo "(dry-run: nessuna modifica. Rilancia con --apply per eseguire.)"
  exit 0
fi

# ── APPLY ──
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
towin() { command -v cygpath >/dev/null 2>&1 && cygpath -w "$1" || echo "$1"; }

echo "-- 1) DUMP dal meta (solo colonne comuni meta ∩ progetto), per tabella…"
LOAD="$WORK/load.sql"
: > "$LOAD"
echo "SET session_replication_role='replica';" >> "$LOAD"
# TRUNCATE di tutte le tabelle dati del progetto (fresh). CASCADE per le FK.
ALLT="$(printf '%s,' "${PROJ_TABLES[@]}" "${RUN_TABLES[@]}")"; ALLT="${ALLT%,}"
echo "TRUNCATE $ALLT RESTART IDENTITY CASCADE;" >> "$LOAD"

for t in "${PROJ_TABLES[@]}" "${RUN_TABLES[@]}"; do
  exists_meta="$(qmeta "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema='public' AND table_name='$t')")"
  [ "$exists_meta" = "t" ] || continue
  # colonne comuni (intersezione), nell'ordine del progetto (destinazione)
  cols_meta="$(qmeta  "SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name='$t'" | sort)"
  cols_app="$(qapp "$DB_NAME" "SELECT column_name||'|'||ordinal_position FROM information_schema.columns WHERE table_schema='public' AND table_name='$t'")"
  common="$(echo "$cols_app" | while IFS='|' read -r name ord; do echo "$cols_meta" | grep -qx "$name" && echo "$ord|$name"; done | sort -n | cut -d'|' -f2)"
  collist="$(echo "$common" | paste -sd, -)"
  [ -z "$collist" ] && { echo "   $t: nessuna colonna comune, skip"; continue; }
  dat="$WORK/$t.dat"
  "$PSQL" -h "$META_HOST" -p "$META_PORT" -U "$META_USER" -d "$META_DB" \
    -c "\copy (SELECT $collist FROM $t WHERE $(scope_clause "$t")) TO '$(towin "$dat")' (FORMAT text)"
  echo "\copy $t ($collist) FROM '$(towin "$dat")' (FORMAT text)" >> "$LOAD"
done
echo "RESET session_replication_role;" >> "$LOAD"

echo "-- 2) LOAD nel progetto (session_replication_role=replica, TRUNCATE + copy)…"
PGPASSWORD="$APP_ADMIN_PWD" "$PSQL" -h "$APP_HOST" -p "$APP_PORT" -U "$APP_ADMIN_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1 -q -f "$LOAD"

echo "-- 3) Backfill nexus_data_routing nel meta (session + run)…"
qmeta "INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id)
       SELECT 'session', id, project_id FROM chat_sessions WHERE project_id='$PID'
       ON CONFLICT DO NOTHING;" >/dev/null
qmeta "INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id)
       SELECT 'run', id, project_id FROM agent_runs WHERE project_id='$PID' AND project_id IS NOT NULL
       ON CONFLICT DO NOTHING;" >/dev/null

echo "-- 4) Verifica conteggi meta == progetto:"
mismatch=0
for t in "${PROJ_TABLES[@]}" "${RUN_TABLES[@]}"; do
  exists_meta="$(qmeta "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema='public' AND table_name='$t')")"
  [ "$exists_meta" = "t" ] || continue
  cm="$(qmeta "SELECT count(*) FROM $t WHERE $(scope_clause "$t")")"
  ca="$(qapp "$DB_NAME" "SELECT count(*) FROM $t")"
  flag="OK"; [ "$cm" != "$ca" ] && { flag="MISMATCH"; mismatch=$((mismatch+1)); }
  printf "   %-32s meta=%-6s progetto=%-6s %s\n" "$t" "$cm" "$ca" "$flag"
done
rr="$(qmeta "SELECT count(*) FROM nexus_data_routing WHERE project_id='$PID'")"
echo "   nexus_data_routing (meta) per progetto: $rr righe"
if [ "$mismatch" -gt 0 ]; then echo "ATTENZIONE: $mismatch tabelle con conteggi diversi. Verifica sopra."; exit 1; fi
echo "OK: ri-migrazione fresca completata, conteggi allineati."
