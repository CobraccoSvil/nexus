#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add scripts/start-mcp-core.sh \
        scripts/check-panels.sh \
        scripts/diag-toolrunner.sh \
        scripts/check-build-after-pop.sh \
        scripts/resolve-conflicts.sh \
        scripts/verify-conflicts-resolved.sh \
        scripts/commit-stash-pop.sh \
        scripts/commit-toolrunner-fix.sh

git commit -m "$(cat <<'EOF'
fix(scripts): cleanup robusto :50071 in start-mcp-core (fix ToolRunner port conflict)

Sintomo osservato dopo deploy-local.sh + start-mcp-core.sh manuale:
mcp-core scriveva in loop ogni 2-30s:

  ToolRunner server terminato con errore (tentativo N/6): transport error
  ToolRunner: retry tra 2s
  ... fino a "raggiunto limite 6 tentativi, arresto definitivo"

Conseguenza: l'agente non puo' eseguire tool (write_file, run_command,
ecc.) perche' il ToolRunner gRPC non parte.

Root cause: due istanze mcp-core simultanee:
  PID 2281602 (vecchio, lanciato da start-mcp-core.sh + setsid)
  PID 2370561 (nuovo, dal deploy)
Il vecchio teneva `:50071` (ToolRunner gRPC). Il nuovo riusciva a bindare
`:4000` (HTTP API) ma fallivasaranno tutti i bind di `:50071`.

Il `pkill -f mcp-core` del deploy-local.sh non raggiungeva il vecchio
perche' setsid lo aveva distaccato dal gruppo bash padre, e il `pkill`
ha latency tra trovata-PID e effettivo-SIGKILL.

Fix: aggiunto in `start-mcp-core.sh` cleanup esplicito di tutte le porte
del processo PRIMA del nuovo spawn:

  pkill -9 -f "target/release/mcp-core" || true
  fuser -k -9 50071/tcp || true
  fuser -k -9 4000/tcp  || true
  sleep 2

`fuser -k` chiude i file descriptor della porta a livello kernel,
indipendentemente da PID/gruppo del processo che la tiene.

Script di diagnosi aggiunti (utili per future regressioni):
- `scripts/check-panels.sh` — smoke degli endpoint dei pannelli web-ide.
- `scripts/diag-toolrunner.sh` — log ToolRunner + porta + env vars.
- altri da Fase 8 live audit.
EOF
)"

git log -1 --stat | head -10
