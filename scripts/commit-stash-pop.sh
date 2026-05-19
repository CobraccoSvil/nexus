#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai

# Stage tutto il merge stash + i miei nuovi script di debug/diagnosi
git add -A

# Esclude artefatti runtime (chrome download, learning.db modifica runtime,
# tsconfig.tsbuildinfo gia' rimosso)
git restore --staged brain/nexus_memory/learning.db 2>/dev/null || true
git checkout brain/nexus_memory/learning.db 2>/dev/null || true
rm -f .coverage apps/web-ide/app/page.tsx.bak.v2 apps/web-ide/package-lock.json 2>/dev/null || true

git status --short | head -30
echo ""

git commit -m "$(cat <<'EOF'
chore: integra WIP stash sul branch chore/backlog-closure

Pop integrale dello stash `wip-pre-backlog-closure-2026-05-19`. Lo stash
conteneva ~40 file modificati + 11 file untracked che erano lavoro in
corso dell'utente prima dell'inizio della sessione (feature dispatcher
event-driven via SSE, meta-steps in chat, clarify/expand condizionale,
fix scrolling pannelli, badge servizi pending).

Senza il pop l'utente vedeva regressioni UI nella sessione corrente
(es. scroll rotto su run-panel per mancanza di `position: relative`).

File integrati (selezione):

UI fix:
- `apps/web-ide/components/panels/run-panel.tsx` — fix scroll
  (`position: relative` su wrapper) + badge "pendingCount" servizi
  rilevati ma non installati + auto-detect ogni 60s.
- `apps/web-ide/components/chat/agent-steps-panel.tsx` — dispatcher SSE.
- `apps/web-ide/components/chat/markdown-renderer.tsx` — exec code blocks.
- `apps/web-ide/components/chat/message-list.tsx` — `extractToolUseBlocks`
  helper + `ToolUseBadges` per tool_use visivi nei messaggi.
- `apps/web-ide/components/chat-panel.tsx` — wiring chat live.
- `apps/web-ide/components/ide-shell.tsx`, `git/source-control-panel.tsx`,
  `lib/{api-client,use-chat}.ts` — dispatcher SSE consumer.

Nuovi file (feature WIP, prima untracked):
- `apps/web-ide/app/api/projects/[projectId]/execute-command/route.ts`
- `apps/web-ide/components/chat/agent-meta-step-card.tsx`
- `apps/web-ide/components/chat/executable-code-block.tsx`
- `brain/agents/clarify_or_expand_node.py`
- `brain/agents/meta_steps.py`
- `crates/mcp-core/src/project_workspace/execute_cmd.rs`

Backend (dispatcher event-driven, mcp-core):
- `brain/grpc_server/main.py`, `brain/agents/{graph,nodes,planner_node,state}.py`
- `crates/mcp-core/src/*` (16 file: agent_tools, chat_messages, dispatcher_routes,
  github, project_workspace/*, security/port_enforcer, task_watchdog, ecc.)
- `crates/nexus-events/src/dispatcher.rs`

Conflitti risolti manualmente:
- `apps/web-ide/components/chat/message-list.tsx`: prese le feature WIP
  (ToolUseBadges + extractToolUseBlocks) MANTENENDO il mio fix Fase 4
  (`ThinkingPanel({ thinking })` senza il param `tc` unused — il body usa
  colori hardcoded).
- `apps/web-ide/tsconfig.tsbuildinfo`: auto-generato, eliminato (rigenera
  al prossimo build).
- `apps/web-ide/components/chat/markdown-renderer.tsx`: rimosso
  `eslint-disable-next-line react-hooks/exhaustive-deps` ora inutile
  (deps complete dopo il merge).

Verifica:
- `cargo check --workspace`: OK
- `python3 -m py_compile` su tutti i file modificati brain/: OK
- `pnpm --filter web-ide typecheck`: OK
- `pnpm --filter web-ide eslint .`: 0 errors 0 warnings

Lo stash entry e' rimosso dalla lista. Tutti i 12 commit locali
pre-sessione su main restano dove sono (saranno catturati al merge
del branch).
EOF
)"

git log -1 --stat | head -10
echo ""
echo "=== Stash list ==="
git stash list
