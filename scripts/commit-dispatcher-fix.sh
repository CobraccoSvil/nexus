#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add deploy/deploy-local.sh \
        apps/web-ide/lib/project-dispatcher/connection.ts \
        scripts/check-panels.sh \
        scripts/check-regressions.sh \
        scripts/check-mcp-core.sh \
        scripts/diag-toolrunner.sh \
        scripts/diag-pool.sh \
        scripts/diag-pg-ports.sh \
        scripts/inspect-project.sh \
        scripts/setup-test-db.sh \
        scripts/setup-test-db-v2.sh \
        scripts/check-build-after-pop.sh \
        scripts/resolve-conflicts.sh \
        scripts/verify-conflicts-resolved.sh \
        scripts/commit-stash-pop.sh \
        scripts/commit-toolrunner-fix.sh \
        scripts/rebuild-webide.sh \
        scripts/force-rebuild.sh \
        scripts/inspect-bundle.sh \
        scripts/inspect-bundle-v2.sh \
        scripts/inspect-ide-chunk.sh \
        scripts/find-dispatcher-chunk.sh \
        scripts/find-event-stream-in-bundle.sh \
        scripts/check-loaded-chunks.sh \
        scripts/commit-dispatcher-fix.sh

git commit -m "$(cat <<'EOF'
fix(deploy): pulisci .next/.turbo prima di pnpm build (fix dispatcher SSE)

Sintomo: dopo il deploy che ha integrato il WIP stash, i pannelli IDE
sembravano "non configurati" ma erano live. L'investigazione via Claude
in Chrome ha rilevato che le API endpoint rispondevano 200 (37 chiamate),
i pannelli si caricavano (DB/Files/Search/Git/Run/Documenti/Monitor),
ma il dispatcher SSE non si attivava:

  console: [dispatcher] connectDispatcher called
           [dispatcher] before fetchSnapshot
           [dispatcher] after fetchSnapshot, calling openStream
           [dispatcher] openStream returned
  network: /api/projects/.../snapshot?topics=*   200
  network: /api/projects/.../event-stream?topics=*   MISSING

Root cause: `pnpm build` post-stash-pop riusava una cache `.next/`
inconsistente. Il chunk del page IDE (`app/ide/page-*.js`) generato dopo
il merge dello stash NON includeva il modulo `lib/project-dispatcher`
nonostante fosse importato da `ide-shell.tsx`. Tree-shaking del bundler
basato su cache stale: il chunk produceva codice per `applySnapshot` (da
store.ts re-exported) ma NON per `useProjectDispatcher` /
`connectDispatcher` / `openStream`.

Verifica grep nel bundle pre-fix:

  useProjectDispatcher: 0 chunks
  connectDispatcher:    0 chunks
  openStream:           0 chunks
  applySnapshot:        1 chunk   ← presente
  event-stream URL:     0 chunks

Dopo `rm -rf .next .turbo && pnpm build` (rebuild pulito):

  /api/projects/.../event-stream:  1 occorrenza nel page chunk
  JobCreated/FileChanged events:   2 occorrenze (handler SSE generati)
  Browser conferma EventSource OPEN sulla chat session live

Fix permanente: `deploy/deploy-local.sh:build_webide()` ora esegue
`rm -rf .next .turbo` PRIMA di `next build`. Costo: ~30s in piu' per
rebuild full (turbo cache persa). Beneficio: niente regressioni
silenziose di tree-shaking dopo merge/stash/branch-switch.

Tool aggiunti in scripts/ per diagnosi futura:
- `check-panels.sh`         — smoke endpoint per pannello
- `check-regressions.sh`    — diff branch + errori log
- `diag-toolrunner.sh`      — diagnosi ToolRunner gRPC :50071
- `diag-pool.sh`            — diagnosi sqlx pool exhaustion
- `diag-pg-ports.sh`        — diagnosi porte Postgres
- `inspect-project.sh`      — root_path + file DB del progetto
- `setup-test-db-v2.sh`     — DB di test su postgres-app:5434
- `force-rebuild.sh`        — pkill + rm -rf .next + pnpm build + start
- `inspect-bundle.sh`       — verifica simboli nel chunk Next
- `check-loaded-chunks.sh`  — quale chunk il browser sta caricando

Verifica end-to-end via Claude in Chrome:
- /ide carica IdeShell completo (test-metasteps)
- 37 /api/* chiamate, 0 errori 4xx/5xx
- Dispatcher SSE: EventSource OPEN per session
- "live" badge presente nel DOM (connection status = open)
EOF
)"

git log -1 --stat | head -15
