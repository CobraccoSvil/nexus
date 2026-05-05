# Nexus — Piano di implementazione architettura LLM ibrida

**Obiettivo**: costruire il sottosistema AI di Nexus in modo che funzioni immediatamente con provider esterni (Anthropic, OpenAI, Mistral) ma sia **trasportabile in toto** su infrastruttura LLM interna al cliente (self-hosted vLLM) attraverso il cambio di un solo adapter e di variabili di configurazione.

**Principio guida**: nessuna parte del codice applicativo deve sapere *quale* provider sta usando. Tutto passa dietro un `LLMGateway` unificato. Il giorno del porting on-premise, il diff è localizzato in `src/llm/providers/` e in `.env`.

---

## 1. Principi architetturali non negoziabili

1. **Provider abstraction first**: mai chiamare SDK di provider direttamente dal codice applicativo. Solo il `LLMGateway` parla con i provider.
2. **OpenAI-compatible API come lingua franca**: ogni adapter (cloud o self-hosted) espone la stessa interfaccia interna modellata sull'API OpenAI Chat Completions (che è anche l'API nativa di vLLM/TGI, quindi il porting è a costo zero).
3. **Sensitivity-aware routing**: ogni chiamata LLM passa da un classificatore che assegna un tier e sceglie il provider compatibile con quel tier. Il routing è data-driven, non hardcoded.
4. **Zero leak per default**: embedding, reranker, OCR, classificatori leggeri girano sempre localmente. Solo la *generazione* va in cloud.
5. **PII redaction obbligatoria**: tutto ciò che esce verso un provider esterno passa dal modulo di redaction. Non si fa bypass neanche "temporaneo".
6. **Configuration over code**: la scelta dei provider, la mappa tier→provider, le policy di redaction sono configurazione caricata a runtime, non codice.
7. **Audit trail completo**: ogni chiamata LLM produce un record strutturato. Hash dei payload, mai payload in chiaro nei log.
8. **Feature flags per ogni capability cloud**: ogni funzione che invoca provider esterni può essere disabilitata da config. Un cliente può partire con "solo self-hosted" senza toccare il codice.

---

## 2. Stack tecnico

**Linguaggio principale**: TypeScript (Node.js 20+), coerente con lo stack Nexus esistente.
**Framework API**: Fastify (performance, schema validation nativa, hook lifecycle puliti).
**Package manager**: pnpm con workspace monorepo.
**Database**: Postgres 16 con estensioni `pgvector` (embedding) e `pg_trgm` (BM25-like).
**Cache/queue**: Redis 7 per rate limiting, cache risposte, dead letter queue.
**Embedding engine**: ONNX Runtime via `onnxruntime-node` (TypeScript) nella prima fase. Migrazione futura a `ort` Rust come crate nativo quando il collo di bottiglia lo giustifichi.
**Modelli embedding**: `BAAI/bge-m3` (multilingua, ottimo su italiano, 1024 dim).
**Reranker**: `BAAI/bge-reranker-v2-m3` via ONNX.
**PII redaction**: Microsoft Presidio via microservizio Python dedicato (gRPC o REST interno) — Presidio è maturo e italiano-ready, più pragmatico che reimplementarlo in TS.
**Secret scanning**: `@secretlint/secretlint` o wrapper su gitleaks binary.
**Observability**: OpenTelemetry (OTLP) + Langfuse self-hosted per LLM tracing.
**Testing**: Vitest (unit), Playwright (integration end-to-end sul gateway).

---

## 3. Struttura del monorepo

