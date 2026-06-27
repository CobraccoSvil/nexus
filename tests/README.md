# Test suite Nexus (PR-4)

Livelli di test che completano l'orchestratore Plan/Act/Verify + Sub-agents.

NOTA (migrazione zero-Python): i Livelli 4 (E2E pytest, ex `tests/e2e/nexus-suite/`)
e 5 (maturity rubric Python, ex `tests/nexus-maturity/v2/run_rubric.py`) sono stati
RIMOSSI. Restano il Livello 3 (contract Rust) e il Livello 6 (UI Playwright). La
cartella `tests/nexus-maturity/` conserva ancora gli helper shell (`collect.sh`,
`monitor.sh`) e le run storiche.

## Layout

```
tests/
└── README.md (questo file)

crates/mcp-core/tests/                # Livello 3: contract test Rust
├── agent_tools_safety.rs
├── agent_runs_endpoints.rs
├── orchestrator_db_schema.rs
├── postgres_app_isolation.rs
├── m71_cost_breakdown.rs
└── subagent_workflow.rs

apps/web-ide/e2e/orchestrator/       # Livello 6: UI e2e Playwright
├── _setup.ts
├── admin-orchestrator-toggle.spec.ts
├── admin-subagent-crud.spec.ts
├── admin-plan-inspector.spec.ts
├── admin-sidebar-orchestrator.spec.ts
├── chat-plan-first-toggle.spec.ts
├── provider-settings-reload.spec.ts
├── api-admin-orchestrator.spec.ts
├── db-project-panel.spec.ts
└── m71-cost-breakdown-ui.spec.ts
```

## Esecuzione

```bash
# Livello 3: contract Rust
pnpm test:contract
# → cargo test -p mcp-core --tests

# Livello 6: UI Playwright
pnpm test:ui-e2e
# → playwright test apps/web-ide/e2e/orchestrator
```

## Prerequisiti

I test **NON** avviano i servizi: si aspettano che siano già up.

| Servizio | Porta | Come avviarlo |
|---|---|---|
| postgres-nexus | 5433 | `docker compose -f docker-compose.local.yml up -d postgres-nexus` |
| postgres-app | 5434 | `docker compose -f docker-compose.local.yml up -d postgres-app` |
| mcp-core | 4000 | `bash deploy/deploy-local.sh --service mcp-core` |
| brain | 8001 | `bash deploy/deploy-local.sh --service brain` |
| admin-service | 4010 | `bash deploy/deploy-local.sh --service admin-service` |
| web-ide | 3000 | `bash deploy/deploy-local.sh --web` |

### Variabili d'ambiente

```bash
export DATABASE_URL="postgres://nexus:nexus@localhost:5433/nexus"
export MCP_CORE_URL="http://localhost:4000"
export BRAIN_URL="http://localhost:8001"
export WEB_IDE_URL="http://localhost:3000"
export ADMIN_SERVICE_URL="http://localhost:4010"
export NEXUS_APP_ADMIN_URL="postgres://nexus_admin:nexus_admin_secret@localhost:5434/postgres"

# JWT admin per i test che chiamano endpoint protetti
bash /tmp/mint_jwt.sh   # vedi tools/dev-login per il helper
export NEXUS_TEST_JWT_PATH=/tmp/nexus_jwt.txt
```

### Setup Playwright (una sola volta)

```bash
pnpm add -D @playwright/test
pnpm exec playwright install --with-deps chromium
```

## Comportamento "skip if not available"

Tutti i test sono **robusti a CI shape variations**:

- Test che richiedono DB ma `DATABASE_URL` mancante → `eprintln!("skip: ...")` ed exit 0
- Test che richiedono auth JWT ma `NEXUS_TEST_JWT` mancante → `pytest.skip(...)`
- Test che richiedono servizi live ma fail il health probe → `eprintln!("skip: ...")`

Questo permette di lanciare la suite anche in ambienti parziali (es. solo unit-level) senza falsi positivi.

## CI/CD

Vedi [.github/workflows/nexus-suite.yml](../.github/workflows/nexus-suite.yml):
- **PR**: Livello 3 (contract Rust) sempre
- **Nightly**: Livello 6 (Playwright) su worktree dedicata
