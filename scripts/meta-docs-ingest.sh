#!/usr/bin/env bash
# scripts/meta-docs-ingest.sh — post-commit: notifica mcp-core dell'ultimo
# commit per aggiornare il meta-vault (regola L: logica in un solo punto,
# invocata da lefthook).
#
# Fail-soft: se mcp-core non e' raggiungibile (dev offline) lo script esce
# comunque 0 e il MetaDocsRefreshWorker recupera al prossimo tick periodico.
#
# In script dedicato (non `run: |` inline) perche' il blocco multi-line di
# lefthook non sopravvive all'esecuzione su git-bash Windows (syntax error).

COMMIT="$(git rev-parse HEAD)"
if curl -sf -m 3 -X POST http://localhost:4000/api/meta-docs/ingest-commit \
     -H "Content-Type: application/json" \
     -d "{\"commit\":\"$COMMIT\"}" >/dev/null; then
  echo "[meta-docs] commit $COMMIT registrato"
else
  echo "[meta-docs] mcp-core non raggiungibile (commit registrato, worker recuperera')"
fi
exit 0