```
nexus/
├── packages/
│   ├── llm-gateway/              ← cuore: astrazione provider + routing
│   │   ├── src/
│   │   │   ├── providers/         ← un file per adapter
│   │   │   │   ├── anthropic.ts
│   │   │   │   ├── openai.ts
│   │   │   │   ├── mistral.ts
│   │   │   │   ├── bedrock.ts
│   │   │   │   ├── azure-openai.ts
│   │   │   │   ├── vllm-local.ts  ← self-hosted target
│   │   │   │   └── base.ts        ← interfaccia comune
│   │   │   ├── router/
│   │   │   │   ├── sensitivity-classifier.ts
│   │   │   │   ├── policy-engine.ts
│   │   │   │   └── fallback-chain.ts
│   │   │   ├── redaction/
│   │   │   │   ├── presidio-client.ts
│   │   │   │   ├── secret-scanner.ts
│   │   │   │   ├── code-anonymizer.ts
│   │   │   │   └── redaction-map.ts
│   │   │   ├── gateway.ts         ← entry point unico
│   │   │   └── types.ts
│   │   └── package.json
│   │
│   ├── embeddings/                ← sempre locale
│   │   ├── src/
│   │   │   ├── onnx-runner.ts
│   │   │   ├── chunker.ts
│   │   │   └── reranker.ts
│   │   └── package.json
│   │
│   ├── rag/
│   │   ├── src/
│   │   │   ├── ingestion.ts
│   │   │   ├── retrieval.ts
│   │   │   ├── hybrid-search.ts
│   │   │   └── index.ts
│   │   └── package.json
│   │
│   ├── audit/
│   │   ├── src/
│   │   │   ├── logger.ts
│   │   │   ├── dlp-scanner.ts
│   │   │   └── anomaly-detector.ts
│   │   └── package.json
│   │
│   └── shared/
│       ├── src/
│       │   ├── config.ts          ← loader Zod-validated
│       │   ├── telemetry.ts
│       │   └── errors.ts
│       └── package.json
│
├── apps/
│   ├── api/                       ← Fastify server che espone il gateway
│   ├── worker/                    ← job asincroni (ingestion, reindexing)
│   └── classifier-svc/            ← Python microservice per Presidio
│
├── infra/
│   ├── docker/
│   │   ├── docker-compose.cloud.yml    ← stack con solo provider esterni
│   │   ├── docker-compose.hybrid.yml   ← stack misto
│   │   └── docker-compose.onprem.yml   ← stack full self-hosted + vLLM
│   ├── k8s/                       ← helm charts per deploy produzione
│   └── terraform/                 ← IaC provisioning
│
├── config/
│   ├── providers.schema.json      ← JSON schema config provider
│   ├── policies/
│   │   ├── default.yaml
│   │   ├── strict.yaml            ← clienti regolamentati
│   │   └── onprem-only.yaml
│   └── prompts/
│       └── system/*.md
│
└── docs/
    ├── architecture.md
    ├── security.md
    ├── runbook.md
    └── migration-to-onprem.md
```

---

## 4. Il pattern chiave — Provider Abstraction Layer

Questa è la parte da cui dipende tutto. Va fatta **bene** e subito, perché è l'unica che rende il sistema trasportabile.

### 4.1 Interfaccia comune

```typescript
// packages/llm-gateway/src/providers/base.ts

export interface LLMMessage {
  role: "system" | "user" | "assistant" | "tool";
  content: string | LLMContentBlock[];
  tool_call_id?: string;
  tool_calls?: LLMToolCall[];
}

export interface LLMRequest {
  model: string;              // alias logico, es. "coder-large", non "claude-opus-4"
  messages: LLMMessage[];
  temperature?: number;
  max_tokens?: number;
  tools?: LLMToolDefinition[];
  response_format?: "text" | "json" | { type: "json_schema"; schema: object };
  stream?: boolean;
  metadata: {
    tenant_id: string;
    user_id: string;
    request_id: string;
    sensitivity_tier: 0 | 1 | 2 | 3;
    feature: string;          // es. "code-review", "doc-generation"
  };
}

export interface LLMResponse {
  content: string;
  tool_calls?: LLMToolCall[];
  usage: { input_tokens: number; output_tokens: number };
  model_used: string;         // modello reale usato, per audit
  provider_used: string;      // provider reale usato, per audit
  latency_ms: number;
  finish_reason: "stop" | "length" | "tool_calls" | "content_filter";
}

export interface LLMProvider {
  readonly name: string;
  readonly supports_tools: boolean;
  readonly supports_streaming: boolean;
  readonly max_context_tokens: number;
  readonly tier_compatibility: (0 | 1 | 2 | 3)[];

  complete(req: LLMRequest): Promise<LLMResponse>;
  stream(req: LLMRequest): AsyncIterable<LLMStreamChunk>;
  healthcheck(): Promise<boolean>;
}
```

Nota critica: **il `model` nella request è un alias logico**, non un identificativo provider-specifico. `"coder-large"` viene risolto in `claude-sonnet-4` oppure `qwen-2.5-coder-32b` in base al provider attivo. Questa è la cerniera che permette il porting.

### 4.2 Model alias mapping

