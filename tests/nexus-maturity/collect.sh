#!/usr/bin/env bash
# Raccolta dati iterazione del test di maturita Nexus.
# Uso: bash collect.sh <TS> <PROJECT_ID> <T0_ISO> <ITER_LABEL>
#   TS         timestamp sessione (es. 2026-05-14T1556)
#   PROJECT_ID UUID progetto target registrato in Nexus
#   T0_ISO     timestamp inizio iterazione (es. 2026-05-14T16:30:00+02:00)
#   ITER_LABEL etichetta iter (es. iter_01)
#
# Variabili d'ambiente attese:
#   PGPASSWORD   password Postgres Nexus (host)
#   PG_HOST      default localhost
#   PG_PORT      default 5433
#   PG_USER      default nexus
#   PG_DB        default nexus
#   PROJ_PATH    path progetto target (per git diff)
#   TARGET_BASELINE_SHA file con sha baseline progetto target
#   IDEAI_BASELINE_SHA  file con sha baseline monorepo IDEAI

set -uo pipefail

TS="${1:?timestamp sessione mancante}"
PID="${2:?project id mancante}"
T0="${3:?T0 ISO mancante}"
ITER="${4:?iter label mancante}"

REPO_ROOT="${REPO_ROOT:-/home/administrator/ideai}"
OUT="$REPO_ROOT/tests/nexus-maturity/$TS/$ITER"
mkdir -p "$OUT"

PG_CONTAINER="${PG_CONTAINER:-ideai-postgres-nexus-1}"
PG_USER="${PG_USER:-nexus}"
PG_DB="${PG_DB:-nexus}"
PROJ_PATH="${PROJ_PATH:-}"

# Fix M7: usa docker exec invece di psql host (non installato in WSL),
# e passa PID/T0 come variabili psql -v per evitare problemi di escape SQL.
# Il quoting `:'name'` di psql sostituisce in modo sicuro la variabile dentro stringhe SQL.
run_psql_copy() {
  local sql="$1"
  local out_file="$2"
  local err_file="$3"
  docker exec -i "$PG_CONTAINER" psql \
    -U "$PG_USER" -d "$PG_DB" -At \
    -v pid="$PID" -v t0="$T0" \
    -c "$sql" > "$out_file" 2>"$err_file" || true
}

echo "[collect] dump tabelle agente -> $OUT"

run_psql_copy \
  "COPY (SELECT * FROM agent_runs WHERE project_id=:'pid'::uuid AND created_at >= :'t0'::timestamptz ORDER BY created_at) TO STDOUT WITH CSV HEADER" \
  "$OUT/agent_runs.csv" "$OUT/agent_runs.err"

run_psql_copy \
  "COPY (SELECT s.* FROM agent_steps s JOIN agent_runs r ON r.id = s.run_id WHERE r.project_id=:'pid'::uuid AND r.created_at >= :'t0'::timestamptz ORDER BY s.run_id, s.step_index) TO STDOUT WITH CSV HEADER" \
  "$OUT/agent_steps.csv" "$OUT/agent_steps.err"

run_psql_copy \
  "COPY (SELECT * FROM agent_processes WHERE project_id=:'pid'::uuid ORDER BY id) TO STDOUT WITH CSV HEADER" \
  "$OUT/agent_processes.csv" "$OUT/agent_processes.err"

run_psql_copy \
  "COPY (SELECT * FROM ai_usage_ledger WHERE project_id=:'pid'::uuid AND created_at >= :'t0'::timestamptz ORDER BY created_at) TO STDOUT WITH CSV HEADER" \
  "$OUT/ai_usage_ledger.csv" "$OUT/ai_usage_ledger.err"

echo "[collect] tool name frequency"
run_psql_copy \
  "COPY (SELECT s.tool_name, COUNT(*) AS n FROM agent_steps s JOIN agent_runs r ON r.id=s.run_id WHERE r.project_id=:'pid'::uuid AND r.created_at >= :'t0'::timestamptz GROUP BY 1 ORDER BY 2 DESC) TO STDOUT WITH CSV HEADER" \
  "$OUT/tool_name_frequency.csv" "$OUT/tool_name_frequency.err"

