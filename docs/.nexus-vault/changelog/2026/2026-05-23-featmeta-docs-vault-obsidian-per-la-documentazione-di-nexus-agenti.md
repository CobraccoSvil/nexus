---
id: 5ea763e8-8742-406d-9f9f-14ef85bfa089
kind: changelog
title: "feat(meta-docs): vault Obsidian per la documentazione di Nexus + agenti"
slug: featmeta-docs-vault-obsidian-per-la-documentazione-di-nexus-agenti
tags:
  - changelog
source_commit: 700dacf37bfb9b980a8121425eb5d66cffcd4ac0
source_files:
  - .claude/agents/_nexus-orchestrator.md
  - .claude/agents/nexus-db-architect.md
  - .claude/agents/nexus-doc-writer.md
  - .claude/agents/nexus-frontend-implementer.md
  - .claude/agents/nexus-python-implementer.md
  - .claude/agents/nexus-rust-implementer.md
  - .claude/agents/nexus-test-author.md
  - apps/web-ide/components/chat/change-draft-card.tsx
  - apps/web-ide/components/knowledge/knowledge-panel.tsx
  - apps/web-ide/components/knowledge/meta-tab.tsx
  - apps/web-ide/e2e/nexus-self/_setup.ts
  - apps/web-ide/e2e/nexus-self/smoke-compact-session.spec.ts
  - apps/web-ide/e2e/nexus-self/smoke-ide-loads.spec.ts
  - apps/web-ide/e2e/nexus-self/smoke-knowledge-panel.spec.ts
  - apps/web-ide/e2e/nexus-self/smoke-meta-docs-api.spec.ts
  - apps/web-ide/lib/api-client.ts
  - crates/mcp-core/src/change_drafts.rs
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/meta_docs/apply.rs
  - crates/mcp-core/src/meta_docs/generators/api.rs
  - crates/mcp-core/src/meta_docs/generators/architecture.rs
  - crates/mcp-core/src/meta_docs/generators/changelog.rs
  - crates/mcp-core/src/meta_docs/generators/decisions.rs
  - crates/mcp-core/src/meta_docs/generators/mod.rs
  - crates/mcp-core/src/meta_docs/generators/schema.rs
  - crates/mcp-core/src/meta_docs/mod.rs
  - crates/mcp-core/src/meta_docs/routes.rs
  - crates/mcp-core/src/meta_docs/vault.rs
  - crates/mcp-core/src/meta_docs_watcher.rs
  - crates/mcp-core/src/meta_docs_workers.rs
  - crates/mcp-core/src/nexus_autofix_worker.rs
  - crates/mcp-core/src/vector_memory.rs
  - db/migrations/0177_nexus_meta_docs.sql
  - docs/.nexus-vault/.obsidian/app.json
  - docs/.nexus-vault/.obsidian/core-plugins.json
  - docs/.nexus-vault/README.md
  - docs/.nexus-vault/adr/0005-meta-docs-vault.md
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/architecture/overview.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featagent-heartbeat-routing-context-aware-infra-error-detection-abcd.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featbrain-migrazione-learning-storage-da-sqlite-a-postgresql.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featbranding-rendering-cobracco-con-bra-in-grassetto.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featclarify-bias-esplora-prima-skip-in-modalita-autonoma-ab.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featescalation-limite-fallback-dinamico-n-provider-idonei-nel-catalog.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featknowledge-knowledge-base-per-progetto-obsidian-compatible.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featlanding-footer-copyright-by-cobracco-con-link-a-cobraccoit.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featlanding-knowledge-base-nella-pagina-di-presentazione-fix-overflow-sidebar.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featlanding-logo-nexus-n-in-campo-viola-nella-navbar-del-sito-statico.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featproject-db-azioni-suggerite-quando-il-db-rilevato-non-e-raggiungibile.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featprovider-resigned-detection-classifier-fix-m-ticket-smart-test-admin.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featui-bottone-compatta-a-larghezza-dinamica-indicatore-provider-live.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featui-context-window-sul-bottone-compatta-chat-rimossa-auto-abort-3min.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-fix-compattazione-chat-usa-routing-matrix-allineamento-tasto-compatta.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-fixauth-login-link-a-authgithub-relativo-no-localhost-hardcoded.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-fixbraingoogle-cache-client-per-event-loop-risolve-event-loop-is-closed.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-fixbrainopenai-supporto-gpt-5-family-max-completion-tokens-e-block-pulito-modell.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-fixcompact-bottone-allargato-refresh-messages-intercetta-riassunti-degeneri.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-fixknowledge-layout-responsive-sidebar-knowledge-base.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-fixrun-panel-wizard-install-systemd-feedback-in-modale-re-fetch-sempre.md
  - docs/.nexus-vault/schema/migrations-log.md
  - docs/.nexus-vault/schema/postgres-tables.md
  - docs/.nexus-vault/schema/qdrant-collections.md
  - lefthook.yml
