#!/usr/bin/env bash
# ADR 0028 livello 3 — Sposta mcp-core e brain da systemd --user a --SYSTEM.
#
# Perche': in WSL il manager systemd --user (user@<UID>) raggiunge exit.target e
# si spegne quando la sessione logind si chiude (chiusura terminale / errore WSL
# Relay vsock), NONOSTANTE loginctl enable-linger -> porta giu' mcp-core e brain
# anche durante una chat. PID 1 (--system) e' indipendente da logind/sessioni/
# linger, come i container Docker che infatti non cadono mai. Idempotente.
#
# USO:  sudo bash deploy/install-system-units.sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
  echo "ERRORE: esegui con sudo -> sudo bash deploy/install-system-units.sh" >&2
  exit 1
fi

USER_NAME="${SUDO_USER:-administrator}"
USER_UID="$(id -u "$USER_NAME")"
HOME_DIR="$(getent passwd "$USER_NAME" | cut -d: -f6)"
ROOT="$HOME_DIR/ideai"
SRC="$ROOT/deploy/systemd"

echo "==> Nexus core -> systemd --system (utente=$USER_NAME uid=$USER_UID)"

# 1. Disabilita le unit --user omonime (evita doppioni: stesso nome, scope
#    diverso -> due processi sulla stessa porta). Rimuove i symlink di enable e
#    ferma i servizi/manager --user se attivi (best-effort).
USER_WANTS="$HOME_DIR/.config/systemd/user/default.target.wants"
for svc in nexus-mcp-core nexus-brain; do
  rm -f "$USER_WANTS/${svc}.service"
done
runuser -l "$USER_NAME" -c "XDG_RUNTIME_DIR=/run/user/$USER_UID systemctl --user stop nexus-mcp-core.service nexus-brain.service" 2>/dev/null || true
echo "  unit --user disabilitate"

# 2. Genera e installa le unit --system dal template (sostituendo USER/UID).
for svc in nexus-brain nexus-mcp-core; do
  src="$SRC/${svc}-system.service"
  [ -f "$src" ] || { echo "ERRORE: template mancante $src" >&2; exit 1; }
  sed -e "s/__USER__/$USER_NAME/g" -e "s/__UID__/$USER_UID/g" "$src" \
      > "/etc/systemd/system/${svc}.service"
  echo "  installato /etc/systemd/system/${svc}.service"
done

# Log su file storici /tmp/nexus-*.log. ATTENZIONE: fs.protected_regular=2
# (hardening kernel) impedisce a systemd (PID1=root) di aprire in append un
# file NON di sua proprieta' in una dir sticky world-writable come /tmp ->
# errore "Failed to set up standard output: Permission denied" (status
# 209/STDOUT) e crash-loop. Le unit --system aprono StandardOutput come root,
# quindi i log devono essere di proprieta' di ROOT: root li apre come owner
# (protected_regular soddisfatto) e passa il fd gia' aperto al processo
# User=administrator, che ci scrive. I tool di lettura (tail) restano ok (644).
touch /tmp/nexus-neural.log /tmp/nexus-mcp-core.log
chown root:root /tmp/nexus-neural.log /tmp/nexus-mcp-core.log
chmod 644 /tmp/nexus-neural.log /tmp/nexus-mcp-core.log

# 3. Avvia: brain prima (mcp-core attende il gRPC 50051), poi mcp-core.
systemctl daemon-reload
# Azzera eventuali crash-loop/start-limit da tentativi precedenti.
systemctl reset-failed nexus-brain.service nexus-mcp-core.service 2>/dev/null || true
systemctl enable nexus-brain.service nexus-mcp-core.service >/dev/null 2>&1 || true
systemctl restart nexus-brain.service
sleep 8
systemctl restart nexus-mcp-core.service
sleep 14

# 4. Verifica.
echo "==> Stato unit:"
systemctl is-active nexus-brain.service nexus-mcp-core.service || true
echo "==> Health:"
curl -s -o /dev/null -w "  brain(8001)=%{http_code}\n" --max-time 6 http://127.0.0.1:8001/health || true
curl -s -o /dev/null -w "  mcp-core(4000)=%{http_code}\n" --max-time 6 http://127.0.0.1:4000/health || true

echo
echo "==> Fatto. Restart futuri:  sudo systemctl restart nexus-mcp-core nexus-brain"
echo "==> Test di stabilita' definitivo: chiudi TUTTI i terminali WSL, attendi"
echo "    1-2 min, poi  curl http://127.0.0.1:4000/health  deve dare 200."
