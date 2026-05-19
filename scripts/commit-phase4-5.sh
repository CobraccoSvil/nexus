#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add -u apps/web-ide/lib/use-tracked-api.ts \
           apps/web-ide/lib/use-api-with-abort.ts \
           apps/web-ide/lib/PENDING_OPERATIONS.md \
           apps/web-ide/components/monitoring-panel.tsx \
           apps/web-ide/components/source-control-panel.tsx
git add scripts/dead-code-rust.sh \
        scripts/commit-phase4-5.sh

git commit -m "$(cat <<'EOF'
chore(web-ide): rimuovi file orfani (Fase 4.5 dead code)

Scansione con `pnpm dlx ts-prune -p apps/web-ide/tsconfig.json` ha
identificato i seguenti file con zero import esterni e nessuna
dichiarazione di entry-point Next.js:

- `apps/web-ide/lib/use-tracked-api.ts` — hook mai consumato
- `apps/web-ide/lib/use-api-with-abort.ts` — hook mai consumato
- `apps/web-ide/lib/PENDING_OPERATIONS.md` — doc che descriveva i due
  hook sopra (orfana dopo la rimozione)
- `apps/web-ide/components/monitoring-panel.tsx` — componente mai importato
- `apps/web-ide/components/source-control-panel.tsx` — re-export shim
  verso `./git/source-control-panel.tsx`; `sidebar-manager` importa gia'
  direttamente la posizione canonica.

Cancellati. Typecheck e lint restano verdi.

Altri unused export segnalati ma NON rimossi (richiedono analisi caso per
caso o sono falsi positivi):
- Tutti i `default` di pagine Next.js (`page.tsx`, `layout.tsx`),
  `middleware`/`config` di `middleware.ts`, `metadata` esportato — sono
  convention obbligatorie del framework.
- `lib/text-utils.ts:{getTruncateProps,getTruncatePropsFull,getTruncateTitle}` —
  referenziati come esempio in `STYLING_GUIDE.md`. Lascio per refactor
  styling futuro.
- `lib/styles.ts:{buttonStyles,inputStyle,cardStyle}` — helper styling
  potenzialmente utili dopo Fase 5.
- `lib/format.ts:shortenAbsolutePathCompact` — utility puntuale.
- `components/landing/Band.tsx:PALETTE` — gia' rimosso un consumer orfano
  in Fase 4 (commit fcaf656), il symbol resta come parte dell'API del
  componente landing.

Scansione Rust:
- `cargo clippy --workspace --all-targets -- -W dead_code -W unused_imports`
  produce **0 warning** dead_code/unused_imports (tool: `scripts/dead-code-rust.sh`).
  Il workspace Rust e' gia' pulito.

Scansione Python e dedup (jscpd):
- `vulture`/`pyflakes`/`jscpd` non installati globalmente. Documento come
  tech-debt; richiedono pip/npm install che esula scope di questo commit.
EOF
)"

git log -1 --stat
