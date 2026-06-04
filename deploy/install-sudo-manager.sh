#!/usr/bin/env bash
# Installa il Sudo Manager Livello 1 (ADR 0017).
#
# COSA FA:
#   1. Builda `nexus-sudo-runner` in release.
#   2. Copia il binary a /usr/local/bin/nexus-sudo-runner (root:root, 0755).
#   3. Installa /etc/sudoers.d/nexus-runner con NOPASSWD whitelistato.
#   4. Valida la sintassi con `visudo -c`.
#
# COSA NON FA:
#   - Non installa pacchetti.
#   - Non modifica nient'altro del sistema.
#   - Non concede sudo arbitrario: solo /usr/local/bin/nexus-sudo-runner *
#
# UTENTE: lo script si chiede solo la password SUDO una volta (one-time install).
# Dopo questo setup, mcp-core puo' chiamare i purpose nella whitelist via
# `sudo /usr/local/bin/nexus-sudo-runner <purpose>` senza password.
#
# RUN:  bash deploy/install-sudo-manager.sh
# UNINSTALL: sudo rm /usr/local/bin/nexus-sudo-runner /etc/sudoers.d/nexus-runner

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_SRC="${ROOT}/target/release/nexus-sudo-runner"
BIN_DST="/usr/local/bin/nexus-sudo-runner"
SUDOERS_DST="/etc/sudoers.d/nexus-runner"

# Determina l'utente che fara' chiamate sudo al runner (deve essere quello
# sotto cui gira mcp-core). Default: utente che lancia lo script.
SUDO_USER_NAME="${SUDO_USER:-$(id -un)}"

echo "==> Sudo Manager Livello 1 — Setup"
echo "    Workspace:  $ROOT"
echo "    Utente:     $SUDO_USER_NAME"
echo "    Binary:     $BIN_DST"
echo "    Sudoers:    $SUDOERS_DST"
echo

# ── 1. Build release del runner ──────────────────────────────────────────
echo "==> [1/4] Build nexus-sudo-runner (release)..."
cd "$ROOT"
cargo build -p nexus-sudo-runner --release
if [[ ! -x "$BIN_SRC" ]]; then
    echo "ERR: binary non prodotto: $BIN_SRC" >&2
    exit 1
fi
echo "    OK: $(file "$BIN_SRC" | head -c 80)..."
echo

# ── 2. Copia binary (richiede sudo) ──────────────────────────────────────
echo "==> [2/4] Copia binary in $BIN_DST (richiede sudo)..."
sudo install -m 0755 -o root -g root "$BIN_SRC" "$BIN_DST"
echo "    OK: $(ls -l "$BIN_DST")"
echo

# ── 3. Installa sudoers.d/nexus-runner ──────────────────────────────────
echo "==> [3/4] Installa $SUDOERS_DST..."
TMP_SUDOERS="$(mktemp)"
cat > "$TMP_SUDOERS" <<EOF
# /etc/sudoers.d/nexus-runner — Generato da deploy/install-sudo-manager.sh
#
# Concede NOPASSWD all'utente '$SUDO_USER_NAME' SOLO per il binary
# /usr/local/bin/nexus-sudo-runner. Il runner valida internamente il purpose
# richiesto contro la whitelist nel DB Nexus + allowlist hardcoded di
# programmi (apt-get, systemctl, ...). Nessun comando shell arbitrario.
#
# Defense-in-depth: anche se il DB viene compromesso e nexus_sudo_purposes
# viene riempito di comandi ostili, il runner respinge tutto cio' che non
# matcha PATH_ALLOWLIST e ARG_SAFE_PATTERN (vedi crates/nexus-sudo-runner/src/main.rs).
$SUDO_USER_NAME ALL=(root) NOPASSWD: $BIN_DST *
Defaults!$BIN_DST !requiretty
EOF

sudo install -m 0440 -o root -g root "$TMP_SUDOERS" "$SUDOERS_DST"
rm -f "$TMP_SUDOERS"
echo "    OK: $(ls -l "$SUDOERS_DST")"
echo

# ── 4. Valida sintassi con visudo -c ─────────────────────────────────────
echo "==> [4/4] Validazione visudo..."
if ! sudo visudo -c -f "$SUDOERS_DST" >/dev/null; then
    echo "ERR: visudo segnala errori di sintassi in $SUDOERS_DST" >&2
    sudo rm -f "$SUDOERS_DST"
    echo "     File rimosso. Niente effetti residui sul sistema." >&2
    exit 2
fi
echo "    OK: sintassi sudoers valida."
echo

# ── Test smoke: il runner risponde con usage? ────────────────────────────
echo "==> Smoke test: sudo $BIN_DST (usage attesa)"
if sudo "$BIN_DST" 2>&1 | head -1 | grep -q 'usage:'; then
    echo "    OK: runner raggiungibile via sudo NOPASSWD."
else
    echo "    ATTENZIONE: il runner non risponde con usage. Verifica manualmente."
fi
echo

echo "==> Sudo Manager installato."
echo "    Prossimi step:"
echo "      - Apri Admin UI -> /admin/sudo-manager per vedere i purpose registrati."
echo "      - Da mcp-core: sudo_manager::execute(\"playwright-install-deps\").await"
echo "      - Smoke manuale: sudo $BIN_DST playwright-install-deps"
