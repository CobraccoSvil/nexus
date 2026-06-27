#!/usr/bin/env bash
# ADR 0028 livello 3 — Sposta mcp-core e web-ide da systemd --user a
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

# 1. Disabilita la unit --user omonima (mcp-core: stesso nome, scope diverso ->
#    doppioni di porta) e ferma il web-ide legacy (nohup).
USER_WANTS="$HOME_DIR/.config/systemd/user/default.target.wants"
rm -f "$USER_WANTS/nexus-mcp-core.service"
runuser -l "$USER_NAME" -c "XDG_RUNTIME_DIR=/run/user/$USER_UID systemctl --user stop nexus-mcp-core.service" 2>/dev/null || true
pkill -f "apps/web-ide/server.js" 2>/dev/null || true
# Ferma anche il gateway LLM avviato via nohup legacy (pre-systemd): libera la
# 4060 per la unit systemd. Il watchdog del binario nuovo non lo rilancia; se per
# race il primo start della unit trovasse la porta ancora occupata, Restart=always
# riprova e a quel punto la 4060 e' libera.
pkill -x nexus-gateway 2>/dev/null || true
echo "  unit --user disabilitate, web-ide + gateway nohup legacy fermati"

# 2. Genera e installa le unit --system dei servizi CORE dai template.
for svc in nexus-mcp-core nexus-gateway nexus-web-ide; do
  src="$SRC/${svc}-system.service"
  [ -f "$src" ] || { echo "ERRORE: template mancante $src" >&2; exit 1; }
  sed -e "s/__USER__/$USER_NAME/g" -e "s/__UID__/$USER_UID/g" "$src" \
      > "/etc/systemd/system/${svc}.service"
  echo "  installato /etc/systemd/system/${svc}.service"
done

# 2bis. Microservizi Rust (chat/admin/plugin/doc/billing): stessa governance
#   systemd --system dei core (ADR 0028 L3, parita'). Prima giravano solo come
#   nohup di deploy-local.sh -> non ripartivano dopo freeze/sospensione WSL.
#
#   Mappa "template:binario:unit":
#     - template  = file in deploy/systemd/<template>-system.service
#     - binario   = nome del binario in target/debug/ (legge la porta dal DB:
#                   nexus_auth::resolve_port settings.<svc>_port, NON da env)
#     - unit      = nome installato in /etc/systemd/system/<unit>.service
#   Il nome unit e' nexus-<binario>.service cosi' deploy-local.sh start_service
#   lo riconosce (ramo systemd --system, riga ~259) e fa restart via systemd
#   invece di nohup: niente doppioni sulla stessa porta. Il pannello UI cerca
#   nexus-chat-wsl/... ma usa port_alive come fallback, quindi li vede comunque.
#
#   (a) Symlink stabile target/nexus-current/<binario> -> target/debug/<binario>
#       (stesso pattern dei core in deploy-local.sh start_service), usato come
#       ExecStart: la unit esegue sempre l'ultima build debug.
mkdir -p "$ROOT/target/nexus-current"
for entry in chat:chat-service admin:admin-service plugin:plugin-service doc:doc-service billing:billing-service; do
  tmpl="${entry%%:*}"
  bin="${entry#*:}"
  src="$SRC/nexus-${tmpl}-system.service"
  binpath="$ROOT/target/debug/$bin"
  [ -f "$src" ] || { echo "ERRORE: template mancante $src" >&2; exit 1; }
  if [ ! -f "$binpath" ]; then
    echo "  ATTENZIONE: binario mancante $binpath (compila con deploy-local.sh --rust); unit installata comunque, Restart=always la avviera' appena il binario esiste"
  fi
  ln -sfn "$binpath" "$ROOT/target/nexus-current/$bin"
  chown -h "$USER_NAME:$USER_NAME" "$ROOT/target/nexus-current/$bin" 2>/dev/null || true
  sed -e "s/__USER__/$USER_NAME/g" -e "s/__UID__/$USER_UID/g" "$src" \
      > "/etc/systemd/system/nexus-${bin}.service"
  echo "  installato /etc/systemd/system/nexus-${bin}.service (symlink -> $binpath)"
done

