# FASE 0 — Bootstrap

Questa fase stabilisce il fondamento infrastrutturale per il sistema Nexus LLM ibrido.

## Setup locale

### Prerequisiti

- Node.js 20+
- pnpm 10.7.0+
- Docker + Docker Compose
- (opzionale) GPU e CUDA per vLLM (necessaria per phase hybrid/onprem)

### Avvio rapido

```bash
# Installa dipendenze
pnpm install

# Avvia stack cloud profile (no GPU needed)
make dev-cloud

# In un altro terminale, verifica services
curl http://localhost:5432  # Postgres
curl http://localhost:6379  # Redis
curl http://localhost:16686 # Jaeger UI
curl http://localhost:8001  # Presidio
```

### Profili disponibili

#### Cloud (default, no GPU needed)
```bash
make dev-cloud
```
- Postgres + pgvector
- Redis
- OpenTelemetry Collector → Jaeger
- Presidio API stub
- Zero external provider calls yet

#### Hybrid (GPU required)
```bash
make dev-hybrid
```
- All of cloud +
- vLLM container (Qwen 2.5 Coder 32B)
- GPU: `CUDA_VISIBLE_DEVICES=0` (configurabile)

#### On-Premise (GPU required)
```bash
make dev-onprem
```
- All of hybrid
- Cloud providers disabled
- Mirrored production setup for local testing

## Struttura di configurazione

### Profili di configurazione
- `config/policies/default.yaml` — cloud profile (stage default)
- `config/policies/hybrid.yaml` — cloud + vLLM fallback
- `config/policies/onprem.yaml` — solo vLLM

### Model aliases
- `config/model-aliases.yaml` — mapping logico → provider-specifico

### Environment variables
```bash
export NEXUS_PROFILE=cloud          # cloud | hybrid | onprem
export POSTGRES_URL=postgres://...  # default: localhost:5432
export REDIS_URL=redis://...        # default: localhost:6379
```

## Pacchetti installati

### @nexus/shared (packages/shared)
- **config.ts**: Zod schema validator + loader
- **telemetry.ts**: OpenTelemetry SDK initialization
- **errors.ts**: Error types per domain
- **index.ts**: Barrel export

```typescript
import { loadConfig, createLogger, initTelemetry } from "@nexus/shared";

const config = loadConfig();
initTelemetry(config);
const log = createLogger(config);
```

## CI/CD base

GitHub Actions workflow in `.github/workflows/ci.yml`:

1. **Lint** — ESLint on all packages
2. **Typecheck** — TypeScript strict mode
3. **Build** — Turbo build pipeline
4. **Test** — Vitest runner
5. **Health check** — Smoke test

Triggered on: push to main/develop, all PRs.

## Acceptance Criteria

✅ **Fase 0 è completata quando:**

- [ ] `pnpm install` succeeds, no warnings
- [ ] `make dev-cloud` starts all services
- [ ] `pnpm lint` passes
- [ ] `pnpm typecheck` passes
- [ ] `pnpm build` produces dist/ artifacts
- [ ] CI pipeline completes green on main
- [ ] Jaeger UI accessible at http://localhost:16686
- [ ] Postgres health check passes: `curl http://localhost:5432/health` (or psql test)
- [ ] Redis health check passes: `redis-cli ping`
- [ ] Config loader can instantiate all three profiles without errors

## Prossimi passi

→ **FASE 1**: LLM Gateway con provider esterni (Anthropic, OpenAI, Mistral)

Dopo questa fase, il gateway sarà pronto per routing e provider abstraction.
