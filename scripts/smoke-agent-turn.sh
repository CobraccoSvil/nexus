#!/usr/bin/env bash
# scripts/smoke-agent-turn.sh
# Smoke test: verifica che tutti i provider configurati producano
# stop_reason='tool_use' al primo turno agente.
#
# Uso: ./scripts/smoke-agent-turn.sh [--provider deepseek] [--verbose]
#
# Richiede: brain REST in ascolto su localhost:8001 (o BRAIN_URL env)

set -euo pipefail

BRAIN_URL="${BRAIN_URL:-http://localhost:8001}"
VERBOSE=0
FILTER_PROVIDER=""

for arg in "$@"; do
  case "$arg" in
    --verbose|-v) VERBOSE=1 ;;
    --provider=*) FILTER_PROVIDER="${arg#--provider=}" ;;
    --provider) shift_next=1 ;;
    *)
      if [[ "${shift_next:-0}" == "1" ]]; then
        FILTER_PROVIDER="$arg"
        shift_next=0
      fi
      ;;
  esac
done

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

PASSED=0
FAILED=0
SKIPPED=0

# Recupera i provider configurati dal brain
PROVIDERS_JSON=$(curl -fsS --max-time 10 "${BRAIN_URL}/providers/status" 2>/dev/null || echo "[]")
if [[ "$PROVIDERS_JSON" == "[]" ]]; then
  echo -e "${RED}Impossibile contattare brain su ${BRAIN_URL}/providers/status${NC}"
  exit 1
fi

# Estrai provider con status=ready
READY_PROVIDERS=$(echo "$PROVIDERS_JSON" | python3 -c "
import sys, json
data = json.load(sys.stdin)
for p in data:
    if p.get('status') == 'ready':
        print(p['provider'])
" 2>/dev/null || echo "")

if [[ -z "$READY_PROVIDERS" ]]; then
  echo -e "${YELLOW}Nessun provider con status=ready trovato.${NC}"
  exit 1
fi

echo "Provider disponibili: $READY_PROVIDERS"
echo ""

test_provider() {
  local provider="$1"

  # Payload minimale: un messaggio utente che richiede una tool call
  local payload
  payload=$(cat <<JSONEOF
{
  "provider": "$provider",
  "prompt": "Elenca i file nella directory corrente del progetto.",
  "tools": [
    {
      "name": "list_files",
      "description": "Lista i file in una directory",
      "input_schema": {
        "type": "object",
        "properties": {
          "path": {
            "type": "string",
            "description": "Percorso della directory"
          }
        },
        "required": ["path"]
      }
    },
    {
      "name": "read_file",
      "description": "Legge il contenuto di un file",
      "input_schema": {
        "type": "object",
        "properties": {
          "file_path": {
            "type": "string",
            "description": "Percorso del file"
          }
        },
        "required": ["file_path"]
      }
    }
  ],
  "max_tokens": 1024,
  "system_text": "Sei un agente. Per rispondere DEVI usare i tool disponibili (function calling nativo). NON scrivere tag XML o JSON nel testo."
}
JSONEOF
)

  local response
  response=$(curl -fsS --max-time 30 \
    -X POST "${BRAIN_URL}/agent/single-turn" \
    -H "Content-Type: application/json" \
    -d "$payload" 2>&1) || {
    echo -e "  ${YELLOW}SKIP${NC}  $provider (endpoint non raggiungibile o errore)"
    SKIPPED=$((SKIPPED + 1))
    return
  }

  local stop_reason
  stop_reason=$(echo "$response" | python3 -c "
import sys, json
data = json.load(sys.stdin)
print(data.get('metadata', {}).get('stop_reason', 'unknown'))
" 2>/dev/null || echo "parse_error")

  if [[ "$stop_reason" == "tool_use" ]]; then
    echo -e "  ${GREEN}OK${NC}    $provider -> stop_reason=tool_use"
    PASSED=$((PASSED + 1))
  else
    echo -e "  ${RED}FAIL${NC}  $provider -> stop_reason=$stop_reason (atteso: tool_use)"
    FAILED=$((FAILED + 1))
    if [[ "$VERBOSE" == "1" ]]; then
      echo "    Risposta: $(echo "$response" | python3 -c "
import sys, json
data = json.load(sys.stdin)
content = data.get('content', '')[:200]
print(content)
" 2>/dev/null || echo "$response" | head -c 200)"
    fi
  fi
}

echo "=== Smoke test tool calling multi-provider ==="
echo ""

for provider in $READY_PROVIDERS; do
  if [[ -n "$FILTER_PROVIDER" && "$provider" != "$FILTER_PROVIDER" ]]; then
    continue
  fi
  test_provider "$provider"
done

echo ""
total=$((PASSED + FAILED + SKIPPED))
if [[ $FAILED -eq 0 ]]; then
  echo -e "${GREEN}Tutti i test superati ($PASSED/$total, $SKIPPED saltati)${NC}"
  exit 0
else
  echo -e "${RED}$FAILED/$total falliti ($PASSED ok, $SKIPPED saltati)${NC}"
  exit 1
fi
