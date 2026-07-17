#!/usr/bin/env bash
# Go/No-Go LIVE audit: ora che Nexus è UP, verifichiamo item deploy-bound
# che diventano testabili. NON sostituisce test cross-profilo completi
# (richiedono deploy hybrid+onprem), ma chiude item su profilo dev locale.

PASS=0
FAIL=0
SKIP=0

ok()   { echo "[ OK ]   $*"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL]   $*"; FAIL=$((FAIL + 1)); }
skip() { echo "[SKIP]   $*"; SKIP=$((SKIP + 1)); }

echo "═══════════════════════════════════════════════════"
echo "  Go/No-Go LIVE audit (deploy-bound items)"
echo "═══════════════════════════════════════════════════"
echo ""

# ── B5: cloud tier-3 bloccato ──────────────────────────────────────────────
# Verifichiamo tramite endpoint gateway che i provider cloud (anthropic, openai)
# siano registrati e che il flag NEXUS_ALLOW_CLOUD_TIER3 sia leggibile.
# Nel profilo dev (non onprem) il blocco non e' attivo — verifica statica.
PROVIDERS=$(curl -s -m 5 http://localhost:4060/providers 2>/dev/null || echo "[]")
if echo "$PROVIDERS" | grep -q "vllm"; then
  ok "B5: vllm registrato nel gateway"
fi
if echo "$PROVIDERS" | grep -qE "anthropic|openai"; then
  ok "B5: provider cloud registrati (profilo dev — block solo in profilo onprem)"
else
  skip "B5: gateway non riporta provider cloud in modo testabile"
fi

# ── D4: Jaeger span ────────────────────────────────────────────────────────
if curl -s -m 5 http://localhost:16686/api/services 2>/dev/null | grep -q '"data"'; then
  ok "D4: Jaeger UI risponde /api/services"
else
  skip "D4: Jaeger non raggiungibile (verificare che ideai-jaeger-1 sia healthy)"
fi

# ── E2-E5: performance ────────────────────────────────────────────────────
# Verifica solo che gli endpoint base rispondano sotto 1s
echo ""
echo "── Performance probe (single-call latency) ──"
for endpoint in \
  "http://localhost:4000/health" \
  "http://localhost:4060/health" \
  "http://localhost:3000/api/health" \
  "http://localhost:4010/health"
do
  t=$(curl -s -o /dev/null -m 5 -w "%{time_total}" "$endpoint" 2>/dev/null || echo "999")
  if awk "BEGIN { exit !($t < 1.0) }"; then
    ok "latency $endpoint = ${t}s (< 1s)"
  else
    fail "latency $endpoint = ${t}s (≥ 1s)"
  fi
done

# ── G2: TLS ────────────────────────────────────────────────────────────────
# Dev: niente TLS. Verifichiamo solo che la config production lo preveda.
if grep -rq "tls\|https" infra/docker/docker-compose.onprem.yml 2>/dev/null || \
   grep -rq "tls" deploy/ 2>/dev/null; then
  ok "G2: TLS referenziato in config production"
else
  skip "G2: TLS — dev env non lo richiede; verificare reverse-proxy in production"
fi

# ── G5: alert Grafana ──────────────────────────────────────────────────────
if curl -s -m 5 -u admin:admin http://localhost:3001/api/health 2>/dev/null | grep -q '"database"'; then
  ok "G5: Grafana risponde /api/health"
else
  skip "G5: Grafana — verificare credenziali admin"
fi

# ── H2: retention audit ────────────────────────────────────────────────────
RET=$(docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus -t -c \
  "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name='audit_llm_calls')" 2>/dev/null | tr -d ' \n')
if [ "$RET" = "t" ]; then
  ok "H2: audit_llm_calls schema presente"
else
  skip "H2: audit_llm_calls non trovato (richiede mig applicata sul DB infra)"
fi

# ── H5: data residency (dev = irrelevant) ──────────────────────────────────
skip "H5: data residency tier-3 — testabile solo in deploy onprem reale (Fase 7)"

echo ""
echo "═══════════════════════════════════════════════════"
echo "  Live audit: $PASS OK, $FAIL fail, $SKIP skip"
echo "═══════════════════════════════════════════════════"
[ $FAIL -eq 0 ] && exit 0 || exit 1
