# Nexus LLM Gateway — Go/No-Go Checklist

Checklist pre-go-live. Ogni item deve essere firmato dal responsabile prima del deploy in produzione.

---

## A. Architettura e portabilità (gate critico Fase 7)

- [ ] **A1** — Lo stesso test suite cross-profilo passa identico su `cloud`, `hybrid`, `onprem` (`pnpm test` verde su tutti e 3)
- [ ] **A2** — Zero modifiche al codice applicativo tra i 3 profili (solo config/env cambia)
- [ ] **A3** — `VLLMProvider` testato contro un endpoint vLLM reale (non mock)
- [ ] **A4** — Model alias resolver mappa correttamente gli alias logici per ogni profilo
- [ ] **A5** — Smoke test on-premise (`scripts/onprem-smoke.sh`) supera 0 failure

---

## B. Sicurezza

- [ ] **B1** — Red team test suite (108 vettori) verde al 100% (`tests/red-team.test.ts`)
- [ ] **B2** — DLP scanner blocca tutti i segreti tier-3 nella response del modello
- [ ] **B3** — Injection patterns (jailbreak, DAN, XML injection) tutti rilevati
- [ ] **B4** — Redaction pipeline: nessun pattern sensibile noto attraversa in chiaro al cloud
- [ ] **B5** — Tier 3 bloccato in profilo cloud (verificato da test + smoke test)
- [ ] **B6** — `validateTierClaim()` blocca underdeclaration (tier 0 dichiarato con PII tier-3)
- [ ] **B7** — JWT: token scaduto e firma alterata vengono rifiutati (test `tenant-isolation`)
- [ ] **B8** — Nessun segreto hardcoded nel codice (audit `git grep -r "api_key\s*=\s*[\"'][^$]"`)

---

## C. Isolamento multi-tenant

- [ ] **C1** — RLS attivo su `audit_llm_calls`, `embeddings`, `rate_limits`, `tenants` (`rowsecurity = t`)
- [ ] **C2** — `FORCE ROW LEVEL SECURITY` attivo (protegge anche il table owner)
- [ ] **C3** — Test cross-tenant: tenant A non può leggere dati di tenant B (test `tenant-isolation`)
- [ ] **C4** — Crypto-shredding testato: dopo `shredTenant()` i blob sono irrecuperabili
- [ ] **C5** — Ruolo `nexus_app` è non-superuser (verifica: `SELECT rolsuper FROM pg_roles WHERE rolname='nexus_app'`)

---

## D. Osservabilità

- [ ] **D1** — Dashboard Langfuse mostra trace complete per ogni chiamata LLM
- [ ] **D2** — Audit DB interrogabile: `SELECT * FROM audit_llm_calls LIMIT 10` ritorna dati
- [ ] **D3** — Anomaly detector: alert configurati per token spike, rate spike, tier escalation
- [ ] **D4** — Jaeger mostra span per ogni layer (classifier, redaction, provider, audit)
- [ ] **D5** — Log strutturati Pino: ogni record ha `request_id`, `tenant_id`, `audit: true`
- [ ] **D6** — Health endpoint risponde `200` con stato di tutti i provider

---

## E. Performance

- [ ] **E1** — Load test k6: 100 req/s sostenute, latenza p95 < 3s (`scripts/load-test.k6.js`)
- [ ] **E2** — Retrieval ibrido (vector + BM25 + reranker) < 200ms p95
- [ ] **E3** — vLLM healthcheck < 15s da cold start del container
- [ ] **E4** — Embedding ingest: 10k chunk < 5 min
- [ ] **E5** — Gateway memory: < 512 MB in stato steady (verificato con `docker stats`)

---

## F. Documentazione

- [ ] **F1** — `docs/migration-to-onprem.md` review completato dal team infra
- [ ] **F2** — `docs/runbook.md` review completato dall'on-call team
- [ ] **F3** — `docs/security.md` review completato dal CISO o security lead
- [ ] **F4** — DPIA completata per il deploy cloud-based (vedi `docs/dpia-template.md`)
- [ ] **F5** — Guida "Come aggiungere un nuovo provider" documentata

---

## G. Infrastruttura

- [ ] **G1** — Backup Postgres schedulato e testato (restore verificato)
- [ ] **G2** — TLS 1.3 attivo su tutti gli endpoint esposti
- [ ] **G3** — Nessun segreto in `.env` committato (`.gitignore` aggiornato)
- [ ] **G4** — `JWT_SECRET` e `LANGFUSE_SECRET` in Vault / Secrets Manager (non in env file)
- [ ] **G5** — Alert Grafana configurati: provider down > 5min, error rate > 5%, latency p95 > 10s
- [ ] **G6** — Rate limit configurato e testato (429 corretto su superamento soglia)

---

## H. Compliance

- [ ] **H1** — PII mai in chiaro nei log (verificato campionando 100 record audit: nessun testo in chiaro)
- [ ] **H2** — Retention `audit_llm_calls` configurata (default 90gg, revisione legale completata)
- [ ] **H3** — Crypto-shredding playbook documentato e testato per diritto all'oblio GDPR
- [ ] **H4** — DPA firmato con i provider cloud (Anthropic, OpenAI, Mistral)
- [ ] **H5** — Data residency verificata: dati tier-3 mai escono dall'infrastruttura on-premise

---

## Firma Go/No-Go

| Ruolo | Nome | Data | Firma |
|---|---|---|---|
| Tech Lead | | | |
| Security Lead / CISO | | | |
| Infra / SRE Lead | | | |
| Privacy / DPO | | | |
| Product Owner | | | |

**Soglia di approvazione**: almeno 4/5 firme, incluse obbligatoriamente Tech Lead e Security Lead.

**Veto automatico** (bloccano il go-live indipendentemente dalle firme):
- B1 (red team) non al 100%
- C1 (RLS) non attivo
- A1 (cross-profile test) non verde
- H4 (DPA) non firmato
