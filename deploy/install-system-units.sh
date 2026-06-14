#!/usr/bin/env bash
# ADR 0028 livello 3 — Sposta mcp-core, brain e web-ide da systemd --user a
# --SYSTEM (gestiti da PID 1, immuni dalla caduta del manager --user in WSL).
#
# Perche': in WSL il manager systemd --user (user@<UID>) raggiunge exit.target e
# si spegne quando la sessione logind si chiude (chiusura terminale / errore WSL
# Relay vsock), NONOSTANTE loginctl enable-linger -> porta giu' Nexus anche
# durante una chat. PID 1 e' indipendente da logind/sessioni/linger, come i
# container Docker che infatti non cadono mai. Idempotente.
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

# SELF-DETACH: disabilitare le unit --user (sotto) puo' far cadere il manager
# user@UID in WSL, che termina la user-slice INCLUSO questo script se gira nella
# sessione utente (osservato: lo script si auto-terminava con "Terminated"). Ci
# si ri-esegue UNA volta in uno scope --system adottato da PID 1, immune.
if [ -z "${NEXUS_INSTALL_DETACHED:-}" ]; then
  echo "==> Ri-eseguo in scope --system (immune alla caduta sessione WSL)..."
  exec systemd-run --scope --collect --quiet \
    --setenv=NEXUS_INSTALL_DETACHED=1 --setenv=SUDO_USER="$USER_NAME" \
    bash "$ROOT/deploy/install-system-units.sh"
fi

echo "==> Nexus -> systemd --system (utente=$USER_NAME uid=$USER_UID)"

# 1. Disabilita le unit --user omonime (mcp-core/brain: stesso nome, scope
#    diverso -> doppioni di porta) e ferma il web-ide legacy (nohup).
USER_WANTS="$HOME_DIR/.config/systemd/user/default.target.wants"
rm -f "$USER_WANTS/nexus-mcp-core.service" "$USER_WANTS/nexus-brain.service"
runuser -l "$USER_NAME" -c "XDG_RUNTIME_DIR=/run/user/$USER_UID systemctl --user stop nexus-mcp-core.service nexus-brain.service" 2>/dev/null || true
pkill -f "apps/web-ide/server.js" 2>/dev/null || true
# Ferma anche il gateway LLM avviato via nohup legacy (pre-systemd): libera la
# 4060 per la unit systemd. Il watchdog del binario nuovo non lo rilancia; se per
# race il primo start della unit trovasse la porta ancora occupata, Restart=always
# riprova e a quel punto la 4060 e' libera.
pkill -x nexus-gateway 2>/dev/null || true
echo "  unit --user disabilitate, web-ide + gateway nohup legacy fermati"

# 2. Genera e installa le unit --system dai template.
for svc in nexus-brain nexus-mcp-core nexus-gateway nexus-web-ide; do
  src="$SRC/${svc}-system.service"
  [ -f "$src" ] || { echo "ERRORE: template mancante $src" >&2; exit 1; }
  sed -e "s/__USER__/$USER_NAME/g" -e "s/__UID__/$USER_UID/g" "$src" \
      > "/etc/systemd/system/${svc}.service"
  echo "  installato /etc/systemd/system/${svc}.service"
done

# 3. Log su file storici /tmp/nexus-*.log di proprieta' ROOT: fs.protected_regular
#    (=2) impedisce a systemd (PID1=root) di aprire in append un file NON suo in
#    una dir sticky world-writable come /tmp (errore 209/STDOUT, crash-loop). Le
#    unit --system aprono StandardOutput come root -> i log devono essere di root
#    (apre come owner, passa il fd al processo User=). I tail restano ok (644).
touch /tmp/nexus-neural.log /tmp/nexus-mcp-core.log /tmp/nexus-gateway.log /tmp/nexus-webide.log
chown root:root /tmp/nexus-neural.log /tmp/nexus-mcp-core.log /tmp/nexus-gateway.log /tmp/nexus-webide.log
chmod 644 /tmp/nexus-neural.log /tmp/nexus-mcp-core.log /tmp/nexus-gateway.log /tmp/nexus-webide.log

# 3bis. Disabilita i meccanismi di auto-restart APPLICATIVI di mcp-core: con i
#   servizi a --system il restart e' gia' garantito da systemd (Restart=always),
#   quindi sono ridondanti E conflittuali. ensure_user_manager risuscitava il
#   manager --user instabile (-> deactivate del core a ~2min); services_watchdog
#   riavviava il web-ide via deploy-local.sh -> processo nohup in conflitto con la
#   unit systemd (-> porta occupata -> crash-loop). Best-effort (DB up).
docker exec -i ideai-postgres-nexus-1 psql -U nexus -d nexus -c \
  "UPDATE settings SET value='false' WHERE key IN ('agent.user_manager.autostart_enabled','agent.watchdog.enabled');" \
  >/dev/null 2>&1 && echo "  ensure_user_manager + services_watchdog disabilitati (ridondanti con --system)" || true

# 4. Avvia: brain (gRPC 50051) prima, poi mcp-core, gateway e web-ide.
#    Il gateway dipende da mcp-core (After=) -> parte nello stesso gruppo.
systemctl daemon-reload
systemctl reset-failed nexus-brain nexus-mcp-core nexus-gateway nexus-web-ide 2>/dev/null || true
systemctl enable nexus-brain nexus-mcp-core nexus-gateway nexus-web-ide >/dev/null 2>&1 || true
systemctl start nexus-brain.service
sleep 8
systemctl start nexus-mcp-core.service nexus-gateway.service nexus-web-ide.service
sleep 12

# 5. Verifica.
echo "==> Stato unit:"
systemctl is-active nexus-brain nexus-mcp-core nexus-gateway nexus-web-ide || true
echo "==> Health:"
curl -s -o /dev/null -w "  brain(8001)=%{http_code}\n" --max-time 6 http://127.0.0.1:8001/health || true
curl -s -o /dev/null -w "  mcp-core(4000)=%{http_code}\n" --max-time 6 http://127.0.0.1:4000/health || true
curl -s -o /dev/null -w "  gateway(4060)=%{http_code}\n" --max-time 6 http://127.0.0.1:4060/providers || true
curl -s -o /dev/null -w "  web-ide(3000)=%{http_code}\n" --max-time 6 http://127.0.0.1:3000/ || true

echo
echo "==> Fatto. Restart futuri:  sudo systemctl restart nexus-mcp-core nexus-brain nexus-gateway nexus-web-ide"
echo "==> Test di stabilita' definitivo: chiudi TUTTI i terminali WSL, attendi"
echo "    1-2 min, poi  curl http://127.0.0.1:4000/health  deve dare 200."
