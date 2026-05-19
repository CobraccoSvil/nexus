#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai

git add db/migrations/0168_agent_meta_steps.sql \
        db/migrations/0169_clarify_or_expand.sql \
        scripts/start-mcp-core.sh \
        scripts/start-webide.sh \
        scripts/nexus-status.sh \
        scripts/nexus-health-final.sh \
        scripts/debug-webide.sh \
        scripts/commit-startup-fix.sh

git commit -m "$(cat <<'EOF'
chore(startup): recovery migrazioni 168/169 da stash + script avvio

A sessione iniziata avevamo stashato il WIP utente (`wip-pre-backlog-closure-2026-05-19`)
che includeva le migrazioni 0168 (`agent_meta_steps`) e 0169 (`clarify_or_expand`).
mcp-core all'avvio applicava queste migrazioni al DB; quando abbiamo
stashato i file, la storia in `_sqlx_migrations` non e' stata invertita.

Risultato: al primo deploy del branch `chore/backlog-closure` mcp-core
falliva con `Error: migration 168 was previously applied but is missing
in the resolved migrations`, bloccando anche le nuove 0170/0171.

Fix: recovery dei file 0168/0169 dalla parte untracked dello stash
(`git checkout stash@{0}^3 -- db/migrations/016{8,9}_*.sql`). Le migrazioni
sono additive (CREATE TABLE meta_steps + clarify nodes), non distruttive,
e quindi entrano in main come parte del lavoro WIP utente che entra
naturalmente con questo branch.

Dopo il recovery la catena 168 -> 171 e' consecutiva e sqlx applica
correttamente 170 (capability JSONB su `ai_price_catalog`) e 171 (purpose
model `provider_test_connection.anthropic`, `admin.tool_selection`).

Tool aggiunti per gestione avvio:
- `scripts/start-mcp-core.sh` — start del binario Rust con polling porta.
- `scripts/start-webide.sh` — start Next.js (NB: per detach affidabile
  usare il background runner della shell parent — `nohup`+`pnpm exec`
  da WSL/PowerShell session viene SIGKILL-ato).
- `scripts/nexus-status.sh` — health probe rapido dei 10 servizi.
- `scripts/nexus-health-final.sh` — verifica migrazioni applicate + dati
  popolati (capability thinking, purpose model).
- `scripts/debug-webide.sh` — diagnosi rapida Next.js.

Stato finale: 10/10 servizi up, migrazioni 168-171 applicate.
EOF
)"

git log -1 --stat | head -15
