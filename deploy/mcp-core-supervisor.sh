#!/usr/bin/env bash
# mcp-core supervisor — mitigazione SENZA sudo del gap "mcp-core non si rialza".
#
# Il services_watchdog interno a mcp-core (crates/mcp-core/src/services_watchdog.rs)
# rialza gli ALTRI microservizi (brain, web-ide, gateway, admin/chat/...), ma NON
# puo' rialzare mcp-core stesso: ci vive dentro, quindi se mcp-core cade muore
# anche il watchdog. Questo supervisor ESTERNO colma esattamente quel gap:
#   - probe TCP periodico della porta di mcp-core;
#   - se down per N cicli consecutivi, riavvio via deploy-local.sh --service mcp-core;
#   - cooldown tra i riavvii per evitare loop.
#
# In produzione il ruolo spetta a systemd (Restart=always). In dev/WSL senza
# privilegi sudo questo script e' la rete equivalente. Detached + single-instance
# (flock): un solo supervisor attivo. Avviato da deploy-local.sh (start mcp-core)
# e ri-lanciabile a mano: `setsid nohup bash deploy/mcp-core-supervisor.sh >/dev/null 2>&1 < /dev/null &`
set -u

ROOT="${NEXUS_REPO_ROOT:-/home/administrator/ideai}"
PORT="${MCP_CORE_PORT:-4000}"
INTERVAL="${SUPERVISOR_INTERVAL_S:-15}"
FAIL_THRESHOLD="${SUPERVISOR_FAIL_THRESHOLD:-2}"
# 120s: maggiore del tempo di startup tipico di mcp-core (che all'avvio attende
# il Neural Core/brain), cosi' il supervisor non interrompe un avvio in corso
# innescando un loop di restart.
COOLDOWN="${SUPERVISOR_COOLDOWN_S:-120}"
LOCK="/tmp/nexus-mcp-supervisor.lock"
LOG="/tmp/nexus-mcp-supervisor.log"

# Single-instance: chi non ottiene il lock esce subito (evita supervisor doppi).
exec 9>"$LOCK"
if ! flock -n 9; then
    echo "$(date -Is) supervisor gia' in esecuzione, esco" >> "$LOG"
    exit 0
fi

echo "$(date -Is) mcp-core supervisor avviato (porta=$PORT interval=${INTERVAL}s threshold=$FAIL_THRESHOLD cooldown=${COOLDOWN}s)" >> "$LOG"

probe() {
    # TCP probe via /dev/tcp di bash: nessuna dipendenza esterna.
    (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && { exec 3>&- 3<&-; return 0; }
    return 1
}

down=0
last_restart=0
while true; do
    if probe; then
        if [ "$down" -gt 0 ]; then
            echo "$(date -Is) mcp-core RIPRISTINATO (porta $PORT)" >> "$LOG"
        fi
        down=0
    else
        down=$((down + 1))
        if [ "$down" -ge "$FAIL_THRESHOLD" ]; then
            now=$(date +%s)
            if [ $((now - last_restart)) -ge "$COOLDOWN" ]; then
                echo "$(date -Is) mcp-core DOWN da $down cicli -> riavvio (deploy-local.sh --service mcp-core)" >> "$LOG"
                # 9>&- chiude il fd del lock nel figlio: senza, deploy-local.sh e
                # il nuovo mcp-core EREDITEREBBERO il lock, tenendolo occupato
                # anche dopo la morte del supervisor e bloccando i futuri avvii.
                ( cd "$ROOT" && setsid nohup bash "$ROOT/deploy/deploy-local.sh" --service mcp-core \
                    > /tmp/nexus-supervisor-restart.log 2>&1 < /dev/null 9>&- & )
                last_restart="$now"
                down=0
            fi
        fi
    fi
    sleep "$INTERVAL"
done