auto_generated: true
created_at: 2026-05-23T09:24:06Z
updated_at: 2026-05-23T09:24:06Z
nexus_meta_version: 1
---

# feat(meta-docs): vault Obsidian per la documentazione di Nexus + agenti

**Commit**: `700dacf37bfb9b980a8121425eb5d66cffcd4ac0` (2026-05-23 09:24 UTC)

**Significance**: 0.95

## File toccati

- `.claude/agents/_nexus-orchestrator.md`
- `.claude/agents/nexus-db-architect.md`
- `.claude/agents/nexus-doc-writer.md`
- `.claude/agents/nexus-frontend-implementer.md`
- `.claude/agents/nexus-python-implementer.md`
- `.claude/agents/nexus-rust-implementer.md`
- `.claude/agents/nexus-test-author.md`
- `apps/web-ide/components/chat/change-draft-card.tsx`
- `apps/web-ide/components/knowledge/knowledge-panel.tsx`
- `apps/web-ide/components/knowledge/meta-tab.tsx`
- `apps/web-ide/e2e/nexus-self/_setup.ts`
- `apps/web-ide/e2e/nexus-self/smoke-compact-session.spec.ts`
- `apps/web-ide/e2e/nexus-self/smoke-ide-loads.spec.ts`
- `apps/web-ide/e2e/nexus-self/smoke-knowledge-panel.spec.ts`
- `apps/web-ide/e2e/nexus-self/smoke-meta-docs-api.spec.ts`
- `apps/web-ide/lib/api-client.ts`
- `crates/mcp-core/src/change_drafts.rs`
- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/meta_docs/apply.rs`
- `crates/mcp-core/src/meta_docs/generators/api.rs`
- `crates/mcp-core/src/meta_docs/generators/architecture.rs`
- `crates/mcp-core/src/meta_docs/generators/changelog.rs`
- `crates/mcp-core/src/meta_docs/generators/decisions.rs`
- `crates/mcp-core/src/meta_docs/generators/mod.rs`
- `crates/mcp-core/src/meta_docs/generators/schema.rs`
- `crates/mcp-core/src/meta_docs/mod.rs`
- `crates/mcp-core/src/meta_docs/routes.rs`
- `crates/mcp-core/src/meta_docs/vault.rs`
- `crates/mcp-core/src/meta_docs_watcher.rs`
- `crates/mcp-core/src/meta_docs_workers.rs`
- `crates/mcp-core/src/nexus_autofix_worker.rs`
- `crates/mcp-core/src/vector_memory.rs`
- `db/migrations/0177_nexus_meta_docs.sql`
- `docs/.nexus-vault/.obsidian/app.json`
- `docs/.nexus-vault/.obsidian/core-plugins.json`
- `docs/.nexus-vault/README.md`
- `docs/.nexus-vault/adr/0005-meta-docs-vault.md`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/architecture/overview.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featagent-heartbeat-routing-context-aware-infra-error-detection-abcd.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featbrain-migrazione-learning-storage-da-sqlite-a-postgresql.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featbranding-rendering-cobracco-con-bra-in-grassetto.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featclarify-bias-esplora-prima-skip-in-modalita-autonoma-ab.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featescalation-limite-fallback-dinamico-n-provider-idonei-nel-catalog.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featknowledge-knowledge-base-per-progetto-obsidian-compatible.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featlanding-footer-copyright-by-cobracco-con-link-a-cobraccoit.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featlanding-knowledge-base-nella-pagina-di-presentazione-fix-overflow-sidebar.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featlanding-logo-nexus-n-in-campo-viola-nella-navbar-del-sito-statico.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featproject-db-azioni-suggerite-quando-il-db-rilevato-non-e-raggiungibile.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featprovider-resigned-detection-classifier-fix-m-ticket-smart-test-admin.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featui-bottone-compatta-a-larghezza-dinamica-indicatore-provider-live.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featui-context-window-sul-bottone-compatta-chat-rimossa-auto-abort-3min.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-fix-compattazione-chat-usa-routing-matrix-allineamento-tasto-compatta.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-fixauth-login-link-a-authgithub-relativo-no-localhost-hardcoded.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-fixbraingoogle-cache-client-per-event-loop-risolve-event-loop-is-closed.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-fixbrainopenai-supporto-gpt-5-family-max-completion-tokens-e-block-pulito-modell.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-fixcompact-bottone-allargato-refresh-messages-intercetta-riassunti-degeneri.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-fixknowledge-layout-responsive-sidebar-knowledge-base.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-fixrun-panel-wizard-install-systemd-feedback-in-modale-re-fetch-sempre.md`
- `docs/.nexus-vault/schema/migrations-log.md`
- `docs/.nexus-vault/schema/postgres-tables.md`
- `docs/.nexus-vault/schema/qdrant-collections.md`
- `lefthook.yml`

## Cosa cambia

feat(meta-docs): vault Obsidian per la documentazione di Nexus + agenti

## Riferimenti

- Vedi diff git: `git show 700dacf37bfb9b980a8121425eb5d66cffcd4ac0`
