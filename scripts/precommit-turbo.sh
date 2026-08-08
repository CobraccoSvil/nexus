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
# PATH: la shell degli hook lefthook su Windows puo' non avere cargo/pnpm nel PATH.
export PATH="$HOME/.cargo/bin:$PATH"

# Premessa: turbo invocabile in QUESTO albero. Non sorgiamo gate-env.sh (nessuna
# fase cargo qui, e la sua ragione d'essere e' il fingerprint di Cargo), quindi
# il vocabolario dell'esito viene direttamente dal punto unico.
#
# Senza questa riga, in un worktree senza node_modules `pnpm exec` usciva con un
# "Command not found" e il fail_text dell'hook accusava typecheck/lint: un gate
# mai partito raccontato come codice TypeScript rotto.
# shellcheck source=scripts/gate-premesse.sh
source scripts/gate-premesse.sh
gate_pretende_turbo

exec pnpm exec turbo run typecheck lint --continue
