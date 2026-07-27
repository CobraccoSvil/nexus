# AI Billing, Token Accounting, and Quotas

## Objective

Track AI consumption and cost with clear accountability by:

- user
- project
- user+project scope

and enforce configurable quotas to control production spending.

## Data Model

Migration: `db/migrations/0006_ai_billing.sql`

- `ai_price_catalog`
  - price list by `provider` + `model`
  - input/output cost per million tokens
  - effective date range and enable/disable flags
- `ai_quota_policies`
  - quota scope: `user`, `project`, `user_project`
  - limits by token and/or cost
  - validity window and enable/disable flags
- `ai_usage_ledger`
  - reservation/final accounting entries
  - status flow: `reserved` -> `finalized` or `released` / `rejected`
  - detailed cost fields and metadata for auditability

Also:

- `orchestrator_runs.user_id` added for ownership traceability
- `orchestrator_runs.audit_json` added as canonical audit payload

## Runtime Flow

Main logic: `crates/mcp-core/src/billing.rs` and `crates/mcp-core/src/orchestrator.rs`.

1. Orchestrator computes prompt tokens and estimated completion tokens.
2. `reserve_usage` checks active quotas and writes `reserved` ledger entry.
3. If quota is exceeded, write `rejected` entry and skip provider.
4. On provider failure, mark reservation as `released`.
5. On provider success, extract usage, compute final costs, mark `finalized`.

This guarantees each AI execution attempt leaves a billing trail.

## API Endpoints

### Admin

- `GET /api/admin/billing/prices`
- `POST /api/admin/billing/prices`
- `PUT /api/admin/billing/prices/:id`
- `GET /api/admin/billing/quotas`
- `POST /api/admin/billing/quotas`
- `PUT /api/admin/billing/quotas/:id`
- `GET /api/admin/billing/usage`

### Authenticated user

- `GET /api/billing/usage/me`
- `GET /api/projects/:id/billing/usage`

## Admin UI

- New page: `/admin/billing`
- File: `apps/web-ide/app/admin/billing/page.tsx`
- Features:
  - usage report by date range
  - price insertion
  - quota insertion
  - live overview of configured prices/quotas

## Convenzione dei token di prompt (e stacco Anthropic del 2026-07-27)

`ai_usage_ledger.prompt_tokens` e' il prompt **LORDO**: i due conteggi
`cache_read_tokens` e `cache_creation_tokens` ne sono un dettaglio, mai addendi.
Lo scorporo (a quanti token si applica la tariffa piena) avviene in un posto solo,
`nexus_pricing::calculate_cost_breakdown`. Chi aggrega:

- input lordo della chiamata = `prompt_tokens`
- input a tariffa piena = `prompt_tokens - cache_read_tokens - cache_creation_tokens`
  (clamp a >= 0)
- cache hit-rate = `cache_read_tokens / prompt_tokens`

La vista `ai_usage_analytics_view` applica queste formule dalla migrazione
`db/migrations/0644_ai_usage_analytics_view_prompt_lordo.sql`; la 0405 le
calcolava sulla premessa opposta (prompt netto) e da li' doppio-contava i
`cache_read`.

**Discontinuita' per Anthropic, dal 2026-07-27.** Fino a quella data l'adapter
Anthropic scriveva nel ledger `usage.input_tokens` del wire, che per Anthropic e'
il NETTO (le quantita' di cache arrivano come campi separati). Ora le somma, come
per ogni altro provider. Conseguenze:

- le righe Anthropic anteriori allo stacco hanno `prompt_tokens` e `total_tokens`
  piu' bassi a parita' di chiamata, quindi i trend che attraversano la data
  mostrano un gradino che non e' un aumento di consumo;
- sulle righe anteriori l'hit-rate calcolato dalla vista risulta sovrastimato
  (denominatore piccolo). Il dato non e' ricostruibile: i token veri di quelle
  chiamate non sono nel ledger;
- le quote (`ai_quota_policies`) dal deploy misurano per Anthropic il consumo
  reale invece di quello sottostimato: i limiti a token si raggiungono prima di
  quanto la serie storica lasciasse prevedere.

## Notes for Other AI Agents

- Base currency setting key: `billing_base_currency`
- Quota enforcement is done before provider completion call.
- For reliable reporting, aggregate from `ai_usage_ledger` with `status='finalized'`.
- Rejected and released rows are still useful for diagnostics and forecasting.
