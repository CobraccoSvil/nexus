#!/usr/bin/env bash
# Installa la garanzia del systemd --user manager (Livello 1, ADR 0028).
#
# COSA FA (one-time, richiede sudo una volta):
#   1. Verifica che il Sudo Manager (nexus-sudo-runner) sia gia' installato.
#   2. Risolve utente e UID reali (id -un / id -u), niente assunzione di 1000.
#   3. Genera /etc/systemd/system/nexus-user-manager.service dal template,
#      sostituendo __NEXUS_ADMIN_USER__ / __NEXUS_ADMIN_UID__.
#   4. daemon-reload + enable --now della unit (abilita linger e avvia user@UID).
#   5. Allinea il command_template del purpose sudo 'user-manager-start' all'UID
#      reale (la migrazione 0369 seed con default 1000).
#   6. Valida che user@UID.service risulti active.
#
# COSA NON FA: non installa pacchetti, non tocca container, non concede sudo
# arbitrario (la unit gira come root al boot, ma fa solo enable-linger + start).
#
# RUN:        bash deploy/install-user-manager.sh
# UNINSTALL:  sudo systemctl disable --now nexus-user-manager.service \
#               && sudo rm /etc/systemd/system/nexus-user-manager.service \
#               && sudo systemctl daemon-reload

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UNIT_SRC="${ROOT}/deploy/systemd/nexus-user-manager.service"
UNIT_DST="/etc/systemd/system/nexus-user-manager.service"
RUNNER_BIN="/usr/local/bin/nexus-sudo-runner"

# Utente sotto cui gira mcp-core (quello che ospita i servizi --user di progetto).
TARGET_USER="${SUDO_USER:-$(id -un)}"
TARGET_UID="$(id -u "$TARGET_USER")"

echo "==> Nexus user-manager guarantee — Setup (ADR 0028)"
echo "    Workspace: $ROOT"
echo "    Utente:    $TARGET_USER (uid=$TARGET_UID)"
echo "    Unit:      $UNIT_DST"
echo

# ── 0. Prerequisito: Sudo Manager installato ─────────────────────────────
if [[ ! -x "$RUNNER_BIN" ]]; then
    echo "ERR: $RUNNER_BIN assente. Installa prima il Sudo Manager:" >&2
    echo "     bash deploy/install-sudo-manager.sh" >&2
    exit 1
fi
echo "==> [0/5] Sudo Manager presente: $RUNNER_BIN"

if [[ ! -f "$UNIT_SRC" ]]; then
    echo "ERR: template unit non trovato: $UNIT_SRC" >&2
    exit 1
fi

# ── 1. Genera la unit con UID reale ──────────────────────────────────────
echo "==> [1/5] Genero la unit con utente/UID reali..."
TMP_UNIT="$(mktemp)"
sed -e "s|__NEXUS_ADMIN_USER__|${TARGET_USER}|g" \
    -e "s|__NEXUS_ADMIN_UID__|${TARGET_UID}|g" \
    "$UNIT_SRC" > "$TMP_UNIT"
echo "    OK (placeholder sostituiti)."

# ── 2. Installa la unit (richiede sudo) ──────────────────────────────────
echo "==> [2/5] Installo $UNIT_DST (richiede sudo)..."
sudo install -m 0644 -o root -g root "$TMP_UNIT" "$UNIT_DST"
rm -f "$TMP_UNIT"
echo "    OK: $(ls -l "$UNIT_DST")"

# ── 3. daemon-reload + enable --now ──────────────────────────────────────
echo "==> [3/5] daemon-reload + enable --now nexus-user-manager.service..."
sudo systemctl daemon-reload
sudo systemctl enable --now nexus-user-manager.service
echo "    OK: unit abilitata e avviata."

# ── 4. Allinea il purpose sudo all'UID reale ─────────────────────────────
echo "==> [4/5] Allineo il command_template del purpose 'user-manager-start' a uid=$TARGET_UID..."
NEW_CMD="systemctl start user@${TARGET_UID}.service"
PG="docker exec -e PGPASSWORD=nexus ideai-postgres-nexus-1 psql -h localhost -U nexus -d nexus -At"
if $PG -c "SELECT 1" >/dev/null 2>&1; then
    $PG -c "UPDATE nexus_sudo_purposes SET command_template = '${NEW_CMD}', updated_at = NOW() WHERE name = 'user-manager-start';" >/dev/null \
        && echo "    OK: purpose allineato ($NEW_CMD)." \
        || echo "    ATTENZIONE: UPDATE del purpose fallito; verifica manualmente (default 1000 nella mig 0369)."
else
    echo "    ATTENZIONE: DB non raggiungibile via docker exec; se uid != 1000 aggiorna a mano:"
    echo "      UPDATE nexus_sudo_purposes SET command_template='${NEW_CMD}' WHERE name='user-manager-start';"
fi

# ── 5. Validazione ───────────────────────────────────────────────────────
echo "==> [5/5] Validazione..."
sleep 1
if systemctl is-active "user@${TARGET_UID}.service" >/dev/null 2>&1; then
    echo "    OK: user@${TARGET_UID}.service = active."
else
    echo "    ATTENZIONE: user@${TARGET_UID}.service non risulta active. Diagnostica:"
    echo "      systemctl status nexus-user-manager.service --no-pager"
    echo "      systemctl status user@${TARGET_UID}.service --no-pager"
fi
echo
echo "==> Fatto. Il manager utente ripartira' ad ogni boot della distro (anche dopo wsl --shutdown)."
