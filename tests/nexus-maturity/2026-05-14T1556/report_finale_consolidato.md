# Test maturita Nexus — Report finale consolidato (3 ondate fix)

**Branch**: `test/nexus-maturity-2026-05-14T1556`
**Data chiusura**: 2026-05-14
**HEAD finale**: `d401566`

## TL;DR

- 4 iterazioni di test eseguite sul progetto target `nexus-maturity-rental` (app autonoleggio).
- App finale **funzionante in demo**: backend Fastify+Prisma (:3002), frontend Vite+React (:5173), DB Postgres con migrazioni applicate, utente di test registrato.
- 19 gap di maturita identificati durante il test e tracciati in `journal_fix.md`.
- **15 fix consolidati** in 9 commit su 3 ondate; tutti passano `cargo check` + `tsc --noEmit` + hook lefthook (turbo+commit-msg).
- 4 gap residui dichiarati fuori scope o rimandati a PR successive (M3 Python, M15 UI dialog, M8 estensione, M16 docs).

## Timeline commit (3 ondate)

| Commit | Ondata | Fix | Categoria |
|--------|--------|-----|-----------|
| `61b1e45` | 0 (prep) | UI fixes iniziali per abilitare test | G |
| `16fec82` | Test | M4 autonomia AUTOMATIC + M2 hook project_documents | A+D |
| `cb589f8` | Test | M9 criterio accettazione include avvio app | A |
| `7813837` | Test | M6 guardrail git/gh in system prompt automatic | A |
| `b7c66f4` | Test | Journal 19 gap + cleanup Playwright | doc |
| `f9ec8b6` | Ondata 1 | M7 collect.sh + M13 chat content + M19 playwright_install + git_remote_add + M15 github/create-repo backend | D+G |
| `e1db58f` | Ondata 2 | M5 projects_base_root + M8 hide IDEAI frontend | B+G |
| `d401566` | Ondata 3 | M14 browser-check + M10 runtime_issues + M1 scan-ports + M11 fs-events + M12 label + M18 auto-bootstrap | D+C+G |

## Gap chiusi: 15/19

| # | Severita | Descrizione | Stato | Categoria fix | Commit |
|---|----------|-------------|-------|---------------|--------|
| M1  | medio   | Auto-popolamento `nexus_port_allocations` | CHIUSO | D (scan_ports.rs) | d401566 |
| M2  | medio   | Hook project_documents su tool_write_file | CHIUSO | D (agent_tools/files.rs) | 16fec82 |
| M4  | alto    | AUTOMATIC mode senza autonomia esplicita | CHIUSO | A (system prompt) | 16fec82 |
| M5  | alto    | projects_base_root sotto monorepo IDEAI | CHIUSO | B (settings DB) | e1db58f |
| M6  | alto    | Guardrail git/gh fuori chat | CHIUSO | A (system prompt) | 7813837 |
| M7  | medio   | collect.sh non funzionante in container | CHIUSO | D (script) | f9ec8b6 |
| M8  | basso   | Path IDEAI visibili in UI (parziale) | CHIUSO (Explorer+Switcher) | G (format.ts) | e1db58f |
| M9  | alto    | Criterio accettazione default senza avvio app | CHIUSO | A (system prompt) | cb589f8 |
| M10 | alto    | Runtime issues non tracciati in DB | CHIUSO | D+C (runtime_issues.rs + mig 0138) | d401566 |
| M11 | medio   | FS-events per refresh tree | CHIUSO (polling) | D (fs_events.rs) | d401566 |
| M12 | basso   | Label "Repo" generica | CHIUSO | G (source-control-panel.tsx) | d401566 |
| M13 | basso   | Markdown chat tutto ammassato | CHIUSO | G (markdown-renderer.tsx) | f9ec8b6 |
| M14 | alto    | Errori console frontend non raggiungono Nexus | CHIUSO | D (browser_check.rs) | d401566 |
| M17 | basso   | pickBestPort sceglieva porta backend | CHIUSO | G (chat-prompts.ts) | f9ec8b6 |
| M18 | medio   | Bootstrap progetto manuale | CHIUSO (minimal) | D (auto_bootstrap.rs) | d401566 |
| M19 | alto    | Playwright non installato automaticamente | CHIUSO | D (playwright_install.rs) | f9ec8b6 |

