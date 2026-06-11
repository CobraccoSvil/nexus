-- 0397_resource_policies.sql
--
-- Governance unificata delle risorse di sistema (porte, filesystem, URL/rete,
-- database, container): catalogo policy DB-driven. UN solo posto (regola G/L)
-- per accendere/spegnere/configurare ogni guard-rail; il codice legge da qui
-- (cache 60s) e i flag legacy (es. agent.enforce_port_allocation) restano come
-- override retro-compatibili mappati dal modulo resource_governance.
--
-- Colonne:
--   resource_kind  classe risorsa ('port'|'file'|'network'|'db'|'container')
--   rule_key       regola dentro la classe (es. 'enforce_hardcode')
--   enabled        kill-switch per regola
--   severity       'error'|'warning' (mappata su pannello problemi/audit)
--   auto_remediate se true e la regola e' correggibile in modo deterministico,
--                  la violazione apre una diagnosi e innesca il run di
--                  riparazione; se false (o regola non correggibile) si ferma a
--                  blocco/rilevazione + audit + notifica
--   params         tuning specifico della regola (jsonb)
--
-- Idempotente.

BEGIN;

CREATE TABLE IF NOT EXISTS nexus_resource_policies (
    id            SERIAL PRIMARY KEY,
    resource_kind TEXT NOT NULL,
    rule_key      TEXT NOT NULL,
    enabled       BOOLEAN NOT NULL DEFAULT TRUE,
    severity      TEXT NOT NULL DEFAULT 'error'
        CHECK (severity IN ('error', 'warning')),
    auto_remediate BOOLEAN NOT NULL DEFAULT FALSE,
    params        JSONB NOT NULL DEFAULT '{}'::jsonb,
    description   TEXT NOT NULL DEFAULT '',
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (resource_kind, rule_key)
);

COMMENT ON TABLE nexus_resource_policies IS
'Catalogo unico delle policy di governance risorse (mig 0397). Letto da crates/mcp-core/src/security/resource_governance.rs con cache 60s. Regola G: niente flag hardcoded nel codice.';

-- Seed: regole iniziali. auto_remediate=true SOLO dove la correzione e'
-- deterministica (decisione utente 2026-06-11): porte e URL/path hardcoded.
INSERT INTO nexus_resource_policies (resource_kind, rule_key, enabled, severity, auto_remediate, params, description) VALUES
  ('port', 'enforce_hardcode', TRUE, 'error', TRUE, '{}'::jsonb,
   'Porte hardcoded fuori bucket (incl. fallback env tipo process.env.PORT || 5000) rifiutate in scrittura; linter sui sorgenti; riparazione automatica via request_port.'),
  ('port', 'require_allocation', TRUE, 'error', TRUE, '{}'::jsonb,
   'Porte nel bucket Nexus lecite solo se allocate in nexus_port_allocations; enforcement in scrittura + kill runtime (port_enforcer).'),
  ('file', 'jail_root', TRUE, 'error', FALSE, '{}'::jsonb,
   'Letture/scritture confinate alla project_root (incl. risoluzione symlink). Solo blocco, nessun auto-fix.'),
  ('file', 'protected_paths', TRUE, 'error', FALSE, '{}'::jsonb,
   'Path protetti non scrivibili dagli agenti (lista in settings agent.protected_paths).'),
  ('file', 'disk_quota', TRUE, 'error', FALSE, '{"cache_seconds": 300}'::jsonb,
   'Quota disco per progetto (nexus_resource_quotas.max_disk_mb) applicata alle scritture. Solo blocco.'),
  ('file', 'read_max_bytes', TRUE, 'warning', FALSE, '{"max_bytes": 2097152}'::jsonb,
   'Cap dimensione lettura singolo file via read_file (anti-saturazione contesto/memoria).'),
  ('network', 'no_hardcoded_internal', TRUE, 'error', TRUE, '{}'::jsonb,
   'URL hardcoded verso localhost/host interni nei sorgenti rifiutati in scrittura; riparazione con configurazione governata.'),
  ('db', 'sql_injection', TRUE, 'error', FALSE, '{}'::jsonb,
   'Detector SQL (ADR 0021) sulle query del progetto. Solo blocco + audit, nessun auto-fix (decisione utente).'),
  ('db', 'block_nexus', TRUE, 'error', FALSE, '{}'::jsonb,
   'Connessioni/query verso il DB infrastruttura Nexus vietate (resolve_project_conn).'),
  ('container', 'memory_quota', TRUE, 'error', FALSE, '{}'::jsonb,
   'Quota RAM per progetto (nexus_resource_quotas.max_memory_mb) applicata pre-avvio servizio/container. Solo blocco.')
ON CONFLICT (resource_kind, rule_key) DO NOTHING;

COMMIT;
