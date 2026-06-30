#!/usr/bin/env bash
# Post-commit: notifica mcp-core per aggiornare il meta-vault. Fail-soft.
# Estratto da lefthook.yml perche' il blocco 'run: |' multilinea con continuazioni
# di riga '\' veniva word-splittato/troncato da lefthook su Windows
# ("-c: line 4: syntax error: unexpected end of file"): lo script girava in una
# shell malformata. Uno script dedicato gira in una shell reale e completa.
# Fail-soft per scelta: se mcp-core e' offline (dev) il commit e' gia' avvenuto e
# il MetaDocsRefreshWorker recuperera' al prossimo tick periodico -> niente set -e.
set -uo pipefail
cd "$(dirname "$0")/.."

COMMIT=$(git rev-parse HEAD)
if curl -sf -m 3 -X POST http://localhost:4000/api/meta-docs/ingest-commit \
     -H "Content-Type: application/json" \
     -d "{\"commit\":\"$COMMIT\"}" > /dev/null 2>&1; then
  echo "[meta-docs] commit $COMMIT registrato"
else
  echo "[meta-docs] mcp-core non raggiungibile (commit registrato, worker recuperera')"
fi
exit 0