# 3. Log su file storici /tmp/nexus-*.log di proprieta' ROOT: fs.protected_regular
#    (=2) impedisce a systemd (PID1=root) di aprire in append un file NON suo in
#    una dir sticky world-writable come /tmp (errore 209/STDOUT, crash-loop). Le
#    unit --system aprono StandardOutput come root -> i log devono essere di root
#    (apre come owner, passa il fd al processo User=). I tail restano ok (644).
# NB: i microservizi loggano su /tmp/nexus-<binario>.log (StandardOutput nelle
#   loro unit), stessa logica root-owned 644.
MICRO_LOGS="/tmp/nexus-chat-service.log /tmp/nexus-admin-service.log /tmp/nexus-plugin-service.log /tmp/nexus-doc-service.log /tmp/nexus-billing-service.log"
touch /tmp/nexus-mcp-core.log /tmp/nexus-gateway.log /tmp/nexus-webide.log $MICRO_LOGS
chown root:root /tmp/nexus-mcp-core.log /tmp/nexus-gateway.log /tmp/nexus-webide.log $MICRO_LOGS
chmod 644 /tmp/nexus-mcp-core.log /tmp/nexus-gateway.log /tmp/nexus-webide.log $MICRO_LOGS

# 3bis. Disabilita i meccanismi di auto-restart APPLICATIVI di mcp-core: con i
#   servizi a --system il restart e' gia' garantito da systemd (Restart=always),
#   quindi sono ridondanti E conflittuali. ensure_user_manager risuscitava il
#   manager --user instabile (-> deactivate del core a ~2min); services_watchdog
#   riavviava il web-ide via deploy-local.sh -> processo nohup in conflitto con la
#   unit systemd (-> porta occupata -> crash-loop). Best-effort (DB up).
docker exec -i ideai-postgres-nexus-1 psql -U nexus -d nexus -c \
  "UPDATE settings SET value='false' WHERE key IN ('agent.user_manager.autostart_enabled','agent.watchdog.enabled');" \
  >/dev/null 2>&1 && echo "  ensure_user_manager + services_watchdog disabilitati (ridondanti con --system)" || true

# 4. Avvia: mcp-core, gateway e web-ide.
#    Il gateway dipende da mcp-core (After=) -> parte nello stesso gruppo.
#    I microservizi dipendono da mcp-core (After=) -> partono dopo.
MICRO_UNITS="nexus-chat-service nexus-admin-service nexus-plugin-service nexus-doc-service nexus-billing-service"
systemctl daemon-reload
systemctl reset-failed nexus-mcp-core nexus-gateway nexus-web-ide $MICRO_UNITS 2>/dev/null || true
systemctl enable nexus-mcp-core nexus-gateway nexus-web-ide $MICRO_UNITS >/dev/null 2>&1 || true
systemctl start nexus-mcp-core.service nexus-gateway.service nexus-web-ide.service
sleep 5
# Microservizi dopo mcp-core (leggono la porta dal DB, gia' pronto). Idempotente:
# uno start ripetuto su unit gia' attive e' un no-op.
# shellcheck disable=SC2086
systemctl start $MICRO_UNITS 2>/dev/null || true
sleep 7

# 5. Verifica.
echo "==> Stato unit:"
# shellcheck disable=SC2086
systemctl is-active nexus-mcp-core nexus-gateway nexus-web-ide $MICRO_UNITS || true
echo "==> Health:"
curl -s -o /dev/null -w "  mcp-core(4000)=%{http_code}\n" --max-time 6 http://127.0.0.1:4000/health || true
curl -s -o /dev/null -w "  gateway(4060)=%{http_code}\n" --max-time 6 http://127.0.0.1:4060/providers || true
curl -s -o /dev/null -w "  web-ide(3000)=%{http_code}\n" --max-time 6 http://127.0.0.1:3000/ || true
# Microservizi: porte canoniche da mig 0239 (i servizi le leggono dal DB).
curl -s -o /dev/null -w "  chat-service(4020)=%{http_code}\n"    --max-time 6 http://127.0.0.1:4020/health || true
curl -s -o /dev/null -w "  admin-service(4010)=%{http_code}\n"   --max-time 6 http://127.0.0.1:4010/health || true
curl -s -o /dev/null -w "  plugin-service(4050)=%{http_code}\n"  --max-time 6 http://127.0.0.1:4050/health || true
curl -s -o /dev/null -w "  doc-service(4030)=%{http_code}\n"     --max-time 6 http://127.0.0.1:4030/health || true
curl -s -o /dev/null -w "  billing-service(4040)=%{http_code}\n" --max-time 6 http://127.0.0.1:4040/health || true

echo
echo "==> Fatto. Restart futuri:  sudo systemctl restart nexus-mcp-core nexus-gateway nexus-web-ide $MICRO_UNITS"
echo "==> Test di stabilita' definitivo: chiudi TUTTI i terminali WSL, attendi"
echo "    1-2 min, poi  curl http://127.0.0.1:4000/health  deve dare 200."
