#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add packages/rag/package.json \
        packages/llm-gateway/package.json \
        packages/embeddings/package.json \
        packages/audit/package.json \
        docs/tech-debt-ts.md \
        scripts/run-verify-baseline.sh \
        scripts/run-verify-ci.sh \
        scripts/verify-summary.sh \
        scripts/verify-deepdive.sh \
        scripts/find-vitest-watch.sh \
        scripts/unwrap-perfile.sh \
        scripts/commit-phase2.sh

git commit -m "$(cat <<'EOF'
chore(verify): pnpm verify torna verde (CLAUDE.md §B)

Il gate `pnpm verify` (CLAUDE.md §B) era rosso da settimane. Cause
identificate e risolte:

1. `vitest` in watch mode bloccava turbo. I 4 pacchetti
   `packages/{rag,llm-gateway,embeddings,audit}` usavano `"test": "vitest
   --config tests/vitest.config.ts"` che resta in `Waiting for file
   changes...` indefinitamente quando lanciato senza CI=1. Cambiato a
   `vitest run` (single-pass) — rende il comportamento deterministico in
   ogni ambiente.

2. `.next/types/validator.ts` poteva contenere riferimenti stale a moduli
   rimossi (es. directory `app/api/projects/[id]/execute-command/`
   scomparsa dopo uno stash o branch switch). Risolto manualmente con
   `rm -rf apps/web-ide/.next`; Next.js rigenera al prossimo build.
   `docs/tech-debt-ts.md` ora documenta la situazione e i 105 warning lint
   residui (0 errori, non bloccanti — il piano di bonifica `any` resta
   aperto come Fase 4).

Stato dopo questa fase:
- `pnpm verify` exit 0 in locale (CI=1 o senza, dopo questo commit).
- 105 lint warnings ancora in web-ide (no `@typescript-eslint/no-explicit-any`
  come error) — vengono affrontati in Fase 4.
- cargo check / clippy -D warnings / test workspace passano.

Tool aggiunti in scripts/ per riprodurre e diagnosticare il verify
(run-verify-baseline.sh, run-verify-ci.sh, verify-summary.sh,
verify-deepdive.sh, find-vitest-watch.sh) e per mappare l'unwrap-debt
Rust file-by-file (unwrap-perfile.sh, utile alla Fase 3).
EOF
)"

git log -1 --stat
