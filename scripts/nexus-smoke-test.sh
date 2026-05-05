#!/usr/bin/env bash
# Nexus smoke test — valida i 4 endpoint pubblici del NexusBridge dopo il deploy.
#
# Usage:
#   scripts/nexus-smoke-test.sh [host:port]
#
# Examples:
#   scripts/nexus-smoke-test.sh                 # default: localhost:4000
#   scripts/nexus-smoke-test.sh server-prod:4000     # remote host
#   BASE_URL=http://1.2.3.4:4000 scripts/nexus-smoke-test.sh
#
# Exit codes:
#   0 — tutti gli endpoint rispondono correttamente
#   1 — almeno un endpoint fallisce / risposta malformata
#   2 — dipendenza mancante (curl / jq)

set -euo pipefail

# --- dependencies --------------------------------------------------------
for bin in curl jq; do
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "ERROR: '$bin' non trovato nel PATH" >&2
        exit 2
    fi
done

# --- config --------------------------------------------------------------
HOST_ARG="${1:-}"
if [[ -n "$HOST_ARG" ]]; then
    BASE_URL="http://${HOST_ARG}"
else
    BASE_URL="${BASE_URL:-http://localhost:4000}"
fi

# Colori (disattiva se output non è tty)
if [[ -t 1 ]]; then
    C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YEL=$'\033[33m'
    C_CYAN=$'\033[36m'; C_BOLD=$'\033[1m'; C_OFF=$'\033[0m'
else
    C_RED=""; C_GREEN=""; C_YEL=""; C_CYAN=""; C_BOLD=""; C_OFF=""
fi

PASS=0
FAIL=0
FAILED_ENDPOINTS=()

check() {
    local name="$1"
    local path="$2"
    local expect_code="$3"
    local validator="$4"

    local url="${BASE_URL}${path}"
    printf "  %-30s " "$name"

    local tmp
    tmp="$(mktemp)"
    local code
    code=$(curl -sS -o "$tmp" -w "%{http_code}" --max-time 10 "$url" || echo "000")

    if [[ "$code" != "$expect_code" ]]; then
        printf "%sFAIL%s (HTTP %s, expected %s)\n" "$C_RED" "$C_OFF" "$code" "$expect_code"
        FAIL=$((FAIL + 1))
        FAILED_ENDPOINTS+=("$name")
        rm -f "$tmp"
        return
    fi

    if [[ -n "$validator" ]]; then
        # Il validator riceve il path del body via $BODY invece di stdin:
        # così può usare più comandi (grep/jq) senza consumare stdin una volta sola.
        if ! BODY="$tmp" bash -c "$validator" >/dev/null 2>&1; then
            printf "%sFAIL%s (validazione body fallita)\n" "$C_RED" "$C_OFF"
            echo "    body: $(head -c 200 "$tmp")"
            FAIL=$((FAIL + 1))
            FAILED_ENDPOINTS+=("$name")
            rm -f "$tmp"
            return
        fi
    fi

    printf "%sOK%s\n" "$C_GREEN" "$C_OFF"
    PASS=$((PASS + 1))
    rm -f "$tmp"
}

echo "${C_BOLD}Nexus smoke test${C_OFF}"
echo "  target: ${C_CYAN}${BASE_URL}${C_OFF}"
echo

# 1. healthz — status=ok e router.total_decisions numerico.
check "GET /nexus/healthz" "/nexus/healthz" "200" \
    'jq -e ".status == \"ok\" and (.router.total_decisions|type == \"number\")" "$BODY"'

# 2. stats — JSON con router + scheduler.workers_registered.
check "GET /nexus/stats" "/nexus/stats" "200" \
    'jq -e ".router and .scheduler and (.scheduler.workers_registered|type == \"number\")" "$BODY"'

# 3. tools — catalog con total > 0 e breakdown (mappa categorie → count).
check "GET /nexus/tools" "/nexus/tools" "200" \
    'jq -e "(.total|type == \"number\") and (.total > 0) and (.breakdown|type == \"object\") and (.target == 314)" "$BODY"'

# 4. metrics — Prometheus text exposition con le 3 serie core (conteggio via regex alternativa).
check "GET /nexus/metrics" "/nexus/metrics" "200" \
    '[ "$(grep -cE "^(nexus_router_decisions_total|nexus_router_epsilon|nexus_scheduler_runs_total) " "$BODY")" -ge 3 ]'

echo
printf "  %sresult%s: %d passed, %d failed\n" "$C_BOLD" "$C_OFF" "$PASS" "$FAIL"

if (( FAIL > 0 )); then
    echo "  ${C_RED}failed endpoints:${C_OFF} ${FAILED_ENDPOINTS[*]}"
    echo
    echo "  ${C_YEL}hint${C_OFF}: se /nexus/healthz ritorna 503, il NexusBridge non è stato"
    echo "  inizializzato al boot di mcp-core. Controllare i log: 'systemctl status"
    echo "  ai-orchestrator-mcp-core' su server-prod oppure 'docker logs mcp-core'."
    exit 1
fi

echo "  ${C_GREEN}${C_BOLD}tutti gli endpoint Nexus sono operativi${C_OFF}"
exit 0
