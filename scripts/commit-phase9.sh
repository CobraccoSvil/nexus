#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add docs/backlog-closure-2026-05-19.md \
        docs/architecture/overview.md \
        README.md \
        scripts/commit-phase9.sh

git commit -m "$(cat <<'EOF'
docs(backlog-closure): report sessione 2026-05-19 + sync overview e README

Nuovo documento `docs/backlog-closure-2026-05-19.md` consolida tutte le
fasi affrontate nella sessione:

- Fase 0: baseline (stash WIP, branch, scansione iniziale)
- Fase 1: hardcoding modelli (CLAUDE.md §G) — 5 file + 2 migrazioni
  (0170 ai_price_catalog.capabilities, 0171 purpose_model nuove voci)
- Fase 2: gate `pnpm verify` verde — 4 package.json (vitest run) +
  .next cleanup
- Fase 3: tech-debt Rust §F — 15 fix reali + 93 regex annotati + 17
  idiomatici documentati centralmente
- Fase 4: TS lint 105 → 0 warning
- Fase 5: doc allineamento styling (refactor reale rinviato — preview)
- Backlog residuo: Fase 1.5, 4.5, 6, 7, 8 documentati

Aggiornamenti correlati:
- `docs/architecture/overview.md`: aggiunto principio "no magic fallback"
  + sezione "Pacchetti TypeScript hybrid LLM" con stato reale dei
  pacchetti embeddings/rag/audit/llm-gateway.
- `README.md`: nuova sezione "Backlog closure recente" con i puntatori
  ai documenti.

Il report serve da hand-off documentale per chiunque debba riprendere
il lavoro dalle fasi residue (in particolare 5 styling, 6 hybrid 3-7,
7 on-prem, 8 go/no-go).

Metriche before/after consolidate:
  pnpm verify exit:          1 → 0
  unwrap Rust PROD:        151 → 136 (15 fix) + 110 annotati safety §F
  TS lint warnings web-ide: 105 → 0
  cargo test workspace flaky: si → no (mutex statico nexus-http test)
EOF
)"

git log -1 --stat | head -20
