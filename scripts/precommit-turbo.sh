#!/usr/bin/env bash
# Pre-commit veloce: typecheck + lint via turbo.
# Estratto da lefthook.yml perche' su Windows lefthook word-splitta la stringa
# del campo 'run' ignorando le virgolette: 'bash -lc "pnpm exec turbo ..."'
# diventava 'bash -lc pnpm exec turbo ...', quindi bash riceveva come command
# string solo "pnpm", eseguiva una shell vuota e ritornava 0 (falso-verde, il
# turbo non girava mai). Invocando 'bash scripts/precommit-turbo.sh' lefthook
# passa due soli token e nessuna interpolazione puo' corrompere il comando.
set -euo pipefail
cd "$(dirname "$0")/.."
exec pnpm exec turbo run typecheck lint --continue
