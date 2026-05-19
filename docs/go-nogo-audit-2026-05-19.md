# Go/No-Go audit statico — 2026-05-19

Risultato esecuzione `scripts/go-nogo-audit.sh` sul branch
`chore/backlog-closure`. L'audit verifica MECCANICAMENTE quali item della
checklist `docs/go-nogo-checklist.md` hanno l'artefatto corrispondente
nel repo. Non sostituisce review umani o test live; serve a tagliare il
tempo del Tech Lead nel rilevare item con artefatti mancanti.

## Risultato

  27 OK (artefatto presente, automaticamente verificato)
  14 DEPLOY-bound (richiedono server attivo: test live, benchmark, infra)
   4 HUMAN-bound (richiedono review/firme: DPIA, DPA, playbook GDPR)
   0 MISSING (tutti gli artefatti citati esistono)

## Item OK statico (27)

| Item | Verifica |
|---|---|
| A4 | model alias resolver: `packages/llm-gateway/src/router/` |
| A5 | smoke + preflight script presenti (Fase 7) |
| B1 | red-team test file presente |
| B2 | DLPScanner implementato (`packages/audit/src/dlp-scanner.ts`) |
| B3 | pattern jailbreak/DAN presenti in dlp-scanner.ts |
| B4 | redaction pipeline Python (Fase 6.4): `brain/redaction/client.py` |
| B6 | `validateTierClaim()` implementato |
| B7 | JWT validation in `crates/nexus-auth` |
| B8 | nessun secret hardcoded rilevato (grep) |
| C1 | rls-policies.sql contiene `ENABLE ROW LEVEL SECURITY` |
| C2 | `FORCE ROW LEVEL SECURITY` presente |
| C3 | tenant-isolation test presente |
| C4 | crypto-shredding implementato |
| C5 | ruolo `nexus_app` definito |
| D1 | LangfuseTracer implementato (Fase 6.3) |
| D2 | `audit_llm_calls` schema definito (`infra/sql/init-schemas.sql`) |
| D3 | anomaly-detector implementato (AnomalyEvent type) |
| D5 | Pino logger structured con request_id/tenant_id |
| D6 | `/health` endpoint implementato in gateway |
| E1 | `scripts/load-test.k6.js` presente |
| F1 | `docs/migration-to-onprem.md` presente |
| F2 | `docs/runbook.md` presente |
| F3 | `docs/security.md` presente |
| G1 | backup/restore script presenti |
| G3 | `.env` in .gitignore e non committato |
| G6 | `rate_limits` schema definito |
| H1 | audit usa hash, non plaintext (verificato in audit/src) |

## Item DEPLOY-bound (14) — richiedono target attivo

A1, A2, A3 — test cross-profilo (deploy hybrid + onprem)
B5 — tier-3 cloud blocking (test cross-profilo)
D4 — Jaeger span (deploy attivo)
E2, E3, E4, E5 — benchmark performance (deploy + load)
G2 — TLS 1.3 (verifica con curl/nmap su endpoint live)
G4 — Vault/Secrets Manager (config production)
G5 — alert Grafana (config runtime)
H2 — retention audit 90gg (config DB live)
H5 — data residency tier-3 (verifica deploy onprem)

## Item HUMAN-bound (4) — richiedono review

F4 — DPIA template — sintesi privacy/legal team
F5 — guida "aggiungere provider" — `docs/contributing.md` extension
H3 — GDPR crypto-shredding playbook — review legale
H4 — DPA firmato con cloud providers (Anthropic, OpenAI, Mistral) — firma legal

## Veto automatici (gate critici)

Item indicati nella checklist come bloccanti il go-live:
- **B1** red team al 100% — file presente ma deve essere eseguito
- **C1** RLS attivo — script presente, ma deve essere applicato in deploy
- **A1** test cross-profile — script presente, deploy required
- **H4** DPA firmato — pending firma legale

Tutti e 4 i veti hanno artefatto presente; nessuno è "missing" nel
codice. Tre richiedono esecuzione/firma per essere chiusi.

## Tool

`scripts/go-nogo-audit.sh` — riesegui in qualsiasi momento per refresh.
Output legenda:
- `[ OK ]`   artefatto presente
- `[DEPLOY]` richiede target attivo
- `[HUMAN]`  richiede review/firma
- `[MISS]`   artefatto mancante (regressione)

## Prossimi passi pre-go-live

1. **Esecuzione test live** (deploy-bound) su staging hybrid + staging onprem.
2. **Review legali** (human-bound): DPA, DPIA, playbook crypto-shredding.
3. **Compilazione firme** in fondo a `docs/go-nogo-checklist.md` (Tech Lead,
   Security Lead, Infra/SRE Lead, Privacy/DPO, Product Owner).
4. **Soglia approvazione**: 4/5 firme, obbligatorie Tech Lead + Security Lead.
