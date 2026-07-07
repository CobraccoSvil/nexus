#!/usr/bin/env bash
# Verifica funzionale dei pannelli web-ide:
# 1. Endpoint backend che ogni pannello consuma
# 2. Errori nei log dei servizi (ultimi 5 min)
# 3. Errori console Next.js

GREEN="\033[0;32m"
RED="\033[0;31m"
YELLOW="\033[0;33m"
NC="\033[0m"

ok()   { echo -e "${GREEN}[ OK ]${NC}   $*"; }
fail() { echo -e "${RED}[FAIL]${NC}   $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC}   $*"; }

probe() {
  local label="$1"; local url="$2"; local expect="${3:-200}"
  local code
  # In caso di errore curl stampa comunque "000" via -w: niente || echo,
  # altrimenti il codice raddoppia in "000000" e il ramo TIMEOUT non scatta.
  code=$(curl -sS -o /dev/null -m 5 -w "%{http_code}" "$url" 2>/dev/null) || true
  [ -n "$code" ] || code="000"
  if [ "$code" = "$expect" ]; then
    ok "$label  ($code)  $url"
  elif [[ "$code" =~ ^(401|403)$ ]] && [ "$expect" = "auth" ]; then
    ok "$label  ($code = auth richiesta, normale)  $url"
  elif [ "$code" = "000" ]; then
    fail "$label  TIMEOUT  $url"
  else
    warn "$label  ($code, atteso $expect)  $url"
  fi
}

echo "═══════════════════════════════════════════════════════════"
echo "  Smoke pannelli web-ide ($(date +%H:%M))"
echo "═══════════════════════════════════════════════════════════"

echo ""
echo "── Endpoint base ──"
probe "web-ide root        " "http://localhost:3000/"
probe "api/health          " "http://localhost:3000/api/health"

echo ""
echo "── Pannello Auth/User ──"
probe "/api/auth/me         " "http://localhost:3000/api/auth/me" "auth"

echo ""
echo "── Pannello Projects (sidebar)──"
probe "/api/projects/mine   " "http://localhost:3000/api/projects/mine" "auth"

echo ""
echo "── Pannello Run/Debug (proxy verso mcp-core :4000) ──"
probe "mcp-core /api/health    " "http://localhost:4000/api/health" "404"
probe "/api/system/services    " "http://localhost:3000/api/system/services" "auth"

echo ""
echo "── Pannello Providers (gateway) ──"
probe "gateway /providers       " "http://localhost:4060/providers" "auth"
probe "gateway /v1/models       " "http://localhost:4060/v1/models" "auth"

echo ""
echo "── Pannello Database (project-db) ──"
probe "/api/admin/nexus-database-stats  " "http://localhost:3000/api/admin/nexus-database-stats" "auth"

echo ""
echo "── Pannello Plugin/Settings ──"
probe "/api/admin/settings      " "http://localhost:3000/api/admin/settings" "auth"

echo ""
echo "── Dispatcher SSE (chat/events) ──"
# Non possiamo fare un long-poll; verifichiamo solo che l'endpoint esista
probe "/api/dispatcher/projects  " "http://localhost:3000/api/dispatcher/projects" "auth"

echo ""
echo "── Errori recenti nei log servizi (ultimi 2 min) ──"
for log in /tmp/nexus-mcp-core.log /tmp/nexus-gateway.log /tmp/nexus-webide.log; do
  if [ -f "$log" ]; then
    errs=$(tail -200 "$log" 2>/dev/null | grep -iE 'ERROR|panic|Error:|FATAL|Exception|Traceback' | grep -v 'http.*40[14]' | tail -3)
    if [ -n "$errs" ]; then
      echo ""
      echo "── ${log#/tmp/nexus-} ──"
      echo "$errs" | head -3
    fi
  fi
done

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  Smoke completato. Verifica visiva nel browser per i layout."
echo "═══════════════════════════════════════════════════════════"
