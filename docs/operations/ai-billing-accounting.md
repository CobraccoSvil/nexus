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

## Notes for Other AI Agents

- Base currency setting key: `billing_base_currency`
- Quota enforcement is done before provider completion call.
- For reliable reporting, aggregate from `ai_usage_ledger` with `status='finalized'`.
- Rejected and released rows are still useful for diagnostics and forecasting.
