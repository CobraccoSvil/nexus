-- 0398_resource_violation_policy.sql
--
-- Governance risorse, lato REATTIVO: le violazioni diventano diagnosi visibili
-- (pannello Problemi) e, dove la correzione e' deterministica, innescano la
-- riparazione automatica via run agentico (pattern service_observer_remediation).
--
-- 1. service_diagnoses ospita le violazioni come signal_kind='policy_violation'
--    (scelta vs config_issues: runtime-driven, lifecycle open->diagnosing->
--    resolved gia' integrato nel pannello). Nuovo stato terminale
--    'failed_remediation' (riparazione fallita N volte: resta visibile con
--    richiesta di intervento manuale). Colonne nuove: file_path (sorgente
--    localizzato della violazione, cliccabile in UI) e remediation_attempts.
-- 2. nexus_resource_audit: outcome esteso con 'detected' (rilevazione senza
--    blocco, es. linter sorgenti) e 'failed' (riparazione fallita); resource_kind
--    esteso con 'network' (violazioni URL).
-- 3. Settings del ciclo di riparazione (regola G, ON CONFLICT DO NOTHING).
-- 4. projects.port_lint_enabled: opt-out per-progetto del linter sorgenti; il
--    meta-progetto Nexus e' escluso alla radice (contiene legittimamente
--    letterali di porta in test/migrazioni/doc).
--
-- Idempotente.

BEGIN;

ALTER TABLE service_diagnoses ADD COLUMN IF NOT EXISTS file_path TEXT;
ALTER TABLE service_diagnoses ADD COLUMN IF NOT EXISTS remediation_attempts INT NOT NULL DEFAULT 0;

COMMENT ON COLUMN service_diagnoses.file_path IS
'Path RELATIVO alla project_root del sorgente della violazione (signal_kind=policy_violation). NULL per violazioni runtime non localizzate. Mig 0398.';
COMMENT ON COLUMN service_diagnoses.remediation_attempts IS
'Tentativi di riparazione automatica per questa diagnosi (policy_violation). Mig 0398.';

CREATE INDEX IF NOT EXISTS idx_service_diagnoses_policy
    ON service_diagnoses (project_id, status)
 WHERE signal_kind = 'policy_violation';

-- Audit: nuovi outcome e resource_kind (CHECK ricreati in modo idempotente).
ALTER TABLE nexus_resource_audit DROP CONSTRAINT IF EXISTS nexus_resource_audit_outcome_check;
ALTER TABLE nexus_resource_audit ADD CONSTRAINT nexus_resource_audit_outcome_check
    CHECK (outcome IN ('allowed', 'blocked', 'killed', 'detected', 'failed'));
ALTER TABLE nexus_resource_audit DROP CONSTRAINT IF EXISTS nexus_resource_audit_resource_kind_check;
ALTER TABLE nexus_resource_audit ADD CONSTRAINT nexus_resource_audit_resource_kind_check
    CHECK (resource_kind IN ('port', 'db', 'container', 'file', 'env', 'command', 'service', 'network'));

-- Opt-out linter per-progetto + esclusione meta-progetto Nexus.
ALTER TABLE projects ADD COLUMN IF NOT EXISTS port_lint_enabled BOOLEAN NOT NULL DEFAULT TRUE;
UPDATE projects SET port_lint_enabled = FALSE
 WHERE repository_root_path = '/home/administrator/ideai'
   AND port_lint_enabled = TRUE;

-- Settings ciclo riparazione. auto_remediate=true di default (decisione utente:
-- "quando sicurezza avvisa una violazione deve far riparare a Nexus"); i freni
-- sono cooldown + cap orario + cap tentativi per firma.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.resource_violation.linter_enabled', 'true', 'agent',
   'Linter periodico dei sorgenti progetto per violazioni risorse (porte/URL hardcoded). Sola rilevazione: apre diagnosi policy_violation.'),
  ('agent.resource_violation.linter_interval_seconds', '300', 'agent',
   'Cadenza del linter sorgenti (secondi). I sorgenti cambiano lentamente; il caso urgente passa dalla catena sincrona del port_enforcer.'),
  ('agent.resource_violation.auto_remediate', 'true', 'agent',
   'Se true, le violazioni CORREGGIBILI (porte/URL hardcoded) innescano automaticamente un run di riparazione. Le classi a solo blocco (db/fs/container) non sono mai auto-riparate.'),
  ('agent.resource_violation.remediate_cooldown_seconds', '900', 'agent',
   'Cooldown per firma violazione prima di un nuovo tentativo di riparazione (un run dura minuti).'),
  ('agent.resource_violation.remediate_max_per_hour', '3', 'agent',
   'Cap orario di run di riparazione per progetto (anti-spirale).'),
  ('agent.resource_violation.max_attempts_per_signature', '2', 'agent',
   'Tentativi massimi di riparazione per firma (finestra 24h, cross-row). Oltre: failed_remediation + notifica per intervento manuale.')
ON CONFLICT (key) DO NOTHING;

COMMIT;
