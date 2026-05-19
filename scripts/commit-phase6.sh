#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add brain/redaction/ \
        brain/tests/test_redaction.py \
        docs/hybrid-llm-phase6-status.md \
        scripts/smoke-redaction.sh \
        scripts/commit-phase6.sh

git commit -m "$(cat <<'EOF'
feat(redaction): client Python Presidio + status report Fase 6 hybrid

Fase 6 del piano `docs/nexus-hybrid-llm-plan.md` (sotto-fasi 3-7) era
documentata come "non implementata" ma in realta':

- 6.5 vLLM: container completo gia' in
  `infra/docker/docker-compose.onprem.yml` (profile main + profile cpu-test).
- 6.4 Redaction: container Presidio (analyzer + anonymizer) gia' nel
  compose. MANCAVA il client Python lato brain.
- 6.3 Audit/Langfuse: `LangfuseTracer` gia' implementato in
  `packages/audit/src/langfuse-client.ts` (103 righe, init lazy,
  hash prompt/response, sessionId/tenantId).
- 6.1 Embeddings + 6.2 RAG: pacchetti `packages/{embeddings,rag}` con
  scheletro funzionale (~700 LOC), serve solo wiring endpoint/Qdrant.

Aggiunto in questo commit:

1. `brain/redaction/__init__.py` + `client.py` (~200 righe): client
   httpx async verso Presidio analyzer/anonymizer. URL letti da env
   (CLAUDE.md §G — no fallback hardcoded). `PresidioUnavailable`
   propagato se servizio down; chiamante decide se bloccare o procedere.
   API top-level: `analyze_text()`, `redact_text()` (one-shot
   analyze+anonymize).

2. `brain/tests/test_redaction.py` (5 test): path felice mocked +
   gestione network error reale verso host invalido.

3. `docs/hybrid-llm-phase6-status.md`: report dettagliato dello stato
   reale di tutte e 5 le sotto-fasi (6.1-6.5) con effort residuo
   stimato (~17-24h totali) e dipendenze. Documento di hand-off per
   chi riprenderà il lavoro.

4. `scripts/smoke-redaction.sh`: validazione syntax + import del nuovo
   modulo (passa).

Verifica:
- `python3 -m py_compile brain/redaction/*.py brain/tests/test_redaction.py`: OK
- Import: tutti i symbol esportati visibili (`DetectedEntity`,
  `PresidioClient`, `PresidioUnavailable`, `RedactionResult`,
  `analyze_text`, `redact_text`).
EOF
)"

git log -1 --stat | head -20
