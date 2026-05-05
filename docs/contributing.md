# Contributing — Workflow IDEAI

Questo documento mappa il ciclo di contribuzione (study -> confirm -> automatic) sui meccanismi concreti del repo (branch, PR, hook).

## 1. Study (esplorazione)

- Fork o branch da `main`: `feature/<nome-breve>`.
- Esplorare la base di codice con `Grep`/`Glob`, non con letture integrali.
- Aprire una PR in stato **draft** appena esistono commit utili: serve come lavagna.
- Nessun requisito di build passata in questa fase, ma niente merge.

## 2. Confirm (convergenza)

- Convertire la PR da draft a **ready for review** solo quando:
  - `pnpm verify` passa in locale (hook pre-commit verde);
  - i test aggiunti sono indipendenti e deterministici;
  - il titolo della PR segue `docs/COMMIT_CONVENTIONS.md`;
  - niente emoji nei commit (controllato da `xtask lint-commits`).
- Revisione umana obbligatoria per qualunque modifica a:
  - `config/policies/`
  - `db/migrations/`
  - `crates/nexus-auth/` e flussi di sensitivity tier.

## 3. Automatic (merge e deploy)

- CI (`.github/workflows/verify.yml`) deve essere verde.
- Merge in `main` con squash; messaggio di squash aderente alle convenzioni.
- Deploy in locale via `./deploy/deploy-local.sh` (o `make deploy`).
- Post-merge: `pnpm xtask lint-commits main~10 main` deve restare pulito.

## Regole rapide

| Tema               | Regola                                                     |
|--------------------|------------------------------------------------------------|
| Commit big         | > 500 righe richiede label `big-refactor` nel messaggio    |
| Emoji              | Vietate in file, commit, changelog, report                 |
| `unwrap`/`expect`  | Solo in `#[cfg(test)]` o `tests/`                          |
| Log sensibili      | Niente `payload`, `prompt`, `response` in chiaro           |
| TS `any`           | `@typescript-eslint/no-explicit-any` = error               |
| Python             | `ruff` + `mypy --strict` in `brain/`                       |
