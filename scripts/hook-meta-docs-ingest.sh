#!/usr/bin/env bash
# Hook post-commit lefthook (meta_docs_ingest) estratto in script.
#
# Notifica mcp-core del nuovo commit cosi' il MetaDocsRefreshWorker aggiorna il
# meta-vault. Fail-soft: se mcp-core non e' raggiungibile (dev offline) lo script
# esce comunque 0 e il commit resta valido; il worker recupera al tick periodico.
#
# Perche' uno script e non un comando inline: su Windows Git Bash corrompe il
# comando lefthook multilinea (continuazioni con \ e quote annidate del corpo
# JSON), che finisce interrotto -> "syntax error: unexpected end of file" con
# exit 2 (falso fallimento). Un path di script singolo e' robusto (stesso motivo
# di hook-cargo-check.sh / check-no-emoji.sh).
set -uo pipefail

COMMIT="$(git rev-parse HEAD)"
URL="http://localhost:4000/api/meta-docs/ingest-commit"

if curl -sf -m 3 -X POST "$URL" \
     -H "Content-Type: application/json" \
     -d "{\"commit\":\"$COMMIT\"}" > /dev/null 2>&1; then
  echo "[meta-docs] commit $COMMIT registrato"
else
  echo "[meta-docs] mcp-core non raggiungibile (commit registrato, worker recuperera')"
fi

exit 0
