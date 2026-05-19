#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add STYLING_REFACTOR_PROGRESS.md \
        scripts/count-inline-styles.sh \
        scripts/commit-phase5.sh

git commit -m "$(cat <<'EOF'
docs(styling): allinea STYLING_REFACTOR_PROGRESS a stato reale + check tool

Il documento STYLING_REFACTOR_PROGRESS.md dichiarava "446/1665 stili
ridotti (27%), 9/75 file completati" da una sessione del 2025-04-20.
Riconteggio con script `scripts/count-inline-styles.sh`:

  Totale inline styles attualmente:  2884 in 92 file .tsx

I file dichiarati "completati" sono cresciuti:
  - chat-panel.tsx:           80 inline styles (post-refactor era ~35)
  - ide-shell.tsx:             70                            ~38)
  - routing-config.tsx:        91                            ~45)
  - plugin-manager.tsx:        84                            ~48)
  - infrastructure-settings:   79                            ~45)

Cause: feature aggiunte dopo il refactor reintroducono inline styles.
Senza un lint custom che blocchi pattern gia' coperti da utility, il
debt rigenera.

Aggiornamenti al documento:
- Numeri reali (2884 / 92 file) sostituiscono i dichiarati storici.
- Top 15 file per concentrazione (45% del totale).
- Strategia operativa rivista: refactor styling richiede preview
  visivo, CLAUDE.md vieta `preview_start` ("Tutto gira in locale su WSL").
- Raccomandazione: regola di review + lint custom + check periodico in
  CI via `count-inline-styles.sh`.

Fase 5 del piano `chore/backlog-closure` resta APERTA: richiede una
sessione dedicata con browser di sviluppo locale per validare i
refactor visivamente. Batch consigliato: 5-10 file per commit dei top
file (project-db-panel 118, admin/prompts/page 117, run-panel 101,
source-control-panel 99, sidebar-manager 94, profiles/page 88).

Nuovo tool: `scripts/count-inline-styles.sh` per misurare progresso.
EOF
)"

git log -1 --stat | head -20
