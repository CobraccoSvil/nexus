-- 0272_services_watchdog.sql
-- Watchdog generale dei microservizi Nexus (worker periodico in mcp-core,
-- task_watchdog::spawn_services_watchdog). In dev/WSL non c'e' systemd con
-- Restart=on-failure (a differenza della produzione): se un microservizio cade
-- resta giu'. Questo watchdog fa TCP probe periodico di ogni servizio e, se
-- down per N cicli consecutivi, lo riavvia invocando deploy-local.sh --service
-- <name> --debug in modo detached (riusa l'env corretto, niente duplicazione
-- della logica di avvio per kind di servizio).
--
-- Regola G: la lista servizi NON hardcoda le porte. Ogni voce ha
-- "port_setting_key" = chiave di `settings` da cui risolvere la porta a runtime
-- (stessa fonte di verita' dei servizi stessi). Il campo "name" e' il nome del
-- servizio per deploy-local.sh (--service <name>), allineato a SERVICES_CATALOG.
-- mcp-core e' ESCLUSO di proposito: e' il processo che ospita il watchdog.
--
-- Anti-restart-loop: cooldown tra tentativi + cap di riavvii consecutivi falliti
-- oltre il quale il servizio viene marcato irrecuperabile (log ERROR, niente
-- ulteriori tentativi finche' non torna up da solo).
-- Idempotente.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.watchdog.enabled', 'true', 'agent',
   'Abilita il watchdog generale dei microservizi (TCP probe + auto-restart in dev).'),
  ('agent.watchdog.interval_seconds', '30', 'agent',
   'Intervallo (secondi) tra i cicli di probe del watchdog servizi.'),
  ('agent.watchdog.fail_threshold', '2', 'agent',
   'Numero di cicli down CONSECUTIVI prima di tentare il riavvio di un servizio.'),
  ('agent.watchdog.restart_cooldown_seconds', '120', 'agent',
   'Cooldown (secondi) dopo un riavvio prima di poter ritentare lo stesso servizio.'),
  ('agent.watchdog.max_consecutive_restarts', '5', 'agent',
   'Riavvii consecutivi falliti oltre i quali il servizio e'' considerato irrecuperabile (stop tentativi, log ERROR).'),
  ('agent.watchdog.services',
   '[{"name":"brain","port_setting_key":"brain_rest_port"},{"name":"nexus-gateway","port_setting_key":"nexus_gateway_port"},{"name":"admin-service","port_setting_key":"admin_service_port"},{"name":"chat-service","port_setting_key":"chat_service_port"},{"name":"doc-service","port_setting_key":"doc_service_port"},{"name":"billing-service","port_setting_key":"billing_service_port"},{"name":"plugin-service","port_setting_key":"plugin_service_port"},{"name":"browser-bridge-mcp","port_setting_key":"browser_bridge_port"},{"name":"web-ide","port_setting_key":"web_ide_port"}]',
   'agent',
   'Lista JSON dei microservizi monitorati dal watchdog. name = nome per deploy-local.sh --service; port_setting_key = chiave settings da cui risolvere la porta (regola G). mcp-core escluso (ospita il watchdog).')
ON CONFLICT (key) DO NOTHING;
