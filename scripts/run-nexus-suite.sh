#!/bin/bash
# Esegue tutte le suite di test Nexus (PR-4 Livelli 3, 4, 5, 6) in sequenza.
#
# Prerequisiti runtime (NON gestiti automaticamente):
#   - postgres-nexus su :5433 + postgres-app su :5434 (docker compose up)
#   - mcp-core su :4000, brain su :8001, web-ide su :3000, admin-service su :4010
#   - JWT di un utente admin in /tmp/nexus_jwt.txt (vedi tests/e2e/nexus-suite/_helpers/cfg.py)
#
# Skip non bloccanti: i test che richiedono servizi specifici si auto-skippano
# se non disponibili (tests sono robusti a CI shape variations).
#
# Exit code: 0 se tutte le suite passate (skip ammessi), 1 altrimenti.

set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

OVERALL=0

log() { echo "[nexus-suite] $*"; }
section() { echo; echo "═══════════════════════════════════════════════════"; echo "  $*"; echo "═══════════════════════════════════════════════════"; }

section "Livello 3 — Contract test Rust (mcp-core/tests)"
if cargo test -p mcp-core --tests --release -- --nocapture 2>&1 | tail -30; then
  log "Livello 3 OK"
else
  log "Livello 3 FAILED"
  OVERALL=1
fi

section "Livello 4 — E2E nexus-suite (pytest)"
if python3 tests/e2e/nexus-suite/run_all.py; then
  log "Livello 4 OK"
else
  log "Livello 4 FAILED"
  OVERALL=1
fi

section "Livello 5 — Maturity v2 (rubric runner, run più recente)"
RECENT_RUN_ID=$(docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -t -A -c \
  "SELECT id FROM agent_runs ORDER BY created_at DESC LIMIT 1" 2>/dev/null | tr -d '[:space:]')
if [ -n "$RECENT_RUN_ID" ]; then
  python3 tests/nexus-maturity/v2/run_rubric.py --run-id "$RECENT_RUN_ID" || OVERALL=1
else
  log "skip: nessun agent_run in DB"
fi

section "Livello 6 — UI e2e Playwright (apps/web-ide/e2e/orchestrator)"
if command -v playwright >/dev/null 2>&1 || pnpm --filter web-ide exec playwright --version >/dev/null 2>&1; then
  pnpm --filter web-ide exec playwright test --config=apps/web-ide/playwright.config.ts \
    apps/web-ide/e2e/orchestrator || OVERALL=1
else
  log "skip: playwright non installato — esegui 'pnpm add -D @playwright/test' + 'playwright install'"
fi

echo
if [ "$OVERALL" -eq 0 ]; then
  log "═══ Nexus suite: ALL PASSED ═══"
else
  log "═══ Nexus suite: FAILED ═══"
fi
exit $OVERALL
