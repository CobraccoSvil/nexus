#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add docs/backlog-closure-2026-05-19.md scripts/commit-doc-update.sh

git commit -m "$(cat <<'EOF'
docs(backlog-closure): aggiorna report turno 2 chiude fasi 1.5/4.5/6/7/8

Aggiunte sezioni con tabella commit (1.5 model-catalog, 4.5 file orfani,
6 redaction client, 7 onprem preflight, 8 go-nogo audit) e link ai
report dettagliati delle 4 fasi che richiedono follow-up
(hybrid-llm-phase6-status, onprem-dry-run, go-nogo-audit).
EOF
)"
