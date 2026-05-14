# Project workspace endpoints — automazione e bootstrap

Endpoint REST di `mcp-core` introdotti dal test maturita Nexus 2026-05-14 per automatizzare bootstrap progetti, rilevamento porte, gestione issue runtime, smoke browser e integrazione GitHub.

Tutti gli endpoint richiedono JWT (`Authorization: Bearer <token>`) e validano `project_id` contro i permessi utente via `load_project_context`.

## POST /api/projects/:id/services/install-playwright

Installa Playwright nel progetto (auto-detect frontend dir), genera `playwright.config.ts` deterministico con `baseURL` dalla porta dev rilevata in `nexus_port_allocations`, crea `e2e/smoke.spec.ts` e imposta `settings.playwright_enabled=true`.

Body: nessuno.

Risposta: `{ok, installed_in, base_url, port, config_path, spec_path}`.

Codice: [crates/mcp-core/src/project_workspace/playwright_install.rs](../crates/mcp-core/src/project_workspace/playwright_install.rs).

## POST /api/projects/:id/services/browser-check

Esegue `npx playwright test e2e/smoke.spec.ts --reporter=line` con `BASE_URL` override e parsa console error dallo stdout.

Body: `{base_url?: string}` (opzionale; default deriva da `nexus_port_allocations`).

Risposta: `{ok, exit_code, console_errors[], stdout_tail, stderr_tail}`.

Codice: [crates/mcp-core/src/project_workspace/browser_check.rs](../crates/mcp-core/src/project_workspace/browser_check.rs).

## POST /api/projects/:id/services/scan-ports

Scansiona `package.json` (root + `frontend/` + `backend/`), `vite.config.*`, `Procfile`, `docker-compose.yml` con regex mirate. UPSERT idempotente in `nexus_port_allocations` con label inferita (`app`/`frontend`/`backend`/`compose`).

Body: nessuno.

Risposta: `{ok, detected_count, inserted_count, inserted[], raw_detections[]}`.

Codice: [crates/mcp-core/src/project_workspace/scan_ports.rs](../crates/mcp-core/src/project_workspace/scan_ports.rs).

## POST /api/projects/:id/services/auto-bootstrap

Orchestrator minimal: chiama `scan-ports` + `install-playwright`. Pensato per essere invocato post create/clone progetto.

Body: nessuno.

Risposta: `{ok, stack_detected, steps[]}`.

Codice: [crates/mcp-core/src/project_workspace/auto_bootstrap.rs](../crates/mcp-core/src/project_workspace/auto_bootstrap.rs).

## GET /api/projects/:id/runtime-issues

Lista issue runtime tracciate dagli hook tool agente.

Query: `?status=open|resolved|all` (default `open`).

Risposta: `{issues: [{id, source, severity, message, fingerprint, status, ...}]}`.

## POST /api/projects/:id/runtime-issues

INSERT di una runtime issue. Dedup via `fingerprint = sha256(message+command)[:16]` con `ON CONFLICT (project_id, fingerprint)`.

Body: `{source, severity, message, details?, run_id?, step_id?, tool_name?, command?, exit_code?}`.

Risposta: `{ok, id, created: bool}`.

## PATCH /api/projects/:id/runtime-issues/:iid

Aggiorna lo `status` di una issue (`open`/`resolved`/`ignored`).

Body: `{status: string}`.

Codice: [crates/mcp-core/src/project_workspace/runtime_issues.rs](../crates/mcp-core/src/project_workspace/runtime_issues.rs).

Tabella: [db/migrations/0138_project_runtime_issues.sql](../db/migrations/0138_project_runtime_issues.sql).

## GET /api/projects/:id/fs-events

Snapshot polling-based per refresh tree EXPLORER. Scansione BFS depth 4, esclude `node_modules`, `.git`, `target`, `.next`, `dist`, `build`.

Query: `?since_fingerprint=N` (per detect cambiamenti senza polling pesante).

Risposta: `{fingerprint, file_count, last_modified_iso, changed: bool}`.

Codice: [crates/mcp-core/src/project_workspace/fs_events.rs](../crates/mcp-core/src/project_workspace/fs_events.rs).

## POST /api/projects/:id/github/create-repo

Crea un nuovo repository GitHub per l'utente connesso e configura `origin` remote sul progetto.

Body: `{name: string, private?: bool=true, description?: string, auto_init?: bool=false}`.

Validazione: `name` solo alfanumerico + `-`, `_`, `.` (allow list GitHub).

Risposta: `{ok, html_url, clone_url, full_name, private, origin_configured, default_branch}`.

UI: pulsante "Crea repo su GitHub" in [Source Control panel](../apps/web-ide/components/git/source-control-panel.tsx) quando `githubStatus.reason === "missing_origin_remote"` e l'account GitHub e' connesso.

Codice: [crates/mcp-core/src/github.rs](../crates/mcp-core/src/github.rs) (`github_create_repo`).

## Tool agente correlati

- `git_remote_add(remote_url)` — registrato in [crates/mcp-core/src/agent_tools/git.rs](../crates/mcp-core/src/agent_tools/git.rs); valida https/git@/ssh, idempotente (rimuove origin pre-esistente).

## Vedi anche

- [tests/nexus-maturity/2026-05-14T1556/report_finale_consolidato.md](../tests/nexus-maturity/2026-05-14T1556/report_finale_consolidato.md) — Report finale 3 ondate (15/19 gap chiusi).
- [tests/nexus-maturity/2026-05-14T1556/journal_fix.md](../tests/nexus-maturity/2026-05-14T1556/journal_fix.md) — Journal dei 19 gap originali.
