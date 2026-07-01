#!/usr/bin/env bash
# Hook pre-commit lefthook (turbo_quick) estratto in script.
#
# Perche' uno script e non un comando inline: su Windows lefthook PERDE le
# virgolette annidate di `run: bash -lc "pnpm exec turbo run typecheck lint
# --continue"`, che diventa `bash -lc pnpm exec turbo ...` -> bash esegue solo
# `pnpm` (senza subcomando) e pnpm stampa l'usage con exit 1 (falso fallimento).
# Un path di script singolo, senza quote annidate, e' robusto.
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
exec pnpm exec turbo run typecheck lint --continue
