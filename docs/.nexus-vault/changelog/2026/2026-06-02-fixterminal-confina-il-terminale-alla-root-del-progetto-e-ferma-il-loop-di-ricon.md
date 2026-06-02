---
id: 847eff01-2df4-4d2c-a976-6a88c7e588dc
kind: changelog
title: "fix(terminal): confina il terminale alla root del progetto e ferma il loop di riconnessione"
slug: fixterminal-confina-il-terminale-alla-root-del-progetto-e-ferma-il-loop-di-ricon
tags:
  - changelog
source_commit: 12bbfd87690c670e5497201ddb89e106bd5265ff
source_files:
  - apps/web-ide/components/terminal-panel.tsx
  - brain/grpc_server/main.py
  - brain/tests/test_terminal_token_auth.py
  - crates/mcp-core/src/project_workspace/workbench.rs
auto_generated: true
created_at: 2026-06-02T14:22:44Z
updated_at: 2026-06-02T14:22:43Z
nexus_meta_version: 1
---

# fix(terminal): confina il terminale alla root del progetto e ferma il loop di riconnessione

**Commit**: `12bbfd87690c670e5497201ddb89e106bd5265ff` (2026-06-02 14:22 UTC)

**Significance**: 0.51

## File toccati

- `apps/web-ide/components/terminal-panel.tsx`
- `brain/grpc_server/main.py`
- `brain/tests/test_terminal_token_auth.py`
- `crates/mcp-core/src/project_workspace/workbench.rs`

## Cosa cambia

fix(terminal): confina il terminale alla root del progetto e ferma il loop di riconnessione

## Riferimenti

- Vedi diff git: `git show 12bbfd87690c670e5497201ddb89e106bd5265ff`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[frontend-nextjs]]
