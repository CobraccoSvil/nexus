# AI-Orchestrator v2

Enterprise monorepo scaffold for an AI-assisted development platform with:

- Rust-first MCP/orchestrator core
- Python neural core and provider adapters
- TypeScript Web IDE, CLI, and dashboard
- PostgreSQL, Redis, and Qdrant backing services

## Workspaces

- `crates/*`: Rust core crates
- `brain/*`: Python neural core
- `apps/*`: Web IDE, VS Code extension, CLI
- `packages/*`: shared TypeScript packages
- `proto/*`: service contracts
- `db/migrations/*`: initial database schema
- `deploy/*`: local and production deployment assets

## Quick start

1. Install Node.js 20+, pnpm 9+, Python 3.11+, Rust 1.75+, and `protoc`.
2. Copy `.env.example` to `.env` and adjust values.
3. Run `pnpm install`.
4. Run `docker compose -f deploy/docker-compose.yml up -d postgres redis qdrant shadow-db`.
5. Start the Web IDE with `pnpm --filter @ai-orchestrator/web-ide dev`.
6. Start the neural core with `python -m brain.grpc_server.main`.
7. Build and run the Rust core with Cargo once Rust is installed.

## Database backups (WSL/dev)

If you run the local stack in WSL with `./scripts/dev-wsl.sh`, the Postgres DB lives in a Docker volume.
To avoid losing `settings` (OAuth/API keys/routing) on `docker compose down -v`, use the backup scripts:

- Create a backup:
  - `./scripts/db-backup.sh`
- Restore from latest backup:
  - `./scripts/db-restore.sh latest`
- Restore from a specific file:
  - `./scripts/db-restore.sh ./backups/postgres/nexus_YYYYmmdd_HHMMSS.dump`

Notes:
- Backups are stored in `./backups/postgres/` and are ignored by git.
- Default retention keeps the last 40 dumps. Override with `KEEP_LAST=…`.

## Remote dev server



- 
- `docs/operations/ai-billing-accounting.md`




These are the canonical entry points for AI agents and humans working against the remote dev box.

## Status

This repository currently contains the full architecture scaffold, contracts, schema, and service skeletons needed to implement the execution plan. It is designed to be extended incrementally across the eight roadmap milestones.

## Backlog closure recente

- `docs/backlog-closure-2026-05-19.md` — report degli 8 commit applicati al
  branch `chore/backlog-closure` (bonifica hardcoding modelli §G, gate
  `pnpm verify` verde, fix `unwrap`/`expect` §F, eliminazione 105 warning
  TS lint, allineamento STYLING_REFACTOR_PROGRESS al codice reale).
- `docs/tech-debt-rust.md`, `docs/tech-debt-ts.md` — backlog corrente per
  linguaggio.
- `STYLING_REFACTOR_PROGRESS.md` — riconteggio reale 2884 inline styles in 92 file.
