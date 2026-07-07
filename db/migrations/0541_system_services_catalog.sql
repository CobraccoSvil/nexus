-- 0541_system_services_catalog.sql
-- Catalogo UNICO dei microservizi infrastruttura Nexus (regola G + regola L).
--
-- Prima di questa migrazione la lista dei microservizi infra viveva in DUE posti:
--   1) hardcoded nel route Next.js apps/web-ide/app/api/system/services/route.ts
--      (nomi unit systemd + porte hardcoded + euristica port_alive);
--   2) settings.agent.watchdog.services (JSON name+port_setting_key) letto dal
--      services_watchdog Rust.
-- Due liste della stessa cosa = duplicazione (regola L). Questa migrazione le
-- consolida in un unico catalogo `system.services_catalog`, fonte di verita' per:
--   - l'endpoint mcp-core GET/POST /api/system/services (stato + controllo),
--     consumato dal pannello "Servizi Nexus" del web-ide via proxy;
--   - il services_watchdog (filtra le voci con watchdog_managed=true).
--
-- Schema di ogni voce (JSON):
--   name             nome canonico = --service di deploy-local.sh, usato come id URL
--   label            etichetta UI
--   port_setting_key chiave settings da cui risolvere la porta (regola G), OPPURE
--   port             porta letterale (solo infra dati: postgres/redis)
--   led              LED della statusbar alimentato dal servizio (opzionale)
--   description      descrizione UI
--   readonly         true = mostrato ma non controllabile (postgres/redis)
--   controllable     true = start/stop/restart ammessi (allowlist di controllo)
--   panel_shown      true = mostrato nel pannello "Servizi Nexus"
--   watchdog_managed true = auto-restart dal services_watchdog (mcp-core escluso:
--                    e' il processo che ospita il watchdog)
--   systemd_unit     target di controllo su Unix (systemctl <unit>)
--   winsw_id         target di controllo su Windows (servizio WinSW / manifest)
--   docker_container hint legacy di provenienza (non usato per lo stato: lo stato
--                    e' un TCP probe onesto e cross-platform, regola M)
--
-- Lo STATO non e' piu' dedotto da systemctl (assente su Windows) ne' mascherato
-- via euristica port_alive: e' il TCP probe della porta risolta (segnale
-- strutturato, regola M), identico su Windows e Unix.
-- Idempotente.

INSERT INTO settings (key, value, category, description) VALUES
  ('system.services_catalog',
   '[
     {"name":"mcp-core","label":"Core (mcp-core)","port_setting_key":"mcp_core_http_port","led":"Core","description":"Orchestratore + endpoint AI (/api/neural) + Tool Runner gRPC :50071","readonly":false,"controllable":true,"panel_shown":true,"watchdog_managed":false,"systemd_unit":"nexus-core-wsl","winsw_id":"nexus-mcp-core"},
     {"name":"nexus-gateway","label":"LLM Gateway","port_setting_key":"nexus_gateway_port","led":"Provider","description":"Router provider AI","readonly":false,"controllable":true,"panel_shown":true,"watchdog_managed":true,"systemd_unit":"nexus-gateway","winsw_id":"nexus-gateway"},
     {"name":"admin-service","label":"Admin Service","port_setting_key":"admin_service_port","description":"Backend pannello amministrazione","readonly":false,"controllable":true,"panel_shown":true,"watchdog_managed":true,"systemd_unit":"nexus-admin-wsl","winsw_id":"nexus-admin"},
     {"name":"doc-service","label":"Doc Service","port_setting_key":"doc_service_port","description":"Generatore documentazione","readonly":false,"controllable":true,"panel_shown":true,"watchdog_managed":true,"systemd_unit":"nexus-doc-wsl","winsw_id":"nexus-doc"},
     {"name":"billing-service","label":"Billing Service","port_setting_key":"billing_service_port","description":"Billing e preventivi","readonly":false,"controllable":true,"panel_shown":true,"watchdog_managed":true,"systemd_unit":"nexus-billing-wsl","winsw_id":"nexus-billing"},
     {"name":"plugin-service","label":"Plugin Service","port_setting_key":"plugin_service_port","description":"Connettori MCP","readonly":false,"controllable":true,"panel_shown":true,"watchdog_managed":true,"systemd_unit":"nexus-plugin-wsl","winsw_id":"nexus-plugin"},
     {"name":"browser-bridge-mcp","label":"Browser Bridge","port_setting_key":"browser_bridge_port","description":"MCP browser bridge","readonly":false,"controllable":true,"panel_shown":false,"watchdog_managed":true,"systemd_unit":"nexus-browser-bridge-wsl","winsw_id":"nexus-browser-bridge"},
     {"name":"web-ide","label":"Web IDE","port_setting_key":"web_ide_port","description":"Frontend Next.js","readonly":false,"controllable":false,"panel_shown":false,"watchdog_managed":true,"systemd_unit":"nexus-webide","winsw_id":"nexus-web-ide"},
     {"name":"redis","label":"Redis","port":6379,"led":"Redis","description":"Cache e broker messaggi","readonly":true,"controllable":false,"panel_shown":true,"watchdog_managed":false,"docker_container":"ideai-redis-1"},
     {"name":"postgres","label":"PostgreSQL","port":5433,"led":"DB","description":"Database relazionale principale","readonly":true,"controllable":false,"panel_shown":true,"watchdog_managed":false,"docker_container":"ideai-postgres-nexus-1"}
   ]',
   'system',
   'Catalogo unico dei microservizi infrastruttura Nexus. Fonte di verita per il pannello Servizi Nexus (endpoint mcp-core /api/system/services) e per il services_watchdog (voci watchdog_managed=true). Vedi migrazione 0541 per lo schema di ogni voce.')
ON CONFLICT (key) DO NOTHING;

-- Consolidamento (regola L): la vecchia lista del watchdog e' assorbita dal
-- catalogo (voci watchdog_managed=true). Il services_watchdog ora legge il
-- catalogo unico; questa chiave non ha piu' lettori.
DELETE FROM settings WHERE key = 'agent.watchdog.services';
