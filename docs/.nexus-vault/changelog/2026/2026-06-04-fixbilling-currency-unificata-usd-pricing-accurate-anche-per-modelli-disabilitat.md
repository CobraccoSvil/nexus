---
id: 33f1858a-26f9-4b35-a4e3-85d0730c7ed4
kind: changelog
title: "fix(billing): currency unificata USD + pricing accurate anche per modelli disabilitati"
slug: fixbilling-currency-unificata-usd-pricing-accurate-anche-per-modelli-disabilitat
tags:
  - changelog
source_commit: 7d9d1215a8bb89c888b5514784e554c431782edc
source_files:
  - apps/web-ide/app/admin/billing/page.tsx
  - crates/mcp-core/src/billing.rs
  - db/migrations/0294_billing_currency_usd_and_reset.sql
auto_generated: true
created_at: 2026-06-04T12:22:50Z
updated_at: 2026-06-04T12:22:48Z
nexus_meta_version: 1
---

# fix(billing): currency unificata USD + pricing accurate anche per modelli disabilitati

**Commit**: `7d9d1215a8bb89c888b5514784e554c431782edc` (2026-06-04 12:22 UTC)

**Significance**: 0.69

## File toccati

- `apps/web-ide/app/admin/billing/page.tsx`
- `crates/mcp-core/src/billing.rs`
- `db/migrations/0294_billing_currency_usd_and_reset.sql`

## Cosa cambia

fix(billing): currency unificata USD + pricing accurate anche per modelli disabilitati

## Riferimenti

- Vedi diff git: `git show 7d9d1215a8bb89c888b5514784e554c431782edc`

## Documenti correlati

- [[crates-rust]]
- [[frontend-nextjs]]
- [[postgres-tables]]
- [[migrations-log]]
