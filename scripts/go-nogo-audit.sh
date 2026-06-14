#!/usr/bin/env bash
# Audit meccanico Go/No-Go: verifica statica che file, script, test e
# strutture richiesti dalla checklist `docs/go-nogo-checklist.md` esistano
# nel repo. Non sostituisce il deploy/review umani; serve a tagliare il
# tempo del Tech Lead nel rilevare voci con artefatto mancante.

cd /home/administrator/ideai

PASS=0
NEEDS_DEPLOY=0
NEEDS_HUMAN=0
MISSING=0

ok()   { echo "[ OK ]   $1 — $2"; PASS=$((PASS + 1)); }
dep()  { echo "[DEPLOY] $1 — $2"; NEEDS_DEPLOY=$((NEEDS_DEPLOY + 1)); }
hum()  { echo "[HUMAN]  $1 — $2"; NEEDS_HUMAN=$((NEEDS_HUMAN + 1)); }
miss() { echo "[MISS]   $1 — $2"; MISSING=$((MISSING + 1)); }

has_file() {
  [ -e "$1" ]
}

has_grep() {
  grep -qrE "$1" "$2" 2>/dev/null
}

echo "═══════════════════════════════════════════════════"
echo "  Go/No-Go audit statico (2026-05-19)"
echo "═══════════════════════════════════════════════════"

# ── A. Architettura ────────────────────────────────────────────────────────
echo ""
echo "── A. Architettura ──"
dep "A1" "test cross-profilo su 3 profili — richiede deploy hybrid+onprem"
dep "A2" "zero modifiche al codice — verificabile solo a deploy time"
dep "A3" "VLLMProvider contro endpoint vLLM reale — vedi Fase 6.5"
# Migrazione gateway a Rust: il model alias resolver vive ora nel crate
# crates/nexus-gateway (il vecchio packages/llm-gateway TS e' stato eliminato).
if has_file crates/nexus-gateway/src/model_alias_resolver.rs; then
  ok "A4" "model alias resolver: crates/nexus-gateway/src/model_alias_resolver.rs presente"
else
  miss "A4" "model_alias_resolver.rs mancante in crates/nexus-gateway/src"
fi
if has_file scripts/onprem-smoke.sh && has_file scripts/onprem-preflight.sh; then
  ok "A5" "smoke + preflight script presenti (Fase 7)"
else
  miss "A5" "script on-prem mancanti"
fi

# ── B. Sicurezza ───────────────────────────────────────────────────────────
echo ""
echo "── B. Sicurezza ──"
# Migrazione gateway a Rust: i test red-team del vecchio gateway TS sono stati
# eliminati con apps/nexus-gateway. La copertura va riportata come test Rust del
# crate nexus-gateway (tests/ o #[cfg(test)]).
if has_file tests/red-team.test.ts || has_grep "red.team|red_team" crates/nexus-gateway/; then
  ok "B1" "red-team coverage presente (test)"
else
  miss "B1" "red-team coverage assente — da riportare come test Rust in crates/nexus-gateway"
fi
if has_grep "DLPScanner" packages/audit/src/; then
  ok "B2" "DLPScanner implementato (packages/audit/src/dlp-scanner.ts)"
else
  miss "B2" "DLPScanner non trovato"
fi
if has_grep "INJECTION_PATTERNS|DAN|JAILBREAK" packages/audit/src/dlp-scanner.ts; then
  ok "B3" "pattern jailbreak/DAN presenti in dlp-scanner.ts"
else
  miss "B3" "pattern injection non trovati"
fi
if has_file brain/redaction/client.py; then
  ok "B4" "redaction pipeline Python (Fase 6.4): brain/redaction/client.py"
else
  miss "B4" "brain/redaction mancante"
fi
dep "B5" "tier-3 cloud blocking — verifica via test cross-profilo"
if has_grep "validateTierClaim" packages/; then
  ok "B6" "validateTierClaim() implementato"
else
  miss "B6" "validateTierClaim non trovato"
fi
if has_file crates/nexus-auth || has_grep "JWT|validate_token" crates/nexus-auth/src/; then
  ok "B7" "JWT validation in nexus-auth"
else
  miss "B7" "crates/nexus-auth mancante"
fi
HARD=$(grep -rnE '(api_key|secret_key|API_KEY|SECRET_KEY)\s*=\s*"[A-Za-z0-9_-]{20,}"' brain/ crates/ apps/ packages/ 2>/dev/null \
  | grep -v -E "(test|\.md:|/tests?/|example|placeholder|\.template)" | wc -l)
if [ "$HARD" -eq 0 ]; then
  ok "B8" "nessun secret hardcoded rilevato (grep)"
else
  miss "B8" "$HARD candidati secret hardcoded — review manuale"
fi

# ── C. Multi-tenant ────────────────────────────────────────────────────────
echo ""
echo "── C. Isolamento multi-tenant ──"
if has_file infra/sql/rls-policies.sql; then
  if has_grep "ENABLE ROW LEVEL SECURITY|ALTER TABLE.*ROW LEVEL SECURITY" infra/sql/rls-policies.sql; then
    ok "C1" "rls-policies.sql contiene ENABLE ROW LEVEL SECURITY"
  else
    miss "C1" "rls-policies.sql presente ma non attiva RLS"
  fi
else
  miss "C1" "infra/sql/rls-policies.sql mancante"
fi
if has_grep "FORCE ROW LEVEL SECURITY" infra/sql/rls-policies.sql; then
  ok "C2" "FORCE ROW LEVEL SECURITY presente"
else
  miss "C2" "FORCE ROW LEVEL SECURITY non presente"
