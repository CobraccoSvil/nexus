# Nexus LLM Gateway — Runbook

Scenari di incident response. Ogni scenario segue lo schema: **Sintomi → Diagnosi → Rimedi → Escalation**.

---

## 1. Provider Cloud Down

### Sintomi
- `error_rate > 5%` su dashboard Grafana
- Log: `ProviderError: 503` ripetuti per un provider specifico
- Langfuse mostra spike di `finish_reason: error`

### Diagnosi
```bash
# Stato health checker interno
curl http://localhost:3001/providers | jq '.[] | {name, healthy}'

# Verifica connettività diretta
curl -I https://api.anthropic.com/v1/models -H "x-api-key: $ANTHROPIC_API_KEY"
curl -I https://api.openai.com/v1/models -H "Authorization: Bearer $OPENAI_API_KEY"

# Log gateway
docker logs nexus-gateway --tail=100 | grep "ProviderError"
```

### Rimedi
1. **Fallback automatico**: il fallback multi-provider (`run_fallback`) già tenta il provider secondario. Verifica che il secondario sia healthy.
2. **Forzatura manuale del provider** (aggiornando la policy):
   ```yaml
   # config/policies/default.yaml — commenta il provider down
   routing:
     tier_0:
       primary: openai      # temporaneo, anthropic down
       secondary: mistral
   ```
3. **Riavvia il health check** per forzare rivalutazione:
   ```bash
   curl -X POST http://localhost:3001/admin/health-check
   ```

### Escalation
Se tutti i provider cloud sono down > 15 min → attiva profilo `hybrid` con vLLM (se disponibile).

---

## 2. Breach Sospetto / Data Exfiltration

### Sintomi
- Alert Anomaly Detector: `tier_escalation` o `injection_attempt` ripetuti dallo stesso tenant
- Log: `dlp_block` su richieste che dovrebbero essere safe
- Traffico anomalo da un tenant specifico (token spike)

### Diagnosi
```bash
# Query audit DB per il tenant sospetto
psql $POSTGRES_URL -c "
  SELECT request_id, user_id, sensitivity_tier, dlp_blocked, dlp_patterns, created_at
  FROM audit_llm_calls
  WHERE tenant_id = '<tenant-id>'
    AND created_at > NOW() - INTERVAL '1h'
  ORDER BY created_at DESC
  LIMIT 50;
"

# Pattern di injection rilevati
psql $POSTGRES_URL -c "
  SELECT dlp_patterns, COUNT(*) as hits
  FROM audit_llm_calls
  WHERE tenant_id = '<tenant-id>'
    AND dlp_blocked = true
    AND created_at > NOW() - INTERVAL '24h'
  GROUP BY dlp_patterns;
"

# Hash dei prompt (non in chiaro — solo per correlazione)
psql $POSTGRES_URL -c "
  SELECT prompt_hash, COUNT(*) as ripetizioni
  FROM audit_llm_calls
  WHERE tenant_id = '<tenant-id>'
  GROUP BY prompt_hash
  HAVING COUNT(*) > 10;
"
```

### Rimedi
1. **Blocco immediato del tenant** (aggiungere alla blocklist nella policy):
   ```bash
   # Aggiorna config/policies/default.yaml aggiungendo il tenant alla deny list
   # Poi riavvia il gateway
   docker compose restart nexus-gateway
   ```
2. **Revoca JWT** (cambia il JWT_SECRET → invalida tutti i token esistenti):
   ```bash
   # ATTENZIONE: invalida TUTTI i token, non solo del tenant compromesso
   # Usare solo in emergenza
   docker compose exec nexus-gateway sh -c "JWT_SECRET=new-secret-$(date +%s) ..."
   ```
3. **Crypto-shredding del tenant** (se i dati sono stati compromessi):
   ```typescript
   // Esegui in una shell di manutenzione
   const { TenantCrypto } = require("@nexus/shared");
   const crypto = new TenantCrypto(productionKMS);
   await crypto.shredTenant("<tenant-id>");
   ```
4. **Notifica CISO** e apri ticket di security incident.

### Escalation
Breach confermato → DPIA update obbligatoria, notifica all'autorità di controllo entro 72h (GDPR Art. 33).

---

## 3. Saturazione GPU / vLLM

### Sintomi
- vLLM latency p95 > 10s
- Log vLLM: `CUDA out of memory` o `Queue full`
- `health_check` vLLM ritorna 503

### Diagnosi
```bash
# Utilizzo GPU corrente
nvidia-smi --query-gpu=name,memory.used,memory.free,utilization.gpu --format=csv

# Code vLLM
curl http://localhost:8000/metrics | grep -E "vllm:queue|vllm:running"

# Log vLLM
docker logs nexus-onprem-vllm-1 --tail=50 | grep -E "ERROR|WARNING|OOM"
```