```yaml
# config/model-aliases.yaml

aliases:
  coder-small:
    cloud_primary: openai/gpt-4o-mini
    cloud_secondary: mistral/codestral-latest
    onprem: qwen-2.5-coder-7b
    min_tier: 0
    max_tier: 1

  coder-large:
    cloud_primary: anthropic/claude-sonnet-4
    cloud_secondary: openai/gpt-4o
    onprem: qwen-2.5-coder-32b
    min_tier: 0
    max_tier: 2

  reasoning-heavy:
    cloud_primary: anthropic/claude-opus-4
    cloud_secondary: openai/o3
    onprem: deepseek-r1-distill-70b
    min_tier: 0
    max_tier: 2

  sensitive-only:
    cloud_primary: null           # esplicitamente proibito al cloud
    cloud_secondary: null
    onprem: qwen-2.5-72b
    min_tier: 3
    max_tier: 3
```

### 4.3 Adapter vLLM (già pronto per il giorno del porting)

vLLM espone nativamente un endpoint OpenAI-compatible. L'adapter è quasi identico all'adapter OpenAI:

```typescript
// packages/llm-gateway/src/providers/vllm-local.ts

export class VLLMProvider implements LLMProvider {
  readonly name = "vllm-local";
  readonly supports_tools = true;
  readonly supports_streaming = true;
  readonly tier_compatibility = [0, 1, 2, 3] as const;

  constructor(private config: {
    base_url: string;          // es. http://vllm-service:8000/v1
    api_key?: string;          // vLLM lo accetta ma non lo verifica
    max_context_tokens: number;
  }) {}

  async complete(req: LLMRequest): Promise<LLMResponse> {
    // usa lo stesso client dell'adapter OpenAI, solo con base_url diverso
    // ...
  }
}
```

Giorno del porting: aggiungi `VLLMProvider` al registry, aggiorni la policy, deploy. Fine.

---

## 5. Fasi di implementazione

### FASE 0 — Bootstrap (giorni 1-3)

- Setup monorepo pnpm con workspace
- CI/CD pipeline base (GitHub Actions o GitLab CI): lint, test, build, container image
- Docker Compose per dev locale: Postgres+pgvector, Redis, classifier-svc stub
- Config loader con Zod schema strict
- Struttura `config/` con esempi per i tre profili (cloud, hybrid, onprem)
- Telemetry setup: OpenTelemetry SDK, export OTLP verso collector locale
- Template PR, commit conventions, ADR (Architecture Decision Records) in `docs/adr/`

**Acceptance**: `pnpm dev` avvia lo stack, health endpoint risponde, un test smoke passa in CI.

### FASE 1 — LLM Gateway con provider esterni (giorni 4-10)

- Implementa interfaccia `LLMProvider` e tipi comuni
- Adapter `AnthropicProvider` (SDK ufficiale `@anthropic-ai/sdk`)
- Adapter `OpenAIProvider` (SDK ufficiale `openai`)
- Adapter `MistralProvider` (via OpenAI-compatible endpoint su La Plateforme)
- Model alias resolver basato su `config/model-aliases.yaml`
- Gateway singleton con retry, timeout, circuit breaker (libreria `cockatiel` o `opossum`)
- Streaming support per tutti e tre gli adapter (SSE)
- Tool calling unificato (normalizza schema JSON tra i provider)
- Health checks periodici per ogni provider con marking automatico come unhealthy
- Test di contract per ogni adapter: stesso input → output conforme all'interfaccia

**Acceptance**: una call `gateway.complete({ model: "coder-large", ... })` funziona identicamente cambiando solo la config del provider primario. Test E2E con 3 provider in parallelo.

### FASE 2 — Sensitivity classifier e routing (giorni 11-15)

