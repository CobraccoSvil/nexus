#!/usr/bin/env bash
# Monitor iter Nexus maturity: snapshot DB + grep violazioni + git contamination ogni 60s.
# Uso: monitor.sh <TS> <PROJECT_ID> <PROJ_PATH> <ITER_LABEL> [IDEAI_BASELINE_SHA]
# Scrive:
#   - iter_LABEL/monitor.jsonl (una riga JSON per snapshot)
#   - iter_LABEL/monitor.log (heartbeat human-readable)
#   - iter_LABEL/STOP_TRIGGER.txt (se trigger rilevato, fermo il loop)
set -u

TS="${1:?ts}"
PID="${2:?project_id}"
PROJ="${3:?project path}"
ITER="${4:?iter label}"
IDEAI_BASE="${5:-77dd0929503f1b57cd627195c839f0d33cf95219}"

REPO=/home/administrator/ideai
OUT="$REPO/tests/nexus-maturity/$TS/$ITER"
mkdir -p "$OUT"
JSONL="$OUT/monitor.jsonl"
LOG="$OUT/monitor.log"
STOP_FILE="$OUT/STOP_TRIGGER.txt"

DPS="docker exec -i ideai-postgres-nexus-1 psql -U nexus -d nexus -At"

emit_log() { echo "[$(date -Iseconds)] $1" | tee -a "$LOG"; }
write_stop() {
  if [ -f "$STOP_FILE" ]; then return; fi
  local reason="$1"; local detail="$2"
  cat > "$STOP_FILE" <<EOF
[$(date -Iseconds)] $reason

$detail
EOF
  emit_log "STOP_TRIGGER scritto: $reason"
}

emit_log "monitor START — TS=$TS PID=$PID PROJ=$PROJ ITER=$ITER baseline=$IDEAI_BASE"

