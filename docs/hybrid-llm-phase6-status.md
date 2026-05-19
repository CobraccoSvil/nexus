# Hybrid LLM Plan — Stato Fase 3-7 (aggiornato 2026-05-19)

Stato delle 5 sotto-fasi del piano `docs/nexus-hybrid-llm-plan.md` dopo il
branch `chore/backlog-closure`.

## 6.1 Embeddings gateway — PARZIALE

**Esistente** in `packages/embeddings/src/`:
- `chunker.ts` (177 righe) — chunking testo per RAG
- `onnx-runner.ts` (62 righe) — ONNX inference wrapper
- `reranker.ts` (59 righe) — cross-encoder reranking
- `types.ts` (29 righe), `index.ts` (re-export)
- Test: `chunker.test.ts` (8), `reranker.test.ts` (4) — passano in CI=1

**Da fare**:
- Esportare un endpoint HTTP `/embeddings` dal gateway o da brain neural
  service che consumi `OnnxRunner` direttamente (oggi `packages/embeddings`
  è una libreria pura, nessun servizio la espone).
- Rust client (mcp-core) verso quell'endpoint per consumo da agent runs.
- Cache layer (Redis) per evitare re-embedding di chunk identici.

## 6.2 RAG pipeline — PARZIALE

**Esistente** in `packages/rag/src/`:
- `ingestion.ts` (88 righe) — pipeline ingest (chunk + embed + store)
- `retrieval.ts` (72 righe) — dense retrieval
- `hybrid-search.ts` (154 righe) — BM25 + dense hybrid

**Da fare**:
- Wiring con Qdrant (collection per progetto) — già nel compose, manca
  config loader nel pacchetto.
- Wiring con Postgres `pgvector` (mig per la tabella `documents_embeddings`).
- Endpoint `/rag/query` esposto dal gateway TS.
- Test integrazione con Qdrant testcontainers.

## 6.3 Audit + Langfuse — QUASI COMPLETO

**Esistente** in `packages/audit/src/`:
- `langfuse-client.ts` (103 righe) — `LangfuseTracer` completo (init lazy,
  hash dei prompt invece di plaintext, sessionId/tenantId nei trace, flush).
- `anomaly-detector.ts` (145 righe) — anomaly scoring on token usage / latency
- `audit-writer.ts` (62 righe) — persistenza audit record
- `dlp-scanner.ts` (70 righe) — pattern jailbreak/prompt-injection robusti
- `logger.ts` (123 righe) — Pino logger structured
- Test (3 suite): 19 test passano

**Da fare**:
- Wiring `LangfuseTracer` in `LLMGateway.invoke()` post-response.
- Migrazione DB `audit_records` (campi: hash prompt/response, tenant_id,
  redaction_applied, dlp_blocked, cost_usd).
- Endpoint admin `/api/admin/audit?from=...&to=...` per query trace.

## 6.4 Redaction Presidio — INTRODOTTO IN QUESTO COMMIT

**Aggiunto**:
- `brain/redaction/__init__.py` + `client.py` (~200 righe) — client Python
  async verso Presidio analyzer + anonymizer. URL letti da env
  (`PRESIDIO_ANALYZER_URL` default `http://presidio-analyzer:5002`,
  `PRESIDIO_ANONYMIZER_URL` default `http://presidio-anonymizer:5001`,
  `PRESIDIO_TIMEOUT_S` default 10s).
- `brain/tests/test_redaction.py` — 5 test (mock httpx + network-error).
- Container Presidio (analyzer + anonymizer) gia' presenti in
  `infra/docker/docker-compose.onprem.yml`.

**Da fare**:
- Wiring nel pre-prompt scan del gateway TS (`packages/llm-gateway`):
  pre-request scan + tag entità rilevate; post-response scan + reject
  se PII leak (in modalità strict).
- Profilo policy `config/policies/onprem.yaml` con flag
  `redaction_enabled: true` e lista `entities_to_redact`.
- Smoke test contro container Presidio in `pnpm verify` (richiede
  `docker compose -f infra/docker/docker-compose.onprem.yml --profile redaction up -d presidio presidio-anonymizer`).

## 6.5 vLLM portability — INFRASTRUTTURA PRONTA

**Esistente** in `infra/docker/docker-compose.onprem.yml`:
- Service `vllm`: image `vllm/vllm-openai:latest`, model
  `${VLLM_MODEL:-Qwen/Qwen2.5-Coder-32B-Instruct}`, GPU NVIDIA reservation,
  port 8000, healthcheck.
- Service `vllm-cpu` (profile `cpu-test`): variante CPU per CI / dev
  senza GPU, model `Qwen2.5-Coder-7B-Instruct`, port 8001.
- Volume `vllm-models` persistente per cache HuggingFace.

**Da fare**:
- Provider Python `brain/providers/vllm_provider.py` (OpenAI-compatible
  endpoint, basta riusare l'SDK OpenAI con `base_url` override).
- Routing: voce `nexus_routing_matrix` per profilo `onprem` che instrada
  a `(provider='vllm', model_id='qwen-2.5-coder-32b')`.
- Test smoke: avvio profile `cpu-test`, ping `/v1/models`, una chat completion
  semplice.

## Roadmap riassuntiva

| Sotto-fase | Stato | Effort residuo |
|---|---|---|
| 6.1 Embeddings | parziale lib | ~4-6h (endpoint + Rust client) |
| 6.2 RAG | parziale lib | ~6-8h (Qdrant/pgvector wiring + endpoint) |
| 6.3 Audit/Langfuse | quasi completo | ~2-3h (wiring gateway + mig DB) |
| 6.4 Redaction | client introdotto | ~3-4h (wiring gateway + policy) |
| 6.5 vLLM | container pronto | ~2-3h (provider Python + test) |

Totale residuo: ~17-24h di dev per chiudere completamente Fase 6.

Tutte le sotto-fasi sono indipendenti — possono essere lavorate in parallelo
da team diversi.
