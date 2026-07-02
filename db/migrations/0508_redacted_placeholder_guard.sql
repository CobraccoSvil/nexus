-- 0508_redacted_placeholder_guard.sql
--
-- Incidente Beaty-Book 2026-07-02 (run 4360e0ee): il placeholder FISSO
-- [REDACTED:db_connection_string] prodotto dal secret scanner del gateway
-- (non registrato nella RedactionMap, quindi irreversibile; la reidratazione
-- post-flight tocca comunque solo response.content, mai gli argomenti delle
-- tool call) e' stato copiato dal modello come valore letterale di
-- DATABASE_URL in run_service e backend/.env: pg-connection-string lo ha
-- interpretato come URL relativa -> host 'base' -> getaddrinfo ENOTFOUND.
-- Stessa dinamica con [REDACTED:email_pii] persistito nei sorgenti e nel DB
-- applicativo.
--
-- Fix alla causa radice (regola H), lato mcp-core:
--   1. Iniezione server-side di DATABASE_URL/NEXUS_PROJECT_DB_URL del DB
--      applicativo del progetto (punto unico ensure_project_db_url): attiva in
--      run_command e, per i processi long-running, dentro spawn_agent_process
--      (fix parallelo): il modello non deve mai comporre la URL.
--   2. Punto unico security::redaction_guard: rifiuta run_command/run_service/
--      write_file/edit_file i cui input contengono un placeholder di redazione
--      ([REDACTED:<tipo>] o __NEXUS_<KIND>_<N>__) con tool_result esplicativo
--      che indirizza al meccanismo corretto.
-- La reidratazione dei segreti nei tool_input e' stata scartata: il segreto
-- tornerebbe in chiaro negli step persistiti (agent_steps) e nei log.
--
-- Questa migrazione registra la policy nel catalogo governance (mig 0402):
-- kill-switch DB-driven (regola G). Il codice ha default fail-safe
-- enabled=true se la riga manca.
--
-- Idempotente.

BEGIN;

INSERT INTO nexus_resource_policies (resource_kind, rule_key, enabled, severity, auto_remediate, params, description) VALUES
  ('secret', 'no_redacted_placeholder', TRUE, 'error', FALSE, '{}'::jsonb,
   'Placeholder di redazione ([REDACTED:<tipo>], __NEXUS_<KIND>_<N>__) copiati come valori negli input di run_command/run_service/write_file/edit_file: input rifiutato con guida al meccanismo corretto (DATABASE_URL iniettata server-side, request_port). Solo blocco + audit, nessun auto-fix.')
ON CONFLICT (resource_kind, rule_key) DO NOTHING;

COMMIT;
