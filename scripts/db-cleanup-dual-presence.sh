#!/usr/bin/env bash
# Cleanup dual-presence POST-FLIP (separazione DB per-progetto).
#
# Dopo il flip (settings db.project_separation.enabled = 'true') i dati chat/run
# di un progetto vivono nel suo DB (<slug>_nexus). Le COPIE pre-flip nel meta-DB
# diventano ridondanti: questo script le rimuove PER PROGETTO, recuperando spazio
# e chiudendo la ridondanza. NON tocca lo schema (le tabelle restano, globali):
# svuota solo le righe del progetto indicato.
#
# SICUREZZA:
#   - Rifiuta di girare se il flag di separazione NON e' 'true' (cancellare prima
#     del flip = perdita dati: l'app legge ancora dal meta).
#   - DRY-RUN di default: mostra quante righe cancellerebbe. Serve --apply.
#   - Transazione unica: un errore (es. FK) fa ROLLBACK, niente stato parziale.
#   - PRE-REQUISITO manuale: verifica prima che il progetto abbia i dati nel suo
#     DB (row count in <slug>_nexus >= meta). Lo script NON lo verifica al posto tuo.
#
# Uso:
#   PGPASSWORD=nexus ./scripts/db-cleanup-dual-presence.sh <PROJECT_ID>            # dry-run
#   PGPASSWORD=nexus ./scripts/db-cleanup-dual-presence.sh <PROJECT_ID> --apply   # esegue

set -euo pipefail

PSQL="${PSQL:-/c/Program Files/PostgreSQL/17/bin/psql.exe}"
PGHOST="${PGHOST:-localhost}"; PGPORT="${PGPORT:-5433}"; PGUSER="${PGUSER:-nexus}"; PGDATABASE="${PGDATABASE:-nexus}"
export PGPASSWORD="${PGPASSWORD:-nexus}"

PID="${1:-}"; MODE="${2:-dry-run}"
if [ -z "$PID" ]; then echo "Uso: $0 <PROJECT_ID> [--apply]"; exit 2; fi

q() { "$PSQL" -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$PGDATABASE" -tAc "$1"; }

# ── Guard: il flip deve essere avvenuto ──
flag="$(q "SELECT value FROM settings WHERE key='db.project_separation.enabled'" || echo '')"
if [ "$flag" != "true" ]; then
  echo "ABORT: db.project_separation.enabled='$flag' (atteso 'true'). Cancellare le copie"
  echo "       meta PRIMA del flip cancellerebbe i dati LIVE. Esegui solo dopo il flip."
  exit 1
fi

# Tabelle scopate per project_id (dirette), in ordine FK-safe (figli -> padri).
DIRECT_ORDER=(
  nexus_agent_plans nexus_agent_todos
  prompt_corrections ai_response_feedback
  nexus_session_worklog_events nexus_session_worklog
  chat_message_attachments
  nexus_subagent_runs orchestrator_runs jobs agent_processes project_open_sessions
  agent_runs
  chat_messages
  chat_sessions
)
# Tabelle scopate via run_id -> agent_runs.project_id (figli del run, prima di agent_runs).
RUN_JOIN=(
  agent_steps nexus_agent_meta_steps nexus_agent_traces
  nexus_agent_verifier_runs nexus_graph_checkpoints orchestrator_audit_events
)

echo "== Cleanup dual-presence meta-DB per progetto $PID (mode: $MODE) =="
echo "-- conteggi righe che verrebbero rimosse dal meta-DB:"
tot=0
for t in "${RUN_JOIN[@]}"; do
  c="$(q "SELECT count(*) FROM $t WHERE run_id IN (SELECT id FROM agent_runs WHERE project_id='$PID')" || echo 0)"
  printf "   %-32s %8s (via run_id)\n" "$t" "$c"; tot=$((tot + c))
done
for t in "${DIRECT_ORDER[@]}"; do
  c="$(q "SELECT count(*) FROM $t WHERE project_id='$PID'" || echo 0)"
  printf "   %-32s %8s\n" "$t" "$c"; tot=$((tot + c))
done
echo "   -------------------------------------------------"
printf "   %-32s %8s\n" "TOTALE" "$tot"

if [ "$MODE" != "--apply" ]; then
  echo "(dry-run: nessuna modifica. Rilancia con --apply per eseguire.)"
  exit 0
fi

echo "-- APPLY: DELETE in transazione unica (rollback automatico su errore)…"
SQL="BEGIN;"
# I figli via run_id PRIMA di agent_runs.
for t in "${RUN_JOIN[@]}"; do
  SQL+=" DELETE FROM $t WHERE run_id IN (SELECT id FROM agent_runs WHERE project_id='$PID');"
done
for t in "${DIRECT_ORDER[@]}"; do
  SQL+=" DELETE FROM $t WHERE project_id='$PID';"
done
SQL+=" COMMIT;"
if "$PSQL" -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$PGDATABASE" -v ON_ERROR_STOP=1 -c "$SQL"; then
  echo "OK: copie meta del progetto $PID rimosse ($tot righe). Esegui VACUUM per recuperare spazio:"
  echo "    PGPASSWORD=$PGUSER \"$PSQL\" -h $PGHOST -p $PGPORT -U $PGUSER -d $PGDATABASE -c 'VACUUM (ANALYZE);'"
else
  echo "ERRORE: transazione rollbackata, nessuna riga rimossa. Controlla l'output sopra."
  exit 1
fi
