#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add apps/web-ide \
        eslint.config.mjs \
        crates/nexus-http/src/lib.rs \
        scripts/lint-summary.sh \
        scripts/lint-by-file.sh \
        scripts/re-verify.sh \
        scripts/commit-phase4.sh

git commit -m "$(cat <<'EOF'
chore(web-ide): bonifica 105 warning ESLint -> 0 (Fase 4)

`pnpm verify` ora chiude con EXIT 0 anche senza CI=1 (i package.json di
packages/{rag,llm-gateway,embeddings,audit} gia' usavano `vitest run`
dalla Fase 2).

Warning categorizzati (script lint-by-file.sh):
- @typescript-eslint/no-explicit-any:  82 → 0
- @typescript-eslint/no-unused-vars:   18 → 0
- react-hooks/exhaustive-deps:          5 → 0
                                     ─────
                                      105 → 0

Top file con concentrazione massima (67/105 = 64%):
- apps/web-ide/app/pricing/page.tsx (35): cast `t("..." as any)` spurii,
  rimossi (le chiavi i18n esistono gia' in `lib/i18n.tsx` come
  `TranslationKey` literal).
- apps/web-ide/app/page.tsx (32): id. + `import PALETTE` orfano rimosso
  + `t(cell as Parameters<typeof t>[0])` su COMPARISON_ROWS (riga in cui
  cell e' tipato `string | boolean` dall'array).
- apps/web-ide/components/ide-shell.tsx (6): `getProviderHealth` import +
  `parseProviderHealth` fn + `providerKeys` var orfani rimossi; tre
  `react-hooks/exhaustive-deps` annotati con eslint-disable e motivazione.
- apps/web-ide/components/landing/{HeroSplit,NavBar}.tsx (4+2): id. i18n.
- apps/web-ide/app/admin/project-database/page.tsx (4): tipo dedicato
  per `nexusDbStats` invece di `any`, mapping con `arr.length` invece di
  bang-non-null, `value` `unknown` → `String(value ?? "—")`.

Restanti: 5 catch-orfani in app/api/neural/providers/*/health/route.ts
(`catch (error)` → `catch {}`), 1 fix simile in save-screenshot, 1 in
api/neural/health (`error: any` → `unknown` + instanceof Error narrow),
2 unused-vars in chat-panel/message-list/output-panel/security-panel/
bottom-panel-manager/plugin-manager, 2 react-hooks/exhaustive-deps in
lib/use-chat.ts.

Type model rivisto invece di mascherare con `any`:
- `lib/api-client.ts:PlaywrightArtifact` → struct con `path/kind/name`.
- `lib/project-dispatcher/store.ts:applySnapshot(snapshot)` ora prende
  un nuovo type `ProjectSnapshot` (campi opzionali, tolerante a versioni
  server diverse).

ESLint config:
- `apps/web-ide/next-env.d.ts` aggiunto agli ignores: file generato da
  Next.js che usa `<reference path=...>` (vietato dalla regola
  `@typescript-eslint/triple-slash-reference`); il file dice
  esplicitamente "should not be edited".

Correlato (Fase 3 polish):
- `crates/nexus-http/src/lib.rs`: serializzati i 3 test del modulo che
  mutano env vars (`NEXUS_HTTP_TIMEOUT_SECS`, `NEXUS_PROXY`) con un
  `Mutex` statico. Risolto il fallimento flaky di
  `cargo test --workspace --no-fail-fast` che dipendeva dall'ordine di
  esecuzione parallelo.

Tool aggiunti in scripts/ per categorizzare e diagnosticare warning
(lint-summary, lint-by-file, re-verify).
EOF
)"

git log -1 --stat | head -50
