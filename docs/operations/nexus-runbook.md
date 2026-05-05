# Nexus Runbook (Ruflo Integration)

Runbook operazionale per la stack Nexus — Q-Learning router, swarm coordinator,
learning workers, tool catalog. Integrata in `mcp-core` dalla Fase 6.

**Ultimo aggiornamento**: Fase 8 Deployment & Monitoring.

## TL;DR

- **Che cos'è**: orchestrazione multi-agente nativa in Rust che affianca (non
  sostituisce) il routing provider/model esistente in `agent_loop.rs`. Attualmente
  opera in **shadow mode** — logga la raccomandazione Q-Learning ma non altera
  il flusso reale.
- **Dove vive**: `crates/nexus-orchestrator`, `crates/nexus-agents`, `crates/ruvector`.
  Bridge in `crates/mcp-core/src/nexus_bridge.rs`, catalog in `crates/mcp-core/src/nexus_tool_catalog.rs`.
- **Come si accende / spegne**: init all'avvio di `mcp-core` in `main.rs`
  (`nexus_bridge::NexusBridge::init_global()`). Se non inizializzato, tutti
  i siti di chiamata fanno fallback silent — **nessun impatto sul routing reale**.
- **Come si monitora**: 4 endpoint HTTP pubblici sotto `/nexus/*`.

## Endpoint pubblici

Tutti gli endpoint sono read-only, non richiedono autenticazione, ritornano
`200 OK` se il bridge è inizializzato, `503` altrimenti.

| Path | Scopo | Frequenza scraping tipica |
|---|---|---|
| `GET /nexus/healthz` | Liveness probe del bridge (usabile come k8s liveness) | 10-30s |
| `GET /nexus/stats` | Snapshot JSON completo (router + scheduler + observability_ns) | on-demand, debug |
| `GET /nexus/tools` | Breakdown del `NexusToolCatalog` per categoria | on-demand |
| `GET /nexus/metrics` | Prometheus text format — **scraping Grafana** | 15-30s |

### Esempio `/nexus/healthz`

```json
{
  "status": "ok",
  "router": {
    "total_decisions": 1042,
    "current_epsilon": 0.095
  },
  "scheduler": {
    "workers": 7,
    "total_runs": 834,
    "total_failures": 0
  }
}
```

### Esempio `/nexus/metrics` (Prometheus)

```
# HELP nexus_router_decisions_total Total routing decisions made
# TYPE nexus_router_decisions_total counter
nexus_router_decisions_total 1042
# HELP nexus_router_decision_time_us Average decision time in microseconds
# TYPE nexus_router_decision_time_us gauge
nexus_router_decision_time_us 342.7
# HELP nexus_router_epsilon Current epsilon (exploration rate)
# TYPE nexus_router_epsilon gauge
nexus_router_epsilon 0.095
...
```

## Metriche chiave e soglie

| Metrica | Normale | Warning | Critical |
|---|---|---|---|
| `nexus_router_decision_time_us` | < 1000 (1ms) | 1000–5000 | > 5000 |
| `nexus_scheduler_failures_total / runs_total` | < 1% | 1–5% | > 5% |
| `nexus_router_cold_start_total / decisions_total` | < 10% dopo 1h | 10–30% | > 30% |
| `nexus_namespace_entries` | < 10k | 10k–50k | > 50k (possibile leak TTL) |
| `nexus_router_epsilon` | scende da 1.0 → 0.05 in 1-2h | statico dopo 2h | > 0.5 permanente |

## Alerting (Prometheus AlertManager)

Proposta di alert rules minimali — da adattare a `alerting-rules.yml`:

```yaml
groups:
- name: nexus
  rules:
  - alert: NexusBridgeDown
    expr: up{job="mcp-core"} == 1 and absent(nexus_router_decisions_total)
    for: 2m
    labels: { severity: warning }
    annotations:
      summary: "Nexus bridge non inizializzato (mcp-core è up ma non espone metriche nexus_*)"

  - alert: NexusRoutingSlow
    expr: nexus_router_decision_time_us > 5000
    for: 5m
    labels: { severity: warning }
    annotations:
      summary: "Routing Q-Learning > 5ms medio per 5m"

  - alert: NexusSchedulerFailureRate
    expr: |
      (
        increase(nexus_scheduler_failures_total[5m])
        /
        clamp_min(increase(nexus_scheduler_runs_total[5m]), 1)
      ) > 0.05
    for: 10m
    labels: { severity: critical }
    annotations:
      summary: "Nexus learning scheduler > 5% failure rate (10m window)"

  - alert: NexusNamespaceLeak
    expr: nexus_namespace_entries > 50000
    for: 30m
    labels: { severity: warning }
    annotations:
      summary: "Namespace observability con > 50k entries — possibile eviction TTL non funzionante"
```

## Incident playbook

### NexusBridge non inizializzato (`/nexus/healthz` → 503)

**Sintomo**: l'endpoint ritorna `{"status": "not_initialized"}`.

**Impatto**: **zero sul routing reale** — il bridge è opt-in. L'`agent_loop.rs`
continua a usare il flusso provider/model standard.

