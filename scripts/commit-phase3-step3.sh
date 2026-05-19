#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add docs/tech-debt-rust.md \
        scripts/commit-phase3-step3.sh

git commit -m "$(cat <<'EOF'
docs(tech-debt-rust): classifica idiomatici §F e residui (Fase 3 step 3)

Riscrittura di `docs/tech-debt-rust.md` per riflettere lo stato reale
post-Fase 3 step 1+2:

- Baseline corretta: 128 unwrap + 23 expect PROD (non 446+53 della scansione
  iniziale, falsata da contesto cfg(test) non rilevato a singola riga).
- Cluster Regex literal §F: 6 file annotati con commento di safety.
- Idiomatici §F documentati centralmente invece che con commento inline su
  17 file separati: env bootstrap (5 servizi + mcp-core), tokenizer init,
  lock poisoned, SHA256 try_into, parse literal compile-time, reqwest
  builder, time valid.
- Fix reali applicati: 15, elencati con strategia (let-else, pattern,
  error propagation).
- Falsi positivi del classifier: 5 (stringhe in pattern detector).
- Residui minori: ~20, da affrontare commit-per-crate.

Il documento adesso e' la fonte autoritativa per "perche' questo unwrap
non e' stato fixato". Quando il refactor LazyLock<Regex> verra' fatto,
elimina le annotazioni di safety dai 6 file e aggiorna la sezione
"refactor opportuno".
EOF
)"

git log -1 --stat
