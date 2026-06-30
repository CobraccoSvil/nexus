-- 0496_nexus_data_routing.sql
-- Directory di routing (meta-DB, GLOBALE) per la separazione DB: mappa
-- entity (session_id / run_id) -> project_id. Serve agli handler che hanno solo
-- session_id o run_id (non project_id) per risolvere il pool del DB-progetto
-- una volta che i dati per-progetto sono migrati in <slug>_nexus. Resta nel
-- meta-DB (e' infrastruttura di routing, non dato di un singolo progetto).
CREATE TABLE IF NOT EXISTS nexus_data_routing (
    entity_kind text NOT NULL CHECK (entity_kind IN ('session', 'run')),
    entity_id   uuid NOT NULL,
    project_id  uuid NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (entity_kind, entity_id)
);

CREATE INDEX IF NOT EXISTS nexus_data_routing_project_idx
    ON nexus_data_routing (project_id);

-- Backfill dalle tabelle che hanno ancora project_id nel meta-DB (i dati sono
-- dual-present durante la transizione). Idempotente.
INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id)
SELECT 'session', id, project_id FROM chat_sessions WHERE project_id IS NOT NULL
ON CONFLICT DO NOTHING;

INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id)
SELECT 'run', id, project_id FROM agent_runs WHERE project_id IS NOT NULL
ON CONFLICT DO NOTHING;
