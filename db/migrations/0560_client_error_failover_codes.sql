-- 0560_client_error_failover_codes.sql
-- BUG 2 del failover: vocabolario DB-driven (regola G) dei code di client_error
-- (4xx) PROVIDER-SPECIFICI recuperabili su un ALTRO provider.
--
-- Causa radice: l'executor (crates/nexus-agent-graph/src/nodes/executor.rs) esclude
-- TUTTI i ClientError dal failover cross-provider, motivato dall'incidente f0ad0337
-- (Mistral invalid_request_message_order = errore di history CONDIVISA che
-- fallirebbe su qualunque provider). Ma un 400 PROVIDER-SPECIFICO (es. Google
-- invalid_argument / thought_signature / schema) e' RECUPERABILE con un provider
-- diverso (deepseek): con chain pinnata singola il run moriva senza ripiego.
--
-- Fix: l'executor consulta questa whitelist (via ExecutorConfig.recoverable_client_
-- error_codes, caricata da setting_csv) e, per un ClientError il cui code STRUTTURATO
-- (regola M: status+code, mai la prosa) e' in whitelist, fa failover cross-provider;
-- ogni altro 4xx resta chiusura onesta. Punto unico della decisione:
-- ProviderUnavailableInfo::allows_cross_provider_failover (ports.rs).
--
-- CSV, match case-insensitive lato codice. Aggiungere qui un nuovo code
-- provider-specifico recuperabile NON richiede redeploy (regola G).

INSERT INTO settings (key, value, category, description) VALUES
  ('routing.client_error_failover_codes',
   'invalid_argument,thought_signature,failed_precondition',
   'routing',
   'Vocabolario dei code di client_error (4xx) PROVIDER-SPECIFICI recuperabili su un altro provider: un 400 con uno di questi code (es. Google invalid_argument/thought_signature) fa failover cross-provider invece di chiudere il run; ogni altro 4xx di formato/history condivisa (es. Mistral invalid_request_message_order) resta chiusura onesta. CSV. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
