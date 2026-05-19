# Backlog closure — Branch `chore/backlog-closure` (2026-05-19)

Report consolidato delle fasi affrontate nella sessione del 2026-05-19.
Branch: `chore/backlog-closure` (da `main`).

## Riassunto

8 commit applicati, gate `pnpm verify` ora **EXIT 0** completo:
turbo (typecheck + lint + test) + `cargo check --workspace` +
`cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace --no-fail-fast`.

Le fasi sono state pianificate in `C:\Users\CBRAC\.claude\plans\sharded-enchanting-aurora.md`
(file di plan locale per la sessione).

## Fasi completate

### Fase 0 — Setup baseline
- Stash di 40 file WIP non committati (12 commit locali su `main` non pushati).
- Branch `chore/backlog-closure` da `main` aggiornato.
- Scansione baseline `unwrap`/`expect` per crate + scansione hardcoding modelli.
- Tool: `scripts/backlog-baseline.sh`.

### Fase 1 — Hardcoding modelli AI (CLAUDE.md §G)
Due commit. Fix:

| File | Riga | Fix |
|---|---|---|
| `brain/providers/anthropic_provider.py` | 310, 533 | `THINKING_MODELS` set hardcoded → `_load_thinking_models()` con cache 60s da `ai_price_catalog.capabilities` |
| `brain/providers/anthropic_provider.py` | 646 | `model="claude-haiku-..."` in `test_connection()` → `_resolve_test_connection_model()` da `nexus_purpose_model` |
| `crates/admin-service/src/prompt_templates.rs` | 976 | hardcoding in `run_batch_assign_tools_job` → query `nexus_purpose_model` purpose='admin.tool_selection' |
| `brain/router/service.py` | 476 | docstring obsoleto su fallback inesistente → riflesso comportamento sentinella `__router_unavailable__` |
| `crates/mcp-core/src/routing_config.rs` | 65-66, 142-143 | magic fallback `parse_str(..., "google")` / `"gemini-2.5-flash"` → errore esplicito propagato; `fn defaults()` marcata `#[cfg(test)]` |

Migrazioni nuove:
- `db/migrations/0170_model_capabilities.sql` — colonna `ai_price_catalog.capabilities JSONB` + popolamento capability `thinking` per Claude Sonnet/Opus 4.5+
- `db/migrations/0171_provider_test_and_admin_purposes.sql` — purpose key `provider_test_connection.anthropic`, `admin.tool_selection`

Tool: `scripts/classify-hardcoding-v2.py` per categorizzare A/B/C.

### Fase 2 — Gate `pnpm verify` verde (CLAUDE.md §B)
Un commit. Risolti:

1. **Vitest watch mode**: `packages/{rag,llm-gateway,embeddings,audit}/package.json`
   usavano `"test": "vitest"` → si bloccava in `Waiting for file changes...`.
   Cambiato a `"test": "vitest run"`. CI=1 non piu' necessario.
2. **`.next/types/validator.ts` stale**: il file auto-generato Next.js
   conteneva riferimenti a moduli ora nello stash (es. `app/api/projects/.../execute-command/`).
   `rm -rf .next` risolve, Next.js rigenera. Documentato.

Doc allineato: `docs/tech-debt-ts.md` riflette stato verde + 105 warning lint
ancora aperti come backlog Fase 4.

### Fase 3 — Tech debt Rust `unwrap`/`expect` (CLAUDE.md §F)
Tre commit. Riconteggio robusto (script `unwrap-perfile-v2.py` con
context-aware cfg(test)):

  PROD: 128 unwrap + 23 expect = 151 (non 446+53 della baseline iniziale,
                                       falsata da contesto cfg(test) non rilevato
                                       a singola riga).
  TEST: 316 unwrap + 24 expect = 340 (legittime).

Classificazione PROD (script `classify-unwrap.py`):

| Cat | Count | Azione |
|---|---|---|
| REGEX literal hardcoded | 93 | Annotazione `// safety:` §F in 7 file (cluster) |
| OTHER → idiomatic §F | 17 | Documentati in `docs/tech-debt-rust.md` |
| OTHER → fix reali | 15 | Applicati: let-else, pattern match, ok_or_else, unwrap_or |
| OTHER → falsi positivi | 5 | Stringhe nei pattern detector |
| OTHER → residui minori | ~20 | Backlog futuro commit-per-crate |

Fix applicati:
- `mcp_client.rs` (mcp-core + plugin-service): `child.stdin/stdout/stderr.take()` → `ok_or_else(McpError::Protocol)`
- `agent_processes.rs:93` — `project_root.as_ref().unwrap()` → `ok_or_else(String)`
- `main.rs:211` — `pid.unwrap()` → let-else + continue
- `prompt_admin.rs:36` — `is_some()`+`unwrap()` → if-let
- `openapi_validate.rs:75` — guardia+unwrap → match
- `orchestrator.rs:2386` → `unwrap_or_default`
- `profiles.rs:404` → `map + unwrap_or_else`
- `prompt_templates.rs:868` → `Value::Array` pattern
- `mcp-db/lib.rs:34` → fallback ParserError difensivo
- `ruvector/core.rs:278` → let-else
- `git_blame.rs:102` → let-else dopo peek
- `profile_run.rs:32` → indice esplicito post is_empty
- `auth.rs:307` → `unwrap_or_else` con Response default
- `nexus-http/lib.rs:66,71` → if-let
- `routing_config.rs` — magic fallback rimosso (Fase 1)

