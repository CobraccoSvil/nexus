---
id: c0527359-12ac-41d6-9bcf-e124ea949e56
kind: changelog
title: "feat(knowledge): Knowledge Base per-progetto Obsidian-compatible"
slug: featknowledge-knowledge-base-per-progetto-obsidian-compatible
tags:
  - changelog
source_commit: 59c7fc33686e0beb41c63f025e096fa39258c563
source_files:
  - apps/web-ide/components/chat-panel.tsx
  - apps/web-ide/components/ide-shell.tsx
  - apps/web-ide/components/knowledge/graph-tab.tsx
  - apps/web-ide/components/knowledge/knowledge-panel.tsx
  - apps/web-ide/components/knowledge/note-detail.tsx
  - apps/web-ide/components/knowledge/notes-tab.tsx
  - apps/web-ide/components/knowledge/search-tab.tsx
  - apps/web-ide/components/knowledge/similar-request-banner.tsx
  - apps/web-ide/components/knowledge/tags-tab.tsx
  - apps/web-ide/components/sidebar/sidebar-manager.tsx
  - apps/web-ide/lib/api-client.ts
  - apps/web-ide/lib/i18n.tsx
  - apps/web-ide/lib/project-dispatcher/store.ts
  - apps/web-ide/lib/project-dispatcher/types.ts
  - crates/mcp-core/src/chat_messages.rs
  - crates/mcp-core/src/knowledge/mod.rs
  - crates/mcp-core/src/knowledge/routes.rs
  - crates/mcp-core/src/knowledge/vault.rs
  - crates/mcp-core/src/knowledge_watcher.rs
  - crates/mcp-core/src/knowledge_workers.rs
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/projects/deep_analyze.rs
  - crates/mcp-core/src/vector_memory.rs
  - crates/nexus-events/src/classifier.rs
  - crates/nexus-events/src/event.rs
  - db/migrations/0175_knowledge_base.sql
  - deploy/deploy-local.sh
auto_generated: true
created_at: 2026-05-23T07:20:01Z
updated_at: 2026-05-23T07:20:01Z
nexus_meta_version: 1
---

# feat(knowledge): Knowledge Base per-progetto Obsidian-compatible

**Commit**: `59c7fc33686e0beb41c63f025e096fa39258c563` (2026-05-23 07:20 UTC)

**Significance**: 0.95

## File toccati

- `apps/web-ide/components/chat-panel.tsx`
- `apps/web-ide/components/ide-shell.tsx`
- `apps/web-ide/components/knowledge/graph-tab.tsx`
- `apps/web-ide/components/knowledge/knowledge-panel.tsx`
- `apps/web-ide/components/knowledge/note-detail.tsx`
- `apps/web-ide/components/knowledge/notes-tab.tsx`
- `apps/web-ide/components/knowledge/search-tab.tsx`
- `apps/web-ide/components/knowledge/similar-request-banner.tsx`
- `apps/web-ide/components/knowledge/tags-tab.tsx`
- `apps/web-ide/components/sidebar/sidebar-manager.tsx`
- `apps/web-ide/lib/api-client.ts`
- `apps/web-ide/lib/i18n.tsx`
- `apps/web-ide/lib/project-dispatcher/store.ts`
- `apps/web-ide/lib/project-dispatcher/types.ts`
- `crates/mcp-core/src/chat_messages.rs`
- `crates/mcp-core/src/knowledge/mod.rs`
- `crates/mcp-core/src/knowledge/routes.rs`
- `crates/mcp-core/src/knowledge/vault.rs`
- `crates/mcp-core/src/knowledge_watcher.rs`
- `crates/mcp-core/src/knowledge_workers.rs`
- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/projects/deep_analyze.rs`
- `crates/mcp-core/src/vector_memory.rs`
- `crates/nexus-events/src/classifier.rs`
- `crates/nexus-events/src/event.rs`
- `db/migrations/0175_knowledge_base.sql`
- `deploy/deploy-local.sh`

## Cosa cambia

feat(knowledge): Knowledge Base per-progetto Obsidian-compatible

## Riferimenti

- Vedi diff git: `git show 59c7fc33686e0beb41c63f025e096fa39258c563`
