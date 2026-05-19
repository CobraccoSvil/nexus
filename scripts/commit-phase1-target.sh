#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add brain/providers/anthropic_provider.py \
        brain/router/service.py \
        crates/admin-service/src/prompt_templates.rs \
        db/migrations/0170_model_capabilities.sql \
        db/migrations/0171_provider_test_and_admin_purposes.sql \
        scripts/backlog-baseline.sh \
        scripts/smoke-phase1.sh

git commit -m "$(cat <<'EOF'
chore(routing): bonifica hardcoding modelli in anthropic provider e admin tool selection

Rimuove tre violazioni nette di CLAUDE.md G (registry DB unica fonte modelli):

- brain/providers/anthropic_provider.py: THINKING_MODELS set hardcoded
  (linee 310, 533) e claude-haiku in test_connection() (linea 646) sono
  ora risolti via DB. THINKING_MODELS legge ai_price_catalog.capabilities
  con cache 60s e ThinkingModelsUnavailable esplicito se DB irraggiungibile.
  test_connection() risolve il modello da nexus_purpose_model.

- crates/admin-service/src/prompt_templates.rs: claude-haiku hardcoded
  in run_batch_assign_tools_job (riga 976) sostituito con lookup
  nexus_purpose_model purpose='admin.tool_selection' (mig 0171).

- brain/router/service.py: docstring di decide() corretto — descriveva un
  fallback (openai, gpt-4.1-mini) che non esiste piu' nel codice (e
  sarebbe stata una violazione G).

Migrazioni:
- 0170_model_capabilities.sql: aggiunge ai_price_catalog.capabilities
  (JSONB) e popola capability 'thinking' per Sonnet/Opus 4.5+.
- 0171_provider_test_and_admin_purposes.sql: inserisce due purpose key
  ('provider_test_connection.anthropic', 'admin.tool_selection') che
  consentono di cambiare il modello via admin senza redeploy.

Tool di lavoro aggiunti in scripts/:
- backlog-baseline.sh — scansione baseline unwrap/expect + hardcoding.
- smoke-phase1.sh — smoke import Python + cargo check + lint SQL.
EOF
)"

git log -1 --stat