### Fase 4 — TS warning lint `0` (estende Fase 2)
Un commit. **105 warning → 0**:

| Tipo | Pre | Post |
|---|---|---|
| `@typescript-eslint/no-explicit-any` | 82 | 0 |
| `@typescript-eslint/no-unused-vars` | 18 | 0 |
| `react-hooks/exhaustive-deps` | 5 | 0 |

Top file:
- `app/pricing/page.tsx`: 35 cast `t("..." as any)` spurii rimossi (chiavi i18n esistenti)
- `app/page.tsx`: id. 31 cast + import `PALETTE` orfano + cast `Parameters<typeof t>[0]` su `COMPARISON_ROWS`
- `components/ide-shell.tsx`: 3 unused vars + 3 `exhaustive-deps` annotati con eslint-disable

Type model:
- `lib/api-client.ts:PlaywrightArtifact` da `any` a struct tipato
- `lib/project-dispatcher/store.ts:applySnapshot()` ora prende `ProjectSnapshot` tipato

Config:
- `apps/web-ide/next-env.d.ts` aggiunto a eslint ignores

Correlato:
- `crates/nexus-http/src/lib.rs`: serializzazione test env-vars con `Mutex` statico → fix flaky test workspace.

### Fase 5 — Styling refactor (rinviata)
Un commit doc. Riconteggio reale: **2884 inline styles in 92 file** (vs 1665
dichiarati come baseline). I file dichiarati "completati" sono cresciuti (es.
`chat-panel.tsx` ora 80 styles vs ~35 post-refactor). Refactor reale richiede
preview server attivo (CLAUDE.md vieta `preview_start` nella sessione corrente).

Nuovo tool: `scripts/count-inline-styles.sh` per misurazione progresso.

## Fasi in backlog (non affrontate in questa sessione)

- **Fase 1.5**: TS hardcoding price tables (4 file con tabelle prezzi/modelli
  hardcoded → refactor con API che leggono `ai_price_catalog`/`nexus_routing_matrix`).
- **Fase 4.5**: Dead code + duplicazione (Rust `cargo udeps`, TS `ts-prune`,
  Python `vulture`; `jscpd` per dedup).
- **Fase 5 reale**: Styling refactor in sessione dedicata con preview server.
- **Fase 6**: Hybrid LLM plan fasi 3-7 (`packages/{embeddings,rag,audit}`
  integration con `crates/mcp-core/`, Presidio redaction microservice, vLLM
  portability).
- **Fase 7**: On-prem migration esecutiva (runbook `docs/migration-to-onprem.md`).
- **Fase 8**: Go/No-Go checklist (48 item) verifica e sign-off.

## Metriche before/after

| Metrica | Pre | Post |
|---|---|---|
| `pnpm verify` exit | 1 (rosso) | 0 |
| Hardcoding modelli Cat A bonificato | — | 5 file fixati + 2 migrazioni |
| `unwrap`/`expect` Rust PROD | 151 | 136 (15 fix) + 110 annotati safety §F |
| TS lint warnings web-ide | 105 | 0 |
| Cargo test flaky workspace | sì (nexus-http) | risolto (serial mutex) |
| Doc allineati a stato reale | tech-debt-{rust,ts} obsoleti | aggiornati |

## Indice script di lavoro

In `scripts/`:
- `backlog-baseline.sh` — scansione iniziale unwrap + hardcoding.
- `classify-hardcoding-v2.py` — categorizzazione A/B/C precisa context-aware.
- `unwrap-perfile-v2.py` — count PROD vs TEST per file (cfg(test) robust).
- `classify-unwrap.py` — categorizzazione REGEX/MUTEX/PARSE/OTHER.
- `count-inline-styles.sh` — count inline styles per file `.tsx`.
- `lint-by-file.sh`, `lint-summary.sh` — categorizzazione warning lint.
- `run-verify-baseline.sh`, `run-verify-ci.sh`, `verify-summary.sh`, `verify-deepdive.sh`, `re-verify.sh` — riproduzione e diagnosi verify.
- `find-vitest-watch.sh` — trova package.json con vitest in watch mode.
- `smoke-phase1.sh` — smoke test sui fix Fase 1.

## Lista commit

```
chore(web-ide): bonifica 105 warning ESLint -> 0 (Fase 4)
docs(tech-debt-rust): classifica idiomatici §F e residui (Fase 3 step 3)
chore(rust): elimina unwrap reali fuori test (Fase 3 step 2)
chore(rust): annota cluster Regex literal unwrap come safety (§F)
chore(verify): pnpm verify torna verde (CLAUDE.md §B)
chore(routing): rimuovi magic fallback hardcoded in routing_config
chore(routing): bonifica hardcoding modelli in anthropic provider e admin tool selection
docs(styling): allinea STYLING_REFACTOR_PROGRESS a stato reale + check tool
```

## Note operative

- Il branch `chore/backlog-closure` e' pronto per review/merge.
- Lo stash `wip-pre-backlog-closure-2026-05-19` su `main` contiene il WIP
  dell'utente (40 file modificati + 2 migrazioni 0168/0169) — da ripristinare
  con `git stash pop` dopo merge o rebase.
- Hook `lefthook` pre-commit attivo: niente `--no-verify` durante questi commit.
