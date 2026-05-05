# Nexus LLM Gateway — Architettura di Sicurezza

## 1. Threat Model

Le minacce principali contro un gateway LLM multi-tenant:

| Minaccia | Vettore | Contromisura |
|---|---|---|
| Esfiltrazione PII verso provider cloud | Prompt contenente dati sensibili | Pre-flight redaction pipeline |
| Prompt injection / jailbreak | Input malevolo dell'utente | DLPScanner (11 pattern injection) |
| Rigurgito di segreti dal RAG | Output del modello | DLPScanner post-response |
| Accesso cross-tenant ai dati | Bug applicativo o ID forgiato | Postgres RLS + TenantContext |
| Underdeclaration del tier | Client dichiara tier 0 per dati tier 3 | `validateTierClaim()` nel PolicyEngine |
| Token flooding / cost attack | Rate anomala di richieste | Rate limiter sliding window + AnomalyDetector |
| Furto JWT / API key | Intercettazione o leak del token | Token short-lived (1h), HS256, scope enforcement |
| Compromissione tenant | Breach chiave o credenziali | Crypto-shredding — chiave → dati irrecuperabili |

---

## 2. Layer di Difesa (Defense in Depth)

```
Request → [1] JWT Auth → [2] Rate Limit → [3] Sensitivity Classify
        → [4] Policy Engine → [5] Pre-flight Redaction
        → [6] Provider Call (cloud/vllm)
        → [7] DLP Post-response → [8] Audit Record → [9] Anomaly Detection
```

### Layer 1 — JWT Authentication
- Token HS256, scadenza 1h, claim obbligatori: `tid`, `uid`, `scp`
- `JWTService.requireScope()` verifica lo scope per ogni endpoint
- Rotazione del `JWT_SECRET` invalida immediatamente tutti i token

### Layer 2 — Rate Limiting
- Sliding window in memoria per tenant (1000 req/min) e per provider (500 req/min)
- `RateLimitError` (429) con `retryAfterMs`

### Layer 3 — Sensitivity Classification
- `SecretScanner`: 14 pattern regex (AWS, GCP, GitHub PAT, PEM, IBAN, CF, carta di credito, email...)
- `PresidioClient`: NER via microservizio Python per PII non strutturata
- `classifySync()` nel path streaming (bassa latenza)

### Layer 4 — Policy Engine
- Routing tier → provider da YAML (`config/policies/`)
- `validateTierClaim()`: blocca se tier dichiarato < tier rilevato
- Tier 3 bloccato in profilo cloud per default

### Layer 5 — Pre-flight Redaction
- `RedactionPipeline`: SecretScanner + Presidio + CodeAnonymizer
- `RedactionMap` con TTL — placeholder `__NEXUS_TYPE_N__` sostituiti post-risposta
- Solo per provider cloud — vLLM non invia dati fuori dall'infrastruttura

### Layer 6 — Provider Isolation
- Ogni provider è un adapter isolato dietro `LLMProvider` interface
- Il codice applicativo non conosce quale provider viene usato
- `tier_compatibility` impedisce routing di dati ad alta sensibilità verso provider incompatibili

### Layer 7 — DLP Post-response
- `DLPScanner.assertSafeResponse()`: il modello non deve aver rigurgitato segreti tier-3
- 11 pattern injection nel prompt originale (pre-call)
- `DLPBlockedError` blocca la risposta prima di inviarla al client

### Layer 8 — Audit Trail
- SHA-256 di prompt e response — mai testo in chiaro nei log
- `audit_llm_calls` con retention configurabile (default 90gg)
- `AuditWriter` non-blocking — il fallback del log non impatta la response

### Layer 9 — Anomaly Detection
- Token spike (>50k token/min per tenant)
- Rate spike (>200 req/min)
- Escalation tier progressiva (5+ request con tier crescente verso 3)
- Injection pattern ripetuto dallo stesso tenant
- `content_filter` come finish reason

---

## 3. Isolamento Multi-tenant

### Postgres Row-Level Security
```sql
CREATE POLICY tenant_isolation ON audit_llm_calls
  USING (tenant_id = current_setting('app.current_tenant_id', true));
```

`TenantContext.withTenant(id, fn)` usa `SET LOCAL` all'interno di ogni transazione — il valore viene resettato automaticamente alla fine della transazione, impedendo cross-tenant leakage nel pool di connessioni.

### Encryption at Rest
- `TenantCrypto` (AES-256-GCM) con chiave per-tenant
- IV random per ogni cifratura → no nonce reuse
- **Crypto-shredding**: `shredTenant(id)` distrugge la chiave, rendendo irrecuperabili tutti i blob cifrati senza toccare i record DB

### Vector Isolation
La tabella `embeddings` ha RLS attivo: il retrieval di un tenant non può mai toccare gli embedding di un altro tenant, anche con bug applicativo o ID forgiato.

---

## 4. Secret Management

| Segreto | Storage raccomandato | Rotazione |
|---|---|---|
| `JWT_SECRET` | HashiCorp Vault / AWS Secrets Manager | 90gg |
| API key provider | Vault / K8s Secret | Su compromissione |
| Chiavi di cifratura tenant | Vault KMS / AWS KMS | Mai (usa data key rotation) |
| `LANGFUSE_SECRET` | Vault | 180gg |
| `POSTGRES_PASSWORD` | Vault | 90gg |

**Regola**: nessun segreto in `.env` in produzione. `.env` solo per sviluppo locale (escluso da git via `.gitignore`).

---

## 5. Compliance

### GDPR
- Dati PII mai in chiaro nei log (solo hash SHA-256)
- Retention configurabile per `audit_llm_calls` (default 90gg)
- Crypto-shredding implementato per rispettare il diritto all'oblio (Art. 17)
- Redaction obbligatoria prima di ogni invio a provider cloud

### Dati in transito
- TLS 1.3 obbligatorio verso provider cloud (garantito dagli SDK ufficiali)
- Comunicazione interna (vLLM, Presidio) su rete privata Docker/K8s

### Audit
- Ogni chiamata LLM produce un record in `audit_llm_calls`
- Il record include: tenant, utente, feature, tier, provider, token count, latenza, flag redaction/DLP
- I record sono interrogabili per compliance review senza esporre dati sensibili

---

## 6. Security Testing

| Test type | Coverage | Tool |
|---|---|---|
| Red team injection | 108 vettori | `tests/red-team.test.ts` |
| Secret detection | 14 pattern | `tests/red-team.test.ts` |
| Cross-tenant isolation | JWT + RLS + Crypto | `tests/tenant-isolation.test.ts` |
| DLP scanner | 9 test | `packages/audit/tests/dlp-scanner.test.ts` |
| Load test | 100 req/s, p95 < 3s | `scripts/load-test.k6.js` |

Eseguire il red team test ad ogni release:
```bash
pnpm --filter @nexus/llm-gateway test -- --reporter=verbose tests/red-team.test.ts
```
