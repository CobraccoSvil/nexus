# Test suite Nexus (PR-4)

Quattro livelli di test che completano l'orchestratore Plan/Act/Verify + Sub-agents.

## Layout

```
tests/
├── e2e/
│   └── nexus-suite/                  # Livello 4: E2E scenari Python (pytest)
│       ├── _helpers/                 # cfg, db, api, wait
│       ├── conftest.py
│       ├── run_all.py                # entry point
│       ├── test_scaffold_and_bugfix.py        # scenari 1-2
│       ├── test_subagent_isolation.py         # scenari 3-5
│       ├── test_clarifying_and_instructions.py # scenari 6-9
│       └── test_admin_orchestrator_api.py     # scenari 10-12
├── nexus-maturity/
│   └── v2/                           # Livello 5: rubric automatica D1-D12
│       ├── run_rubric.py
│       └── README.md
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

### Tutto in un colpo

```bash
pnpm test:nexus-suite
# alias di scripts/run-nexus-suite.sh
```

### Singolarmente

```bash
# Livello 3: contract Rust
pnpm test:contract
# → cargo test -p mcp-core --tests

# Livello 4: E2E Python
pnpm test:e2e
# → python3 tests/e2e/nexus-suite/run_all.py

# Livello 5: maturity rubric su run specifico
pnpm test:maturity -- --run-id <UUID>
# → python3 tests/nexus-maturity/v2/run_rubric.py --run-id <UUID>

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
- **PR**: Livello 3 (contract) sempre, Livello 4 (E2E) se servizi up
- **Nightly**: Livello 5 (maturity) + Livello 6 (Playwright) su worktree dedicata