### Rimedi (in ordine)
1. **Riduci max_model_len** per liberare VRAM (richiede restart vLLM):
   ```bash
   VLLM_MAX_MODEL_LEN=16384 docker compose -f infra/docker/docker-compose.onprem.yml restart vllm
   ```
2. **Abilita page attention** per migliore gestione memoria:
   ```bash
   # Aggiungi al comando vLLM: --enable-chunked-prefill
   ```
3. **Rate limiting più aggressivo** (riduci `perProvider.requests` nel gateway).
4. **Aggiungi istanza vLLM** (horizontal scaling):
   ```bash
   # Avvia seconda istanza su porta 8001
   VLLM_PORT=8001 docker compose -f infra/docker/docker-compose.onprem.yml up -d vllm
   # Aggiorna la policy per includere il secondo endpoint
   ```
5. **Fallback a cloud** (solo in hybrid profile): aggiorna temporaneamente la policy.

### Escalation
Se l'OOM è ricorrente → modello troppo grande per la GPU disponibile. Valuta:
- Quantizzazione (AWQ/GPTQ): riduce VRAM 50-75%
- Modello più piccolo (7B → 32B)
- Upgrade hardware GPU

---

## 4. Database Postgres Lento

### Sintomi
- `audit_write latency > 500ms` nei log
- `retrieval latency > 1s` (embedding search lento)
- Connessioni al pool esaurite

### Diagnosi
```bash
# Query lente (> 100ms)
psql $POSTGRES_URL -c "
  SELECT query, calls, total_time, mean_time
  FROM pg_stat_statements
  WHERE mean_time > 100
  ORDER BY mean_time DESC
  LIMIT 20;
"

# Indici mancanti
psql $POSTGRES_URL -c "
  SELECT schemaname, tablename, seq_scan, seq_tup_read, idx_scan
  FROM pg_stat_user_tables
  WHERE seq_scan > idx_scan
  ORDER BY seq_tup_read DESC;
"

# Connessioni attive
psql $POSTGRES_URL -c "
  SELECT count(*), state, wait_event_type
  FROM pg_stat_activity
  GROUP BY state, wait_event_type;
"

# Bloat delle partizioni audit
psql $POSTGRES_URL -c "
  SELECT partition_name, pg_size_pretty(pg_total_relation_size(partition_name::regclass))
  FROM information_schema.tables
  WHERE table_name LIKE 'audit_llm_calls%'
  ORDER BY pg_total_relation_size(partition_name::regclass) DESC;
"
```

### Rimedi
1. **VACUUM ANALYZE** sulle tabelle calde:
   ```sql
   VACUUM ANALYZE audit_llm_calls;
   VACUUM ANALYZE embeddings;
   ```
2. **Drop partizioni vecchie** (data scaduta oltre retention):
   ```sql
   -- Rimuovi partizioni con created_at < 90 giorni fa
   -- Le partizioni più vecchie possono essere droppate senza intaccare i dati recenti
   ```
3. **Aumenta pool connections** se si esauriscono:
   ```bash
   # In .env
   DB_POOL_SIZE=30  # default 20
   ```
4. **pgBouncer** se le connessioni sono il collo di bottiglia.
5. **Reindex** dell'indice ivfflat se le performance vector search degradano:
   ```sql
   REINDEX INDEX CONCURRENTLY idx_embeddings_vector;
   ```

### Escalation
Se le query lente persistono dopo VACUUM → analisi explain plan. Coinvolgi DBA.

---

## 5. Alert: DLP Block Rate Anomalo

### Sintomi
- `dlp_block_rate > 5%` nell'ultima ora (baseline normale < 0.5%)

### Diagnosi
```bash
psql $POSTGRES_URL -c "
  SELECT dlp_patterns, COUNT(*) as hits, tenant_id
  FROM audit_llm_calls
  WHERE dlp_blocked = true
    AND created_at > NOW() - INTERVAL '1h'
  GROUP BY dlp_patterns, tenant_id
  ORDER BY hits DESC;
"
```

### Interpretazione
- **Stesso tenant, pattern ripetuto** → Attacco deliberato. Vedi scenario 2.
- **Pattern `email_pii` o `italian_cf`** → Utente legittimo invia PII senza rendersene conto. Aggiorna la UX per mostrare warning.
- **Pattern `aws_key`** → Sviluppatore ha incollato un file di config. Manda notifica al tenant con istruzioni.

---

## Contatti di Escalation

| Livello | Contatto | SLA risposta |
|---|---|---|
| L1 - On-call | PagerDuty rotation | 15 min |
| L2 - Security | CISO | 30 min |
| L3 - Infra | SRE lead | 1h |
| Provider incident | Anthropic Status / OpenAI Status | external |
