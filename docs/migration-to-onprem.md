# Migrazione On-Premise — Procedura Step-by-Step

Questo documento descrive come migrare un'istanza Nexus dal profilo `cloud` al profilo `onprem` (o `hybrid`). La migrazione non richiede modifiche al codice applicativo: l'unica differenza è la configurazione.

---

## Prerequisiti

| Risorsa | Requisito minimo |
|---|---|
| GPU | NVIDIA con ≥ 40 GB VRAM (es. A100, H100, RTX 6000 Ada) |
| RAM sistema | 64 GB |
| Disco (modelli) | 100 GB SSD NVMe |
| OS | Ubuntu 22.04 LTS + driver NVIDIA 535+ |
| Software | Docker 24+, Docker Compose v2, `nvidia-container-toolkit` |

### Installare nvidia-container-toolkit

```bash
curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey | sudo gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
curl -s -L https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list | \
  sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' | \
  sudo tee /etc/apt/sources.list.d/nvidia-container-toolkit.list
sudo apt-get update && sudo apt-get install -y nvidia-container-toolkit
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker
```

---

## Fase 1 — Preparazione infrastruttura

### 1.1 Clona il repository

```bash
git clone <repo-url> /opt/nexus
cd /opt/nexus
```

### 1.2 Crea il file `.env` on-premise

```bash
cp .env.example .env.onprem
```

Compila `.env.onprem`:

```bash
# Profilo
NEXUS_PROFILE=onprem

# vLLM
VLLM_MODEL=Qwen/Qwen2.5-Coder-32B-Instruct
VLLM_MAX_MODEL_LEN=32768
HF_TOKEN=hf_...               # token HuggingFace per il download del modello

# Database
POSTGRES_PASSWORD=<password-sicura>

# Osservabilità
LANGFUSE_SECRET=<stringa-casuale-32-char>
LANGFUSE_SALT=<stringa-casuale-32-char>

# JWT
JWT_SECRET=<stringa-casuale-32-char>

# Feature flags (zero cloud in onprem)
NEXUS_ALLOW_CLOUD_TIER2=false
NEXUS_ALLOW_CLOUD_TIER3=false
NEXUS_REDACTION_STRICT=true
NEXUS_DLP_ENABLED=true
```

### 1.3 Avvia lo stack

```bash
docker compose -f infra/docker/docker-compose.onprem.yml --env-file .env.onprem up -d
```

vLLM scaricherà il modello al primo avvio (~60 GB). Monitora con:

```bash
docker logs -f nexus-onprem-vllm-1
# Attendi: "Uvicorn running on http://0.0.0.0:8000"
```

---

## Fase 2 — Validazione stack

### 2.1 Smoke test automatico

```bash
chmod +x scripts/onprem-smoke.sh
NEXUS_TEST_TOKEN=$(node -e "
  const { JWTService } = require('@nexus/shared');
  const svc = new JWTService(process.env.JWT_SECRET);
  svc.sign({ tid: 'smoke-test', uid: 'smoke-user', scp: ['llm:complete'] }).then(console.log);
") ./scripts/onprem-smoke.sh
```

Output atteso:
```
[ OK ] Postgres: pg_isready OK
[ OK ] vLLM disponibile
[ OK ] vLLM modello attivo: Qwen/Qwen2.5-Coder-32B-Instruct
[ OK ] Gateway: provider vllm registrato
[ OK ] Call LLM end-to-end: risposta ricevuta
[ OK ] Provider usato: vllm (corretto)
[ OK ] Smoke test SUPERATO (0 failure)
```

### 2.2 Verifica isolamento provider

```bash
curl -s http://localhost:3001/providers | jq '.[] | .name'
```

Deve restituire SOLO `"vllm"`. Se appaiono `"anthropic"`, `"openai"` o `"mistral"`, il profilo non è configurato correttamente.

### 2.3 Verifica RLS database

```bash
docker exec -it nexus-onprem-postgres-1 psql -U nexus -c \
  "SELECT tablename, rowsecurity FROM pg_tables WHERE tablename IN ('audit_llm_calls', 'embeddings', 'rate_limits');"
```

Output atteso: `rowsecurity = t` per tutte le tabelle.

---

## Fase 3 — Migrazione dati (solo se migrazione da cloud esistente)

### 3.1 Export dati dal cloud

```bash
# Sul server cloud
pg_dump -h <cloud-host> -U nexus -d nexus \
  --table=embeddings \
  --table=tenants \
  -Fc -f nexus-export.dump
```

### 3.2 Import on-premise

```bash
# Sul server on-premise
pg_restore -h localhost -U nexus -d nexus \
  --no-owner --no-privileges \
  nexus-export.dump
```

### 3.3 Applica RLS post-import

```bash
docker exec -i nexus-onprem-postgres-1 psql -U nexus -d nexus < infra/sql/rls-policies.sql
```

---

## Fase 4 — Cutover

### 4.1 Aggiorna DNS / reverse proxy

Reindirizza il traffico dal gateway cloud a `http://<onprem-ip>:3001`.

### 4.2 Monitoraggio prime 72h

Dashboard da verificare:
- **Jaeger** `http://localhost:16686` — trace LLM calls
- **Langfuse** `http://localhost:3000` — token usage, latency
- **Logs** `docker compose logs -f nexus-gateway`

Alert da configurare:
- vLLM latency p95 > 10s → scale GPU o riduci `max-model-len`
- `content_filter` in `finish_reason` → revedi la configurazione del modello
- DLP block rate > 5% → verifica redaction pipeline

---

## Fase 5 — Kubernetes (produzione scalabile)

```bash
# Aggiungi il repo Helm (se pubblicato)
helm install nexus ./infra/k8s/nexus-chart \
  -f infra/k8s/nexus-chart/values.yaml \
  --set vllm.model=Qwen/Qwen2.5-Coder-32B-Instruct \
  --set postgres.passwordSecret=nexus-postgres-secret \
  -n nexus --create-namespace
```

Verifica GPU scheduling:
```bash
kubectl get pods -n nexus -o wide
kubectl describe pod <vllm-pod> -n nexus | grep -A5 "Limits:"
```

---

## Rollback

Se la migrazione fallisce, il rollback è immediato: aggiorna il DNS per reindirizzare al gateway cloud. Non è necessario alcun rollback del database — i dati on-premise sono addizionali rispetto al cloud.

---

## Checklist go-live onprem

- [ ] `nvidia-smi` mostra la GPU sul server target
- [ ] vLLM health risponde `{"status":"ok"}` su `/health`
- [ ] Smoke test supera 0 failure
- [ ] Nessun provider cloud in `/providers`
- [ ] RLS attivo su tutte le tabelle (`rowsecurity = t`)
- [ ] Langfuse mostra trace delle chiamate LLM
- [ ] Test cross-profilo verde in CI (`pnpm test`)
- [ ] Jaeger mostra span completi (classifier → redaction → vllm → audit)
- [ ] Alert configurati su latency, DLP block rate, error rate
- [ ] Backup Postgres schedulato (cron o pg_basebackup)
- [ ] `JWT_SECRET` e `LANGFUSE_SECRET` salvati in gestione segreti (Vault, 1Password, ecc.)
