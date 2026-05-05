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

## Lint errori pre-esistenti bloccanti (da risolvere prima di rendere `verify` un gate hard)

Lista rilevata con `pnpm verify` su `feature/dogfood-directives`:

- `apps/web-ide/app/layout.tsx` — due `@ts-ignore` da convertire in `@ts-expect-error`.
- `apps/web-ide/components/chat-panel.tsx:370` e altri file settings/* — regola
  `react-hooks/exhaustive-deps` non trovata (plugin `eslint-plugin-react-hooks`
  non configurato): caricare il plugin o rimuovere il riferimento.
- `apps/web-ide/components/panels/optimization-panel.tsx:625` —
  `no-unused-expressions`.
- `apps/web-ide/server.js` — 4 `require()` CommonJS: migrare a ESM o aggiungere
  override `eslint` per `*.js`.

Finché questi errori esistono, CI `verify.yml` fallirà in fase lint. Trattare
come issue prioritaria Fase 2.

## `any` da rimuovere

Compilare con `path:riga — nota`.

- [ ] `apps/web-ide/` — scan da eseguire
- [ ] `packages/llm-gateway/` — scan da eseguire
- [ ] `packages/shared/` — scan da eseguire

## Test Vitest aggiunti

- [x] `apps/web-ide` — test d'esempio per chat panel (stub)
