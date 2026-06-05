-- 0325_seed_default_settings.sql
--
-- Registry unico dei default di `settings` (regola G: il DB e' l'unica fonte di
-- configurazione; regola H: i dati nuovi arrivano da una migrazione versionata,
-- non da INSERT ad-hoc all'avvio del servizio).
--
-- Sostituisce gli INSERT duplicati che vivevano in
--   crates/mcp-core/src/settings.rs::ensure_required_settings
--   crates/admin-service/src/settings.rs::ensure_required_settings
-- ed elimina i default presi da env var (NEXUS_DLP_ENABLED,
-- NEXUS_ALLOW_CLOUD_TIER2/3), che violavano la regola G.
--
-- Idempotente: ON CONFLICT DO NOTHING non sovrascrive valori gia' impostati
-- dall'admin. La parte dinamica (projects_base_root derivato dalla working dir)
-- resta nel codice come punto unico in nexus_types::ensure_projects_base_root.

INSERT INTO settings (key, value, category, description, is_secret, updated_at) VALUES
  ('dlp_enabled', 'true', 'security',
   'Abilita/disabilita il Data Loss Prevention (classificazione sensibilita'' Tier).', FALSE, NOW()),
  ('dlp_allow_cloud_tier2', 'true', 'security',
   'Se true, consente di inviare Tier 2 (sensibili) verso provider cloud.', FALSE, NOW()),
  ('dlp_allow_cloud_tier3', 'false', 'security',
   'Se true, consente di inviare Tier 3 (critici) verso provider cloud (sconsigliato).', FALSE, NOW()),
  ('projects_base_root', '', 'infrastructure',
   'Root assoluta sotto cui e'' consentita la registrazione/navigazione dei progetti', FALSE, NOW()),
  ('agent_parallel_enabled', 'false', 'agent',
   'Abilita l''esecuzione parallela di piu'' agenti contemporaneamente per accelerare task complessi', FALSE, NOW()),
  ('agent_parallel_max', '3', 'agent',
   'Numero massimo di agenti paralleli per sessione (1-5)', FALSE, NOW()),
  ('network_dns_servers', '', 'infrastructure',
   'Server DNS personalizzati separati da virgola (es. 8.8.8.8,1.1.1.1). Usato dal Neural Core per risolvere i nomi host verso API AI esterne.', FALSE, NOW()),
  ('nexus_external_proxy', '', 'infrastructure',
   'Proxy HTTP/HTTPS per le chiamate verso API esterne (es. http://localhost:8002). Usato da tutti i backend Nexus tramite NEXUS_PROXY. Lascia vuoto per connessione diretta.', FALSE, NOW())
ON CONFLICT (key) DO NOTHING;
