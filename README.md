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

## Remote dev server



- 
- `docs/operations/ai-billing-accounting.md`




These are the canonical entry points for AI agents and humans working against the remote dev box.

## Status

This repository currently contains the full architecture scaffold, contracts, schema, and service skeletons needed to implement the execution plan. It is designed to be extended incrementally across the eight roadmap milestones.
