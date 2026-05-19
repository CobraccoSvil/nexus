# Tech debt — TypeScript

Backlog delle violazioni dogfood nel codice TS/TSX.

## Scansione base

```bash
pnpm exec eslint . --ext .ts,.tsx --rule '@typescript-eslint/no-explicit-any: error' > eslint-any.log
pnpm exec tsc --noEmit --strict > tsc-strict.log
```

## Regole attivate

- [x] `tsconfig.base.json` ha `"strict": true` (già presente).
- [ ] `apps/web-ide/tsconfig.json` — ereditarietà strict confermata.
- [~] `eslint.config.mjs` — regola `@typescript-eslint/no-explicit-any: warn` (salire a `error` dopo pulizia).

## Lint errori bloccanti — RISOLTI (2026-05-19)

`pnpm verify` ora chiude con **exit 0** (Fase 2 del backlog di chiusura,
branch `chore/backlog-closure`):

- `apps/web-ide` lint: 105 warning residui ma **0 errori** — eslint non
  configurato con `--max-warnings 0`, quindi non bloccanti. Vedi sezione
  `any` qui sotto per il piano di bonifica.
- Typecheck web-ide passava già: il fail osservato in precedenza era dovuto
  a `.next/types/validator.ts` stale (riferiva un modulo `execute-command/route.js`
  che esisteva solo in un WIP non committato). Soluzione: rebuild dopo `rm -rf .next`.
- `packages/{rag,llm-gateway,embeddings,audit}/package.json` ora usano
  `vitest run` invece di `vitest` per evitare watch-mode in CI/verify
  (causa di blocco indefinito senza `CI=1`).
- cargo check + cargo clippy `-D warnings` + cargo test workspace: **OK**.

## `any` da rimuovere

Compilare con `path:riga — nota`.

- [ ] `apps/web-ide/` — scan da eseguire
- [ ] `packages/llm-gateway/` — scan da eseguire
- [ ] `packages/shared/` — scan da eseguire

## Test Vitest aggiunti

- [x] `apps/web-ide` — test d'esempio per chat panel (stub)
