---
id: 74df40bd-2119-44c4-927f-37a9328618b8
kind: other
title: NexusAutoFix (PR automatiche)
slug: auto-fix-workflow
tags:
  - concept
  - autofix
  - ci
  - pr
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:09:00Z
updated_at: 2026-06-04T10:32:59Z
nexus_meta_version: 1
---

# NexusAutoFix

Worker che intercetta fallimenti E2E e propone fix via PR GitHub automatiche.

## Trigger

1. `NexusE2eSmokeWorker` esegue suite Playwright `apps/web-ide/e2e/nexus-self/`
2. Se un test fallisce: row in `nexus_e2e_runs` con `status='failed'`
3. `nexus_autofix_worker` (periodico 5 min) intercetta failure non ancora processati
4. Crea `change_drafts` con `trigger_kind='autofix'`

## Workflow (futuro: PR automatiche)

Vedi [[change-drafter]] per la pipeline di approvazione.

Il piano completo (step futuro) prevede:
- Worktree git in `/tmp/nexus-autofix-<uuid>`
- Apply patch via `edit_file`/`write_file`
- `pnpm verify` automatico
- Commit + push branch `nexus-autofix/<data>-<slug>`
- `gh pr create --base main`

## Tabelle

- `nexus_e2e_runs` - run di smoke test
- `change_drafts` - proposte di fix

Vedi [[postgres-tables]].