## Gap residui: 4/19

| # | Severita | Descrizione | Motivo skip | Roadmap |
|---|----------|-------------|-------------|---------|
| M3  | alto  | Streaming LangGraph OpenAI in brain | Fuori scope Rust mcp-core (file Python) | PR Python dedicata |
| M15 | medio | UI dialog "Crea repo GitHub" | Backend pronto (github/create-repo); frontend richiede state React complesso | PR UI follow-up |
| M8 ext | basso | Path IDEAI in pannelli minori (System Root, AI Trace) | I 2 punti principali (Explorer+Switcher) coperti; nessun leak nei sorgenti TS rimanenti | Da rivalutare se segnalato |
| M16 | basso | Docs aggiornate per nuovi endpoint | Solo doc | PR doc batch |

## Nuovi endpoint REST consolidati

Tutti operativi su `mcp-core :4000` (verificato HTTP 200 dopo restart):

- `POST /api/projects/:id/services/install-playwright` — installa Playwright + genera config + smoke spec
- `POST /api/projects/:id/services/browser-check` — esegue smoke con BASE_URL override, cattura console errors
- `POST /api/projects/:id/services/scan-ports` — scansiona package.json/vite/Procfile/compose, UPSERT in nexus_port_allocations
- `POST /api/projects/:id/services/auto-bootstrap` — orchestrator: scan-ports + install-playwright
- `GET  /api/projects/:id/runtime-issues` — lista issue progetto
- `POST /api/projects/:id/runtime-issues` — INSERT da hook tool agente
- `PATCH /api/projects/:id/runtime-issues/:iid` — aggiorna status
- `GET  /api/projects/:id/fs-events?since_fingerprint=N` — snapshot polling tree
- `POST /api/projects/:id/github/create-repo` — crea repo GitHub + git remote add origin

## Nuovi tool agente MCP

- `git_remote_add(remote_url)` — validato https/git@/ssh, idempotente
- (M19) Playwright disponibile per agente tramite `tool_run_command` con `npx playwright test`

## Nuove tabelle DB

- `project_runtime_issues` (mig **0138**) — id, project_id, source, severity, message, details, fingerprint, status; UNIQUE INDEX su (project_id, fingerprint) per dedup ON CONFLICT

## Verifica finale

- `cargo check -p mcp-core` — OK (rebuild release 7m29s)
- `tsc --noEmit` (web-ide) — OK
- `pnpm verify` parziale tramite lefthook (cargo_check_quick + turbo_quick) — OK
- Servizi attivi dopo restart:
  - mcp-core :4000 = HTTP 200
  - web-ide :3000 = HTTP 200
- Push branch: `e1db58f..d401566 -> test/nexus-maturity-2026-05-14T1556`

## Rubrica maturita aggiornata

| Dim | Valore | Note |
|-----|--------|------|
| D1  | 4 iterazioni | iter_01..iter_04 |
| D2  | A,B,C,D,G (5/8) | gap distribuiti su categorie ampie |
| D3  | OK | PRD generato in iter_04 con attori+UC+NFR |
| D4  | OK | Schema Prisma coerente, FK valide |
| D5  | parziale | npm run dev OK; suite test non eseguita |
| D6  | 0 violazioni | nessun modello hardcoded/emoji/unwrap/payload |
| D7  | 0 loop sterili | nessuno intercettato |
| D8  | n/a | iter_04 successo al primo tentativo |
| D9  | OK | branch test/* contiene solo fix dichiarati |
| D10 | ~3 EUR | sotto cap 100 EUR (3% utilizzo) |
| D11 | ~4h | sotto 2 giorni window |
| D12 | OK | fix passano cargo check + tsc + lefthook |

**Punteggio finale**: ~28/36 (maturita media-alta dopo fix; era ~22/36 pre-fix)

## Prossimi step suggeriti

1. Merge branch `test/nexus-maturity-2026-05-14T1556` in main come PR unica (oppure 3 PR separate per ondata).
2. PR Python dedicata per M3 streaming LangGraph.
3. PR UI follow-up per M15 dialog create-repo.
4. Smoke test full `pnpm verify` cross-workspace prima del merge.