**Diagnosi**:
1. Controllare i log di boot di `mcp-core` per l'esito di `NexusBridge::init_global()`.
2. Cercare panic o errori nei worker di `nexus-orchestrator` (`UltralearnWorker`,
   `AuditWorker`, ecc.). Un panic in `new()` potrebbe impedire l'init, anche se
   al momento la `new()` non dovrebbe avere punti di fallimento.
3. Verificare che le deps `nexus-agents` / `nexus-orchestrator` abbiano compilato.

**Recovery**: riavvio di `mcp-core`. Il bridge è stateless in memoria, nessun
dato si perde.

### Routing troppo lento (`nexus_router_decision_time_us` > 5ms)

**Diagnosi**:
1. Consultare `/nexus/stats` per il breakdown `exploration_count`/`exploitation_count`.
   Se l'esplorazione esplode, è un comportamento atteso durante cold start.
2. Controllare il numero di agent registrati (default: 4 core). Con >100 agent
   la similarity search HNSW può rallentare.
3. Se il sistema è sotto heavy load (`tokio` saturo), la decision_time può
   includere contention non effettivo computo.

**Recovery**:
- Non c'è un kill switch dedicato; il bridge è observer-only, quindi
  non degrada il routing reale. Se serve disabilitarlo completamente, commentare
  `NexusBridge::init_global()` in `crates/mcp-core/src/main.rs` e redeploy.

### Learning worker failure rate alto

**Diagnosi**:
1. `/nexus/stats` → `scheduler.per_worker` mostra quale worker fallisce.
2. I worker affettati tipicamente sono `AuditWorker` (heuristic patterns) o
   `AnomalyDetectionWorker` (tuning soglie).

**Recovery**:
- Un worker failed non blocca gli altri (test Fase 7:
  `scheduler_resilience_failing_worker_does_not_block_others`).
- Se il failure rate è del 100% su un singolo worker, investigare il codice
  in `crates/nexus-orchestrator/src/workers/`.

### Namespace observability leak (`nexus_namespace_entries` > 50k)

**Diagnosi**:
1. `CleanupWorker` dovrebbe evictare entries scadute (TTL). Verifica che
   `total_runs` del cleanup worker stia incrementando in `/nexus/stats`.
2. Entries senza TTL (default `None`) non vengono mai evictate — è intenzionale
   per pattern consolidati, ma controllare che non ci siano scritture senza TTL
   da worker non previsti.

**Recovery**:
- Restart di `mcp-core` resetta il namespace in memoria.

## Deployment

### Target: Server di produzione (configura `DEPLOY_HOST`)

1. **Build**:
   ```bash
   cargo build --release -p mcp-core
   ```
2. **Test**: la suite Nexus deve essere verde — 113 test passano in <100ms.
   ```bash
   cargo test -p nexus-orchestrator -p nexus-agents -p ruvector
   cargo test -p mcp-core --bin mcp-core nexus_
   ```
3. **Deploy**: come da runbook standard (scp +
   systemd restart di `mcp-core`).
4. **Smoke check**: dopo il restart,
   ```bash
   curl -s http://${DEPLOY_HOST}:<port>/nexus/healthz | jq
   curl -s http://${DEPLOY_HOST}:<port>/nexus/stats | jq
   curl -s http://${DEPLOY_HOST}:<port>/nexus/metrics | head -30
   ```
5. **Monitoring**: se non già presente, aggiungere il job Prometheus:
   ```yaml
   - job_name: nexus-mcp-core
     metrics_path: /nexus/metrics
     scrape_interval: 30s
     static_configs:
       - targets: ['${DEPLOY_HOST}:<mcp-core-port>']
   ```

### Rollback

Il bridge è 100% additive — un rollback consiste semplicemente nel deploy
della release precedente. Non ci sono migration DB o breaking change di API
da invertire.

### Canary / feature flag

Attualmente il bridge è un pure observer (non influenza il routing reale).
Una canary non serve per la Fase 8: si può deployare senza
impatto sul traffic. Quando si passerà a **routing attivo** (Fase post-8),
si consiglia di introdurre un feature flag in `settings` (`nexus_active_routing`)
e fare rollout 5% → 25% → 100%.

## Note di sicurezza

- Gli endpoint `/nexus/*` sono **unauthenticated** e ritornano solo metriche
  aggregate (numeri, nessun payload di task / istruzioni / codice sorgente).
- Nessun dato utente transita per `observability_ns` — il namespace contiene
  pattern, metriche aggregate e profili per agent_type.
- Il `NexusToolCatalog` attualmente è un registro di stub: 0 tool sono
  effettivamente eseguibili in questa fase. Nessun arbitrary code execution.

## Riferimenti

- Piano integration: `C:\Users\CBRAC\.claude\plans\streamed-prancing-plum.md`
- Code: `crates/nexus-orchestrator`, `crates/nexus-agents`, `crates/ruvector`
- Bridge: `crates/mcp-core/src/nexus_bridge.rs`
- Tool catalog: `crates/mcp-core/src/nexus_tool_catalog.rs`
- Hardening test suite: `crates/nexus-orchestrator/tests/hardening.rs`