while true; do
  TS_NOW=$(date -Iseconds)
  T_EPOCH=$(date +%s)

  # 1) costo iter + count step
  COST=$($DPS -c "SELECT COALESCE(SUM(total_cost),0)::text FROM ai_usage_ledger WHERE project_id='$PID';" 2>/dev/null | head -1)
  TOKENS=$($DPS -c "SELECT COALESCE(SUM(total_tokens),0)::text FROM ai_usage_ledger WHERE project_id='$PID';" 2>/dev/null | head -1)
  RUN_COUNT=$($DPS -c "SELECT COUNT(*) FROM agent_runs WHERE project_id='$PID';" 2>/dev/null | head -1)
  RUN_ACTIVE=$($DPS -c "SELECT COUNT(*) FROM agent_runs WHERE project_id='$PID' AND status='running';" 2>/dev/null | head -1)
  STEP_COUNT=$($DPS -c "SELECT COUNT(*) FROM agent_steps s JOIN agent_runs r ON r.id=s.run_id WHERE r.project_id='$PID';" 2>/dev/null | head -1)
  LAST_TOOL=$($DPS -c "SELECT s.tool_name FROM agent_steps s JOIN agent_runs r ON r.id=s.run_id WHERE r.project_id='$PID' ORDER BY s.created_at DESC LIMIT 1;" 2>/dev/null | head -1)

  # 2) contamination check su monorepo IDEAI (esclude tests/nexus-maturity e projects/)
  IDEAI_DIFF=$(git -C "$REPO" status --porcelain 2>/dev/null | grep -vE '^(.. (tests/nexus-maturity/|projects/))' | head -30)
  CONTAMINATED=0
  if [ -n "$IDEAI_DIFF" ]; then
    # Filtra anche eventuali commit fatti dopo baseline
    IDEAI_DIVERGED=$(git -C "$REPO" rev-list "$IDEAI_BASE..HEAD" --count 2>/dev/null || echo 0)
    if [ "${IDEAI_DIVERGED:-0}" -gt 0 ]; then
      CONTAMINATED=1
    fi
    # Modifiche file out-of-scope:
    NUM_OOS=$(git -C "$REPO" status --porcelain 2>/dev/null | grep -cvE '^(.. (tests/nexus-maturity/|projects/))')
    if [ "${NUM_OOS:-0}" -gt 0 ]; then
      CONTAMINATED=1
    fi
  fi

  # 3) violazioni qualita su PROJ
  V_MODELS=$(grep -rnE 'mistral-|claude-3|claude-sonnet|claude-haiku|claude-opus|gpt-4|gemini-' "$PROJ" --include='*.rs' --include='*.ts' --include='*.tsx' --include='*.py' --include='*.js' 2>/dev/null | wc -l)
  V_EMOJI=$(grep -rPn '[\x{1F300}-\x{1FAFF}\x{2600}-\x{27BF}\x{1F000}-\x{1F2FF}\x{2700}-\x{27BF}]' "$PROJ" --include='*.rs' --include='*.ts' --include='*.tsx' --include='*.py' --include='*.js' 2>/dev/null | wc -l)
  V_UNWRAP=$(grep -rn '\.unwrap()\|\.expect(' "$PROJ" --include='*.rs' 2>/dev/null | grep -cv '/tests/')
  V_LOG=$(grep -rPn 'tracing::(info|debug|warn|error|trace)!\s*\([^)]*\b(payload|prompt|response)\s*=\s*[^%?h]' "$PROJ" --include='*.rs' 2>/dev/null | wc -l)

  # 4) JSONL row
  printf '{"t":"%s","cost":%s,"tokens":%s,"runs":%s,"active":%s,"steps":%s,"last_tool":"%s","violations":{"models":%s,"emoji":%s,"unwrap":%s,"log":%s},"contamination":%s}\n' \
    "$TS_NOW" "${COST:-0}" "${TOKENS:-0}" "${RUN_COUNT:-0}" "${RUN_ACTIVE:-0}" "${STEP_COUNT:-0}" "${LAST_TOOL:-}" \
    "$V_MODELS" "$V_EMOJI" "$V_UNWRAP" "$V_LOG" "$CONTAMINATED" >> "$JSONL"

  emit_log "tick: cost=${COST:-0} tokens=${TOKENS:-0} runs=${RUN_COUNT:-0}(active=${RUN_ACTIVE:-0}) steps=${STEP_COUNT:-0} last_tool=${LAST_TOOL:-} viol(m/e/u/l)=$V_MODELS/$V_EMOJI/$V_UNWRAP/$V_LOG contam=$CONTAMINATED"

  # 5) trigger checks
  if [ "$CONTAMINATED" = "1" ]; then
    write_stop "CONTAMINATION" "$(git -C $REPO status --porcelain 2>&1 | grep -vE '^(.. (tests/nexus-maturity/|projects/))' | head -20)"
  fi
  if [ "$V_MODELS" -gt 0 ]; then
    write_stop "VIOLATION_HARDCODED_MODEL" "$(grep -rnE 'mistral-|claude-3|claude-sonnet|claude-haiku|claude-opus|gpt-4|gemini-' $PROJ --include='*.rs' --include='*.ts' --include='*.tsx' --include='*.py' --include='*.js' 2>/dev/null | head -10)"
  fi
  if [ "$V_EMOJI" -gt 0 ]; then
    write_stop "VIOLATION_EMOJI" "$(grep -rPn '[\x{1F300}-\x{1FAFF}\x{2600}-\x{27BF}\x{1F000}-\x{1F2FF}\x{2700}-\x{27BF}]' $PROJ --include='*.rs' --include='*.ts' --include='*.tsx' --include='*.py' --include='*.js' 2>/dev/null | head -10)"
  fi
  if [ "$V_UNWRAP" -gt 0 ]; then
    write_stop "VIOLATION_UNWRAP_OUT_OF_TEST" "$(grep -rn '\.unwrap()\|\.expect(' $PROJ --include='*.rs' 2>/dev/null | grep -v '/tests/' | head -10)"
  fi
  if [ "$V_LOG" -gt 0 ]; then
    write_stop "VIOLATION_LOG_PAYLOAD_CLEAR" "$(grep -rPn 'tracing::(info|debug|warn|error|trace)!\s*\([^)]*\b(payload|prompt|response)\s*=\s*[^%?h]' $PROJ --include='*.rs' 2>/dev/null | head -10)"
  fi
  # cost budget anomaly
  COST_NUM=${COST:-0}
  if awk -v c="$COST_NUM" 'BEGIN{exit !(c+0 > 30)}'; then
    write_stop "BUDGET_ANOMALY" "Costo iterazione ${COST_NUM} EUR > 30 EUR (anomalo per una sola iterazione)"
  fi

  sleep 60
done
