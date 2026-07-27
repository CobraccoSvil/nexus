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
├── precondizioni_integrazione.rs    # sentinella: dichiara cosa c'e' e cosa no
├── agent_tools_safety.rs
├── agent_runs_endpoints.rs
├── orchestrator_db_schema.rs
├── postgres_app_isolation.rs
├── project_db_config_contract.rs
├── settings_update_contract.rs
├── chat_history_run_anchor.rs
└── m71_cost_breakdown.rs

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

## Comportamento quando l'ambiente non c'e'

I test di integrazione sono opportunistici: se manca il DB, il servizio o il JWT
non possono girare. Come lo dicono e' il punto.

Fino al 2026-07-26 facevano `eprintln!("skip: ...")` e `return`, e questa pagina
sosteneva che cosi' si evitavano i falsi positivi: era l'opposto. Un test che
salta e un test che ha verificato il contratto producevano lo STESSO verde, e nel
conteggio di `cargo test` erano indistinguibili — 48 skip in 11 file di tre crate
che il gate contava come contratti verificati.

Oggi la precondizione passa dal punto unico
[`nexus-test-preconditions`](../crates/nexus-test-preconditions/src/lib.rs):

- **senza** `REQUIRE_INTEGRATION_TESTS`: il test salta stampando
  `NEXUS_TEST_SKIP <categoria>: <motivo>` — visibile nei log, preteso dal guard
  `test-skip-visibile` di `scripts/check-single-source.sh`;
- **con** `REQUIRE_INTEGRATION_TESTS=1`: una precondizione mancante e' un
  FALLIMENTO che la nomina.

La sentinella `mcp-core/tests/precondizioni_integrazione.rs` gira sempre e
dichiara il quadro (`PRECONDIZIONI INTEGRAZIONE mcp-core: n/4 presenti`), con
cosa resta non misurato per ognuna che manca.

Per lanciare la suite pretendendo l'ambiente:

```bash
REQUIRE_INTEGRATION_TESTS=1 cargo test -p mcp-core --test settings_update_contract
```

## CI/CD

Vedi [.github/workflows/integration-full.yml](../.github/workflows/integration-full.yml):
il job che allestisce l'ambiente completo (due cluster Postgres, Redis, mcp-core
in ascolto, JWT coniato dal percorso di produzione) e gira con
`REQUIRE_INTEGRATION_TESTS=1`, di notte o a mano. L'elenco dei test che esegue e'
esplicito nel workflow, insieme ai due che restano fuori e al perche'.

Vedi [.github/workflows/nexus-suite.yml](../.github/workflows/nexus-suite.yml):
- **PR**: Livello 3 (contract Rust) sempre
- **Nightly**: Livello 6 (Playwright) su worktree dedicata
