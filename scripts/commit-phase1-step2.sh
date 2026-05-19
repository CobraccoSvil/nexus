#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add crates/mcp-core/src/routing_config.rs \
        scripts/classify-hardcoding.sh \
        scripts/classify-hardcoding-v2.py \
        scripts/commit-phase1-target.sh \
        scripts/commit-phase1-step2.sh

git commit -m "$(cat <<'EOF'
chore(routing): rimuovi magic fallback hardcoded in routing_config

`fetch_thresholds_from_db` aveva due magic fallback `parse_str(...,
"google")` e `parse_str(..., "gemini-2.5-flash")` per le chiavi
`routing.classifier_provider` / `routing.classifier_model`. CLAUDE.md §G
vieta esplicitamente questo pattern: se la config manca il sistema deve
fallire visibilmente, non degradare silenziosamente al modello sbagliato.

- Le due chiavi ora propagano errore esplicito che riferisce mig 0111.
- `fn defaults()` annotata `#[cfg(test)]`: era usata solo dal test che
  verifica che i default Rust coincidano col seed migrazione, non da
  produzione (verificato con grep).
- Rimosso il closure `parse_str` ormai morto.

Script di lavoro aggiunti in scripts/ per la classificazione hardcoding.
EOF
)"

git log -1 --stat