- Microservizio Python per PII detection via Presidio con endpoint gRPC (più veloce di REST per questo caso d'uso)
- Secret scanner TypeScript con pattern per: AWS keys, GCP keys, Azure keys, GitHub PAT, JWT, private keys (PEM), connection strings DB
- Classificatore sensitivity: combinazione regole (match di pattern sensibili) + modello piccolo fine-tunato (opzionale in fase 2, può essere solo regole inizialmente)
- Policy engine: YAML che mappa `(tier, feature, tenant_flags) → allowed_providers`
- Fallback chain: se primario fallisce/non compatibile, prova secondario, poi rifiuta con errore strutturato
- Rate limiting per tenant e per provider (Redis sliding window)

**Acceptance**: un prompt tier 3 con dati PII non raggiunge mai un provider esterno, e il rifiuto è tracciato. Test dedicato con prompt contenenti CF, email, API key reali (di test) che verifica il blocco.

### FASE 3 — Redaction pipeline (giorni 16-20)

- Pre-flight redaction: prima di ogni chiamata a provider esterno, il prompt passa da Presidio + secret scanner + code anonymizer
- Redaction map in memoria per singola request (con TTL breve), reidratata in post-flight sulla risposta
- Code anonymizer: usa tree-sitter per parsing AST, rinomina identificatori custom marcati con `@confidential` o dentro file in whitelist, preservando semantica
- Whitelist di directory/file/pattern che "possono uscire così come sono" (es. documentazione pubblica, boilerplate)
- Blacklist hard di directory che **non possono mai uscire** (es. `.env`, `secrets/`, `customers/*/private/`)
- Test golden: corpus di 50+ esempi con output atteso della redaction

**Acceptance**: nessun pattern sensibile noto attraversa la redaction in un test di 1000 prompt sintetici. Telemetria conta le redactions per tipo.

### FASE 4 — Embedding e retrieval locale (giorni 21-28)

- `onnxruntime-node` con modello `bge-m3` scaricato e cached localmente (non chiamare HuggingFace a runtime in produzione, bundle o pull da artifact registry)
- Tokenizer via `@xenova/transformers` o `tokenizers` wrapper
- Chunker structure-aware: usa tree-sitter per codice, parser Markdown per docs, custom per docx (riutilizza codice pipeline Cobracco)
- Ingestion pipeline: trigger da Supabase (o endpoint interno) → chunk → embed → upsert pgvector
- Retrieval ibrido: similarity search pgvector + BM25 con `pg_trgm` + reranker `bge-reranker-v2-m3`
- Metadati filter per tenant_id, project_id, sensitivity, vigenza (domain-specific)
- Cache query embedding in Redis (TTL 1h)

**Acceptance**: ingest di 10k chunk < 5 min, query retrieval < 200ms p95 con reranking. Nessuna chiamata esterna nel path di retrieval.

### FASE 5 — Audit, DLP, anomaly detection (giorni 29-33)

- Structured logger (Pino) con hook che produce record audit per ogni chiamata LLM
- Hash SHA-256 di prompt e response invece del testo in chiaro nei log permanenti
- Record dettagliato in DB dedicato `audit_llm_calls` con retention configurabile (default 90gg)
- DLP scanner post-response: riesegue secret scanner sull'output del modello, blocca se trova pattern (il modello potrebbe aver rigurgitato qualcosa presente nel RAG)
- Langfuse integration per observability LLM: trace complete, session grouping, eval tracking
- Anomaly detection: volumi token anomali, pattern di query sospetti (jailbreak attempts via pattern matching su injection notori), rate anomali

**Acceptance**: dashboard Langfuse mostra trace completi per ogni call, audit DB interrogabile, alert su anomalie configurati.

### FASE 6 — Tenant isolation e access control (giorni 34-38)

- Postgres row-level security su tutte le tabelle con `tenant_id`
- Vector collections isolate per tenant (schema Postgres per tenant, oppure namespace)
- API key management: ogni client usa token JWT short-lived emessi dall'auth service, con claim tenant + scopes
- Encryption at rest con chiavi per-tenant (AWS KMS o HashiCorp Vault a seconda del deploy target)
- Crypto-shredding playbook documentato: cancellazione tenant = distruzione chiave + eliminazione record

**Acceptance**: test automatico verifica che tenant A non possa mai retrievare chunk di tenant B, neanche con ID forgiato o bug esotici (fuzzing).

### FASE 7 — Portabilità on-premise (giorni 39-45)

- Adapter `VLLMProvider` implementato e testato contro una vLLM reale
- `docker-compose.onprem.yml`: stack completo con vLLM container (modello Qwen 2.5 Coder 32B per default), nessun riferimento a servizi cloud
- Profilo di configurazione `onprem-only.yaml` che disabilita tutti i provider esterni
- Helm chart per Kubernetes on-premise con GPU node selector
- Script di smoke test che valida: gateway up, retrieval funzionante, una call end-to-end completata
- Documento `migration-to-onprem.md` con procedura step-by-step
- Test E2E completo contro lo stack on-premise in CI (su runner con GPU o con modello piccolo di test)

**Acceptance**: lo stesso test suite E2E passa identico contro profilo `cloud`, `hybrid`, `onprem`. Zero modifiche al codice applicativo tra i tre.

### FASE 8 — Hardening finale e go-live (giorni 46-50)

- Red team interno: tentativi di prompt injection, jailbreak, esfiltrazione dati, bypass redaction
- Load test: target 100 req/s sostenute con latenza p95 < 3s end-to-end
- Runbook incident response in `docs/runbook.md`: scenari (provider down, breach sospetto, saturazione GPU, DB lento)
- Documentazione utente: come configurare un nuovo tenant, come attivare profilo onprem, come aggiungere un nuovo provider
- DPIA template compilato per il deploy cloud-based
- DPA templates per i tre provider principali

**Acceptance**: go/no-go meeting con checklist di 30+ item firmata. Documentazione completa in `docs/`.

---

## 6. Configurazione per i tre profili

Lo stesso binary, tre `.env` + tre YAML diversi. Nessun rebuild necessario per passare da uno all'altro.

### Profilo `cloud` (default, day one)
- Provider attivi: Anthropic (primary), OpenAI (secondary), Mistral (tertiary)
- Embedding: locale (sempre)
- Tutti i tier 0-2 routati al cloud, tier 3 **rifiutato** con errore esplicito (non ancora supportato)

### Profilo `hybrid` (dopo 3-6 mesi)
- Provider attivi: cloud (come sopra) + `vllm-local` per tier 3 e come fallback
- Tier 0-1 → cloud
- Tier 2 → cloud se disponibile, altrimenti locale
- Tier 3 → solo locale

### Profilo `onprem` (consegna al cliente)
- Provider attivi: solo `vllm-local` (uno o più istanze con modelli diversi)
- Cloud providers completamente esclusi dalla registry, non solo disabilitati
- Nessuna dipendenza network verso esterno

---

## 7. Variabili d'ambiente critiche

```bash
# Profilo
NEXUS_PROFILE=cloud                           # cloud | hybrid | onprem

# Gateway
NEXUS_LLM_POLICY_FILE=/config/policies/default.yaml
NEXUS_MODEL_ALIASES_FILE=/config/model-aliases.yaml

# Provider esterni (ignorati se profile=onprem)
ANTHROPIC_API_KEY=***
ANTHROPIC_BASE_URL=https://api.anthropic.com          # override per Bedrock: https://bedrock-runtime.eu-central-1.amazonaws.com
OPENAI_API_KEY=***
OPENAI_BASE_URL=https://api.openai.com/v1             # override per Azure: https://<resource>.openai.azure.com/openai/deployments/<deployment>
MISTRAL_API_KEY=***

# vLLM locale (usato se profile=hybrid | onprem)
VLLM_BASE_URL=http://vllm:8000/v1
VLLM_API_KEY=internal-only-token
VLLM_MODEL_NAME=Qwen/Qwen2.5-Coder-32B-Instruct

# Servizi locali
POSTGRES_URL=postgres://...
REDIS_URL=redis://...
PRESIDIO_GRPC_URL=presidio:50051
LANGFUSE_HOST=http://langfuse:3000
LANGFUSE_SECRET_KEY=***

# Encryption
KMS_PROVIDER=aws                              # aws | vault | local
KMS_KEY_ARN=arn:aws:kms:...

# Feature flags
NEXUS_ALLOW_CLOUD_TIER2=true
NEXUS_ALLOW_CLOUD_TIER3=false
NEXUS_REDACTION_STRICT=true
NEXUS_DLP_ENABLED=true
```

---

## 8. Testing strategy

**Unit test**: ogni adapter, redactor, classifier. Coverage minima 80% su `packages/llm-gateway/`.

**Contract test**: suite di test che ogni `LLMProvider` deve passare identicamente. Garantisce che aggiungere un provider nuovo non rompa niente.

**Integration test**: gateway end-to-end con provider reali (chiavi di test separate per CI).

**Security test**: suite dedicata con 200+ prompt di attacco (injection, exfiltration, jailbreak noti) che deve sempre passare.

**E2E cross-profile**: stesso test suite eseguito contro profilo `cloud`, `hybrid`, `onprem` in CI. Se uno passa e un altro no, è un bug di portabilità e blocca il merge.

**Load test**: k6 con scenari realistici (mix di richieste tier diversi), target 100 req/s sostenute.

---

## 9. Observability minima

- **OpenTelemetry** traces: ogni richiesta è un trace con span per classifier, redaction, retrieval, provider call, DLP, audit write
- **Langfuse** per LLM-specific tracing e eval
- **Prometheus** metrics: req/s per provider, latency p50/p95/p99, token usage per tenant, redactions per tipo, rate errors per provider
- **Grafana dashboards** pre-configurate in `infra/grafana/`: overview, provider health, tenant usage, security events
- **Alert rules**: provider down > 5 min, error rate > 5%, latency p95 > 10s, DLP block spike, redaction rate anomalo

---

## 10. Percorso di migrazione on-premise (quando un cliente lo richiede)

1. Cliente firma contratto con clausola on-premise → rilascio hardware ordinato in parallelo
2. Setup cluster Kubernetes (o Docker Compose se scala piccola) su infrastruttura cliente
3. Deploy stack via Helm con `values-onprem.yaml`
4. Pull modelli open-weight (Qwen 2.5 Coder 32B default) da registry interno o HF mirror
5. Import backup pgvector dal deploy cloud (se migrazione di tenant esistente)
6. Smoke test end-to-end via script `scripts/onprem-smoke.sh`
7. Cutover: update DNS / proxy client
8. Monitoring 72h con team di supporto
9. Handover a IT cliente con runbook

**Tempo stimato**: 2-4 settimane dal firma contratto al go-live, di cui 1-2 per hardware delivery e setup infra, 1 per deploy applicativo e smoke test, 1 per UAT e cutover.

---

## 11. Checklist deliverable per Claude Code

Quando passi questo piano a Claude Code, chiedigli di procedere in questo ordine, con gate espliciti:

- [ ] Fase 0 completa e CI verde
- [ ] Fase 1: gateway con 3 provider cloud, test contract verdi
- [ ] Fase 2: routing + classifier, 100% dei test sensitivity passano
- [ ] Fase 3: redaction, corpus golden passa a 100%
- [ ] Fase 4: embedding + retrieval locali, performance target raggiunti
- [ ] Fase 5: audit + DLP, Langfuse integrato
- [ ] Fase 6: tenant isolation, fuzzing cross-tenant passa
- [ ] **Gate critico**: Fase 7 — dimostrare che lo stesso test suite passa identico su profilo `cloud` e `onprem`. Se non passa, il sistema non è portabile e va rifattorizzato prima di andare avanti.
- [ ] Fase 8: red team + load test + documentazione

**Non saltare fasi.** Il valore di questo piano è proprio nella disciplina di costruire l'astrazione corretta fin dall'inizio. Se Claude Code propone di "semplificare" evitando l'interfaccia `LLMProvider` nella fase 1 per andare più veloce, dì di no: è esattamente ciò che compromette la portabilità.

---

## 12. Decisioni architetturali da validare prima di partire

Questi punti hanno alternative ragionevoli — decidi tu consapevolmente prima di farli partire:

1. **TypeScript vs Go per il gateway**: TS è coerente con stack Nexus, Go è più performante per proxy I/O-bound. Scegli TS a meno che non preveda > 1000 req/s.
2. **Presidio come microservice Python vs porting in TS**: Python è pragmatico e maturo, aggiunge un container. Porting in TS è più uniforme ma richiede 2-3 settimane extra.
3. **Langfuse self-hosted vs cloud**: self-hosted è coerente con posture on-premise, cloud è zero-ops. Scegli self-hosted da subito se il target finale è on-premise.
4. **Redis vs solo Postgres per rate limit/cache**: Redis è standard, Postgres LISTEN/NOTIFY + tabelle sono fattibili e riducono dipendenze in onprem. Mantieni Redis per semplicità.
5. **vLLM vs TGI vs llama.cpp server per l'on-premise**: vLLM per throughput e OpenAI-compat, TGI buona alternativa, llama.cpp per deploy minimali (< 20 utenti). Default vLLM.

---

**Ultima nota**: questo piano presuppone 45-50 giorni di lavoro full-time di un dev senior o 2-3 mesi per un team di due dev mid-level. Se hai vincoli temporali più stretti, tagliare è possibile in quest'ordine di priorità: prima la fase 8 (hardening), poi la 5 (audit) ridotta all'essenziale, mai la 3 (redaction) né la 7 (portabilità) — sono il cuore della proposta di valore.