echo "[collect] cost summary"
run_psql_copy \
  "COPY (SELECT provider, model, COUNT(*) AS calls, SUM(prompt_tokens) AS pt, SUM(completion_tokens) AS ct, SUM(total_cost) AS cost_eur FROM ai_usage_ledger WHERE project_id=:'pid'::uuid AND created_at >= :'t0'::timestamptz GROUP BY 1,2 ORDER BY cost_eur DESC NULLS LAST) TO STDOUT WITH CSV HEADER" \
  "$OUT/cost_by_model.csv" "$OUT/cost_by_model.err"

echo "[collect] docker logs (since $T0)"
for c in ideai-mcp-core-1 ideai-brain-1 ideai-web-ide-1 ideai-postgres-nexus-1; do
  if docker ps --format '{{.Names}}' | grep -q "^${c}$"; then
    docker logs --since "$T0" "$c" > "$OUT/${c}.log" 2> "$OUT/${c}.log.stderr" || true
  else
    echo "(container $c non attivo)" > "$OUT/${c}.log"
  fi
done

echo "[collect] git diff progetto target"
if [ -n "$PROJ_PATH" ] && [ -d "$PROJ_PATH/.git" ]; then
  BASE_SHA="$(cat "${TARGET_BASELINE_SHA:-/tmp/target_baseline.sha}" 2>/dev/null || echo "")"
  if [ -n "$BASE_SHA" ]; then
    git -C "$PROJ_PATH" diff "$BASE_SHA"..HEAD > "$OUT/git-target-diff.patch" 2>/dev/null || echo "" > "$OUT/git-target-diff.patch"
    git -C "$PROJ_PATH" log --since "$T0" --stat > "$OUT/git-target-log.txt" 2>/dev/null || true
  fi
  git -C "$PROJ_PATH" status --porcelain > "$OUT/git-target-status.txt" 2>/dev/null || true
fi

echo "[collect] git status monorepo IDEAI (contamination check)"
git -C "$REPO_ROOT" status --porcelain > "$OUT/git-ideai-status.txt" 2>/dev/null || true
BASE_IDEAI="$(cat "${IDEAI_BASELINE_SHA:-/tmp/ideai_baseline.sha}" 2>/dev/null || echo "")"
if [ -n "$BASE_IDEAI" ]; then
  git -C "$REPO_ROOT" diff "$BASE_IDEAI"..HEAD > "$OUT/git-ideai-diff.patch" 2>/dev/null || true
  git -C "$REPO_ROOT" log "$BASE_IDEAI"..HEAD --oneline > "$OUT/git-ideai-commits-since-baseline.txt" 2>/dev/null || true
fi

echo "[collect] violazioni regole non automatizzate (grep su progetto target)"
if [ -n "$PROJ_PATH" ] && [ -d "$PROJ_PATH" ]; then
  {
    echo "# Modelli AI hardcoded"
    grep -rnE "mistral-|claude-3|claude-sonnet|claude-haiku|claude-opus|gpt-4|gemini-" "$PROJ_PATH" --include="*.rs" --include="*.ts" --include="*.tsx" --include="*.py" --include="*.js" 2>/dev/null || true
    echo
    echo "# Emoji nei sorgenti"
    grep -rPn "[\x{1F300}-\x{1FAFF}\x{2600}-\x{27BF}\x{1F000}-\x{1F2FF}\x{2700}-\x{27BF}]" "$PROJ_PATH" --include="*.rs" --include="*.ts" --include="*.tsx" --include="*.py" --include="*.js" 2>/dev/null || true
    echo
    echo "# unwrap()/expect() fuori test (file *.rs, esclusi tests/)"
    grep -rn "\.unwrap()\|\.expect(" "$PROJ_PATH" --include="*.rs" 2>/dev/null | grep -v "/tests/" || true
    echo
    echo "# Log payload chiaro"
    grep -rPn "tracing::(info|debug|warn|error|trace)!\s*\([^)]*\b(payload|prompt|response)\s*=\s*[^%?h]" "$PROJ_PATH" --include="*.rs" 2>/dev/null || true
  } > "$OUT/violations.txt"
fi

echo "[collect] done. Output in $OUT"
ls -la "$OUT"