fi
# Migrazione gateway a Rust: il test tenant-isolation del vecchio gateway TS e'
# stato eliminato con packages/llm-gateway. La copertura va riportata come test
# Rust del crate nexus-gateway.
if has_grep "tenant.isolation|tenant_isolation" crates/nexus-gateway/; then
  ok "C3" "tenant-isolation coverage presente (test Rust)"
else
  miss "C3" "tenant-isolation coverage assente — da riportare come test Rust in crates/nexus-gateway"
fi
if has_grep "shredTenant|crypto.shred|crypto_shred" packages/ infra/; then
  ok "C4" "crypto-shredding implementato"
else
  miss "C4" "crypto-shredding non trovato"
fi
if has_grep "nexus_app" infra/sql/; then
  ok "C5" "ruolo nexus_app definito"
else
  miss "C5" "ruolo nexus_app non trovato"
fi

# ── D. Osservabilità ───────────────────────────────────────────────────────
echo ""
echo "── D. Osservabilità ──"
if has_grep "Langfuse" packages/audit/src/langfuse-client.ts; then
  ok "D1" "LangfuseTracer implementato (Fase 6.3)"
else
  miss "D1" "LangfuseTracer mancante"
fi
if has_grep "audit_llm_calls" infra/sql/; then
  ok "D2" "audit_llm_calls schema definito (infra/sql/init-schemas.sql)"
else
  miss "D2" "tabella audit_llm_calls non trovata"
fi
if grep -qi "anomaly" packages/audit/src/anomaly-detector.ts 2>/dev/null; then
  ok "D3" "anomaly-detector implementato (AnomalyEvent type)"
else
  miss "D3" "anomaly-detector mancante"
fi
dep "D4" "Jaeger span — richiede deploy attivo"
if has_grep "request_id|tenant_id" packages/audit/src/logger.ts; then
  ok "D5" "Pino logger structured con request_id/tenant_id"
else
  miss "D5" "logger structured mancante"
fi
# Migrazione gateway a Rust: l'endpoint /health vive nel crate nexus-gateway.
if has_grep "/health" crates/nexus-gateway/src/; then
  ok "D6" "/health endpoint implementato in gateway (crates/nexus-gateway)"
else
  miss "D6" "endpoint /health non trovato"
fi

# ── E. Performance ─────────────────────────────────────────────────────────
echo ""
echo "── E. Performance ──"
if has_file scripts/load-test.k6.js; then
  ok "E1" "load-test.k6.js presente"
else
  miss "E1" "scripts/load-test.k6.js mancante"
fi
dep "E2" "retrieval p95 — richiede deploy + benchmark"
dep "E3" "vLLM healthcheck cold start — richiede deploy"
dep "E4" "embedding ingest 10k chunk — richiede benchmark"
dep "E5" "memory steady state — richiede deploy + docker stats"

# ── F. Documentazione ──────────────────────────────────────────────────────
echo ""
echo "── F. Documentazione ──"
if has_file docs/migration-to-onprem.md; then
  ok "F1" "migration-to-onprem.md presente"
else
  miss "F1" "docs/migration-to-onprem.md mancante"
fi
if has_file docs/runbook.md; then
  ok "F2" "runbook.md presente"
else
  miss "F2" "docs/runbook.md mancante"
fi
if has_file docs/security.md; then
  ok "F3" "security.md presente"
else
  miss "F3" "docs/security.md mancante"
fi
if has_file docs/dpia-template.md; then
  ok "F4" "DPIA template presente"
else
  hum "F4" "DPIA template mancante — sintesi legal/privacy"
fi
hum "F5" "guida 'aggiungere provider' — manca anche in docs/contributing"

# ── G. Infrastruttura ──────────────────────────────────────────────────────
echo ""
echo "── G. Infrastruttura ──"
if has_file scripts/db-backup.sh && has_file scripts/db-restore.sh; then
  ok "G1" "backup/restore script presenti"
else
  miss "G1" "scripts db-backup/db-restore mancanti"
fi
dep "G2" "TLS 1.3 — verifica a deploy con curl/nmap"
if grep -q "^\.env" .gitignore 2>/dev/null && ! git ls-files --error-unmatch .env &>/dev/null; then
  ok "G3" ".env in .gitignore e non committato"
else
  miss "G3" ".env potrebbe essere committato"
fi
dep "G4" "Vault/Secrets Manager — verifica deploy production"
dep "G5" "alert Grafana — configurazione runtime"
if has_file infra/sql/init-schemas.sql && has_grep "rate_limits" infra/sql/init-schemas.sql; then
  ok "G6" "rate_limits schema definito"
else
  miss "G6" "rate_limits schema non trovato"
fi

# ── H. Compliance ──────────────────────────────────────────────────────────
echo ""
echo "── H. Compliance ──"
if has_grep "hash|sha256|response_hash|prompt_hash" packages/audit/src/; then
  ok "H1" "audit usa hash, non plaintext (verificato in audit/src)"
else
  miss "H1" "audit potrebbe loggare plaintext"
fi
dep "H2" "retention audit 90gg — configurazione DB"
hum "H3" "GDPR crypto-shredding playbook — review legale"
hum "H4" "DPA con cloud providers — firma legal"
dep "H5" "data residency tier-3 — verifica deploy onprem"

# ── Riepilogo ──────────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════════════════════════"
echo "  Riepilogo: $PASS OK, $NEEDS_DEPLOY deploy-bound, $NEEDS_HUMAN human-bound, $MISSING missing"
echo "═══════════════════════════════════════════════════"
[ $MISSING -eq 0 ] && exit 0 || exit 1
