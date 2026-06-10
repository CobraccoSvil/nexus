#!/bin/bash
# Installa le unit systemd --user del meta-progetto Nexus (deploy/systemd/*.service).
# One-shot, idempotente. Pattern ADR 0028 (richiede nexus-user-manager attivo).
#
# Effetto:
#   - ogni servizio con unit ha auto-restart on-failure;
#   - deploy-local.sh rileva le unit (nexus-<nome>.service) e usa systemctl
#     al posto di nohup per stop/start;
#   - log invariati su /tmp/nexus-*.log + journalctl --user -u nexus-<nome>.
#
# Uso: bash deploy/install-nexus-units.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UNIT_SRC_DIR="${ROOT}/deploy/systemd"
UNIT_DIR="${HOME}/.config/systemd/user"

# Prerequisito ADR 0028: il manager systemd --user deve esistere (linger).
if ! systemctl --user show-environment >/dev/null 2>&1; then
    echo "ERRORE: systemd --user manager non raggiungibile." >&2
    echo "Esegui prima: bash deploy/install-user-manager.sh (ADR 0028)" >&2
    exit 1
fi

# LISTA ESPLICITA delle unit per l'ambiente LOCALE (WSL). deploy/systemd/
# contiene ANCHE le unit di produzione (ExecStart=/opt/ideai/bin/...,
# nexus-core, nexus-admin, ...): un glob *.service le installerebbe tutte
# nell'ambiente sbagliato (incidente 2026-06-10: 10 unit prod abilitate in
# --user locale, una in failed). Le unit prod restano gestite da
# bootstrap-prod/deploy-prod.
LOCAL_UNITS=(nexus-brain.service nexus-mcp-core.service)

units=()
for n in "${LOCAL_UNITS[@]}"; do
    if [ ! -f "${UNIT_SRC_DIR}/${n}" ]; then
        echo "ERRORE: unit locale attesa ma mancante: ${UNIT_SRC_DIR}/${n}" >&2
        exit 1
    fi
    units+=("${UNIT_SRC_DIR}/${n}")
done

mkdir -p "$UNIT_DIR"
for src in "${units[@]}"; do
    install -m 0644 "$src" "${UNIT_DIR}/$(basename "$src")"
done
systemctl --user daemon-reload

# Bootstrap del symlink stabile per mcp-core (la unit esegue
# target/nexus-current/mcp-core; i deploy successivi lo aggiornano da soli).
# Sceglie il binario piu' recente tra release e debug.
mkdir -p "${ROOT}/target/nexus-current"
chosen=""
for b in "${ROOT}/target/release/mcp-core" "${ROOT}/target/debug/mcp-core"; do
    if [ -x "$b" ] && { [ -z "$chosen" ] || [ "$b" -nt "$chosen" ]; }; then
        chosen="$b"
    fi
done
if [ -n "$chosen" ]; then
    ln -sfn "$chosen" "${ROOT}/target/nexus-current/mcp-core"
fi

for src in "${units[@]}"; do
    unit="$(basename "$src")"
    name="${unit#nexus-}"; name="${name%.service}"
    systemctl --user enable "$unit"

    # Transizione dal processo nohup legacy: va fermato PRIMA di avviare la
    # unit, altrimenti la porta risulta occupata e la unit va in restart-loop.
    # NB: se la unit e' GIA' attiva, il processo che matcha il pattern E' quello
    # della unit stessa (non un legacy): killarlo innescherebbe Restart= +
    # restart dell'installer in race. In quel caso si salta direttamente al
    # restart finale.
    case "$name" in
        brain)
            legacy_pattern='brain\.grpc_server\.main' ;;
        *)
            legacy_pattern="target/(debug|release)/${name}([[:space:]]|\$)" ;;
    esac
    if [ "$(systemctl --user is-active "$unit" 2>/dev/null)" != "active" ] \
        && pgrep -f "$legacy_pattern" >/dev/null 2>&1; then
        echo "==> ${name}: fermo il processo legacy (nohup) per la transizione..."
        pkill -TERM -f "$legacy_pattern" || true
        for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
            pgrep -f "$legacy_pattern" >/dev/null 2>&1 || break
            sleep 1
        done
        pkill -9 -f "$legacy_pattern" 2>/dev/null || true
    fi

    # Avvio solo se l'ExecStart e' eseguibile (per mcp-core serve il symlink:
    # se manca il binario, il primo deploy lo creera' e avviera' la unit).
    exec_path=$(grep -m1 '^ExecStart=' "$src" | cut -d= -f2- | awk '{print $1}')
    if [ -x "$exec_path" ] || command -v "${exec_path}" >/dev/null 2>&1; then
        systemctl --user restart "$unit"
        echo "==> ${unit}: installata, abilitata e avviata."
    else
        echo "==> ${unit}: installata e abilitata (NON avviata: ${exec_path} mancante — il prossimo deploy la avviera')."
    fi
done

echo ""
systemctl --user list-units --no-pager 'nexus-*' | head -8
