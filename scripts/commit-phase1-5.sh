#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add apps/web-ide/lib/model-catalog.ts \
        apps/web-ide/lib/context-window.ts \
        apps/web-ide/components/chat/inline-trace-panel.tsx \
        apps/web-ide/components/panels/ai-trace-panel.tsx \
        apps/web-ide/components/chat/profile-editor.tsx \
        scripts/commit-phase1-5.sh

git commit -m "$(cat <<'EOF'
chore(web-ide): unifica catalog modelli AI in lib/model-catalog.ts (Fase 1.5)

Quattro file frontend duplicavano dati hardcoded di prezzi, context window
e liste modelli per provider — violazione CLAUDE.md §G (registry DB unica
fonte) con sintomo aggiuntivo di copy-paste integrale tra
`inline-trace-panel.tsx` e `ai-trace-panel.tsx` (27 modelli identici, stesso
`calcCost`).

Refactor: introdotto `apps/web-ide/lib/model-catalog.ts` come single source
frontend:

- `MODEL_PRICING` + `calcModelCost()` + `formatCostUsd()` —
  consumati da inline-trace-panel.tsx e ai-trace-panel.tsx (le due
  funzioni `calcCost` erano identiche modulo formatting).
- `MODEL_CONTEXT_WINDOW` + `fallbackContextWindow()` — `context-window.ts`
  ora e' un compatibility shim re-export.
- `PROVIDER_MODELS` — consumato da profile-editor.tsx (dropdown).

Il nuovo modulo documenta esplicitamente che e' una deroga temporanea a
§G in attesa di un endpoint API che proxa `ai_price_catalog` /
`nexus_routing_matrix` / `nexus_purpose_model`. Quando la migrazione sara'
fatta, il file potra' essere cancellato (tutti i callsite usano un solo
import; cambiare la fonte significa solo riscrivere quel modulo).

`routing-config.tsx` (display informativo della routing matrix) NON e'
stato toccato: e' una tabella documentale che mostra all'utente cosa fara'
il sistema; non controlla il routing reale (autoritativo: nexus_routing_matrix
via Rust). Allineamento al DB richiederebbe un endpoint dedicato e un loading
state — fuori scope di questo commit.

Verifica:
- `pnpm --filter @ai-orchestrator/web-ide typecheck`: exit 0
- `pnpm --filter @ai-orchestrator/web-ide exec eslint .`: 0 errori 0 warning
EOF
)"

git log -1 --stat
