---
id: 429826b0-e4d9-48d0-8827-8b0d6c0714f1
kind: changelog
title: "fix(M14+M16): lifecycle note (draft->active) + intake request-aware + riabilita discovery-first"
slug: fixm14m16-lifecycle-note-draft-active-intake-request-aware-riabilita-discovery-f
tags:
  - changelog
source_commit: c473ab0557618165560bca1fe8a15306e4e1f704
source_files:
  - apps/web-ide/components/knowledge/similar-request-banner.tsx
  - apps/web-ide/lib/api-client.ts
  - brain/agents/clarify_or_expand_node.py
  - crates/mcp-core/src/knowledge/mod.rs
  - crates/mcp-core/src/knowledge/routes.rs
  - db/migrations/0247_discovery_first_reenable.sql
auto_generated: true
created_at: 2026-06-02T07:47:38Z
updated_at: 2026-06-02T07:47:39Z
nexus_meta_version: 1
---

# fix(M14+M16): lifecycle note (draft->active) + intake request-aware + riabilita discovery-first

**Commit**: `c473ab0557618165560bca1fe8a15306e4e1f704` (2026-06-02 07:47 UTC)

**Significance**: 0.74

## File toccati

- `apps/web-ide/components/knowledge/similar-request-banner.tsx`
- `apps/web-ide/lib/api-client.ts`
- `brain/agents/clarify_or_expand_node.py`
- `crates/mcp-core/src/knowledge/mod.rs`
- `crates/mcp-core/src/knowledge/routes.rs`
- `db/migrations/0247_discovery_first_reenable.sql`

## Cosa cambia

fix(M14+M16): lifecycle note (draft->active) + intake request-aware + riabilita discovery-first

## Riferimenti

- Vedi diff git: `git show c473ab0557618165560bca1fe8a15306e4e1f704`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[frontend-nextjs]]
- [[postgres-tables]]
- [[migrations-log]]
- [[rest-endpoints]]
- [[knowledge-base-funzionamento]]
