#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add scripts/go-nogo-audit.sh \
        docs/go-nogo-audit-2026-05-19.md \
        scripts/commit-phase8.sh

git commit -m "$(cat <<'EOF'
chore(go-nogo): audit statico checklist 48 item (Fase 8)

Implementato `scripts/go-nogo-audit.sh` che verifica meccanicamente per
ogni item di `docs/go-nogo-checklist.md` se l'artefatto referenziato
esiste nel repo. Categorizza in:

  [ OK ]   artefatto presente (auto-verificato)
  [DEPLOY] richiede target attivo (test live / benchmark / infra)
  [HUMAN]  richiede review o firma (DPIA, DPA, playbook legale)
  [MISS]   artefatto mancante (regressione)

Risultato esecuzione sul branch:
  27 OK + 14 deploy-bound + 4 human-bound + 0 missing = 45 item

Nessun MISSING: tutti gli artefatti citati dalla checklist (RLS policies,
audit_llm_calls schema, DLP scanner, redaction client, LangfuseTracer,
backup script, load-test k6, etc.) esistono nel codice.

I 4 veti critici (B1 red team, C1 RLS, A1 cross-profile, H4 DPA) hanno
tutti artefatto presente — restano da chiudere via esecuzione live o
firma legal.

Report dettagliato in `docs/go-nogo-audit-2026-05-19.md`: tabella per
item OK + categorie deploy/human + prossimi passi pre-go-live.

Il Tech Lead ora ha un check riproducibile per:
- accertarsi che nessun artefatto sia regredito (riesegui audit prima
  del rilascio),
- tracciare il passaggio dei deploy-bound a OK quando i test live girano,
- mappare i 4 human-bound come pending nel modulo firma checklist.
EOF
)"

git log -1 --stat
