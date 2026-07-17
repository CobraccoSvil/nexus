#!/usr/bin/env bash
# Nexus On-Premise Smoke Test
# Valida: Postgres up, vLLM up, gateway risponde, call LLM end-to-end completata.
# Uso: ./scripts/onprem-smoke.sh [GATEWAY_URL] [VLLM_URL]
# Default: localhost:3001 / localhost:8000

set -euo pipefail

GATEWAY_URL="${1:-http://localhost:3001}"
VLLM_URL="${2:-http://localhost:8000}"
POSTGRES_HOST="${POSTGRES_HOST:-localhost}"
POSTGRES_PORT="${POSTGRES_PORT:-5432}"
TIMEOUT=120
FAILURES=0

red()    { echo -e "\033[0;31m[FAIL] $*\033[0m"; }
green()  { echo -e "\033[0;32m[ OK ] $*\033[0m"; }
yellow() { echo -e "\033[0;33m[WAIT] $*\033[0m"; }

check() {
  local name="$1"; shift
  if "$@" &>/dev/null; then
    green "$name"
  else
    red "$name"
    FAILURES=$((FAILURES + 1))
  fi
}

wait_for() {
  local name="$1"; local url="$2"
  local elapsed=0
  yellow "Attendo $name ($url)..."
  until curl -sf "$url" &>/dev/null; do
    sleep 5; elapsed=$((elapsed + 5))
    if [ $elapsed -ge $TIMEOUT ]; then
      red "$name non risponde dopo ${TIMEOUT}s"
      FAILURES=$((FAILURES + 1))
      return 1
    fi
  done
  green "$name disponibile (${elapsed}s)"
}

echo ""
echo "═══════════════════════════════════════════"
echo "  Nexus On-Premise Smoke Test"
echo "  Gateway: $GATEWAY_URL"
echo "  vLLM:    $VLLM_URL"
echo "═══════════════════════════════════════════"
echo ""

# 1. Postgres
wait_for "Postgres" "http://dummy" || true  # pg_isready non usa HTTP
if pg_isready -h "$POSTGRES_HOST" -p "$POSTGRES_PORT" -U nexus -q; then
  green "Postgres: pg_isready OK"
else
  red "Postgres: non disponibile"
  FAILURES=$((FAILURES + 1))
fi

# 2. vLLM health
wait_for "vLLM" "$VLLM_URL/health"

# 3. vLLM modelli disponibili
MODELS=$(curl -sf "$VLLM_URL/v1/models" 2>/dev/null)
if echo "$MODELS" | grep -q '"id"'; then
  MODEL_ID=$(echo "$MODELS" | node -e "const d=JSON.parse(require('fs').readFileSync(0,'utf8')); console.log(d.data[0].id)" 2>/dev/null || echo "unknown")
  green "vLLM modello attivo: $MODEL_ID"
else
  red "vLLM: nessun modello trovato"
  FAILURES=$((FAILURES + 1))
fi

# 4. Gateway health
wait_for "Gateway" "$GATEWAY_URL/health"

# 5. Gateway /providers — verifica che solo vllm sia registrato
PROVIDERS=$(curl -sf "$GATEWAY_URL/providers" 2>/dev/null || echo "[]")
if echo "$PROVIDERS" | grep -q "vllm"; then
  green "Gateway: provider vllm registrato"
else
  yellow "Gateway /providers non disponibile o vllm non trovato (non critico se API non ancora esposta)"
fi

if echo "$PROVIDERS" | grep -qE "anthropic|openai|mistral"; then
  red "Gateway: provider cloud trovato in profilo onprem (violazione isolamento!)"
  FAILURES=$((FAILURES + 1))
fi

# 6. Call LLM end-to-end via gateway (richiede JWT valido in CI)
if [ -n "${NEXUS_TEST_TOKEN:-}" ]; then
  RESPONSE=$(curl -sf -X POST "$GATEWAY_URL/v1/complete" \
    -H "Authorization: Bearer $NEXUS_TEST_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "model": "coder-small",
      "messages": [{"role": "user", "content": "Rispondi con una sola parola: ok"}],
      "metadata": {
        "tenant_id": "smoke-test",
        "user_id": "smoke-user",
        "request_id": "smoke-001",
        "sensitivity_tier": 0,
        "feature": "smoke-test"
      }
    }' 2>/dev/null)

  if echo "$RESPONSE" | grep -q '"content"'; then
    green "Call LLM end-to-end: risposta ricevuta"
    PROVIDER=$(echo "$RESPONSE" | node -e "const d=JSON.parse(require('fs').readFileSync(0,'utf8')); console.log(d.provider_used ?? '?')" 2>/dev/null || echo "?")
    if [ "$PROVIDER" = "vllm" ]; then
      green "Provider usato: vllm (corretto)"
    else
      red "Provider usato: $PROVIDER (atteso: vllm)"
      FAILURES=$((FAILURES + 1))
    fi
  else
    red "Call LLM end-to-end: nessuna risposta valida"
    FAILURES=$((FAILURES + 1))
  fi
else
  yellow "NEXUS_TEST_TOKEN non impostato — skip call LLM end-to-end"
fi

echo ""
echo "═══════════════════════════════════════════"
if [ $FAILURES -eq 0 ]; then
  green "Smoke test SUPERATO (0 failure)"
  exit 0
else
  red "Smoke test FALLITO ($FAILURES failure)"
  exit 1
fi
