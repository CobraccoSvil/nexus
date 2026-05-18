-- 0166_nexus_resource_audit.sql
-- Audit trail centralizzato di TUTTE le allocazioni e blocchi di risorse di sistema
-- richieste dai progetti tramite tool agente o middleware Nexus.
--
-- Scritta in batch async (security/audit.rs) per non rallentare il path critico:
-- accumulo in canale mpsc, flush ogni 100 eventi o 5 secondi.
--
-- Domande tipiche risposte da questa tabella:
--   - Quante porte ha allocato il progetto X nelle ultime 24h?
--   - Quali tentativi di violazione policy sono stati bloccati?
--   - Chi ha lanciato il container Docker che e' stato killato dal port_enforcer?

CREATE TABLE IF NOT EXISTS nexus_resource_audit (
    id BIGSERIAL PRIMARY KEY,
    ts TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,

    actor TEXT NOT NULL
        CHECK (actor IN ('agent', 'user', 'system')),
    actor_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    actor_session_id UUID,                     -- run_id agente se actor='agent'

    -- Azione: verb_resource. Lista non esaustiva, estendibile senza migrazione.
    --   port_allocate, port_release, port_violation_kill
    --   service_start, service_stop, service_killed
    --   db_query, db_query_blocked
    --   container_create, container_blocked
    --   file_write, file_blocked
    --   env_rejected
    --   command_blocked (da safety.rs::check_command)
    action TEXT NOT NULL,

    -- Categoria della risorsa toccata.
    resource_kind TEXT NOT NULL
        CHECK (resource_kind IN ('port', 'db', 'container', 'file', 'env', 'command', 'service')),

    -- Identificativo human-readable della risorsa. Es: '30050', 'rental_db', 'nx-sb-abc123', 'src/app.ts'.
    resource_id TEXT,

    -- Esito dell'azione: allowed (autorizzata), blocked (rifiutata pre-exec), killed (terminata post-exec).
    outcome TEXT NOT NULL
        CHECK (outcome IN ('allowed', 'blocked', 'killed')),

    -- Dettagli strutturati: payload originale, reason del blocco, quota corrente, ecc.
    details JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS nexus_resource_audit_project_ts
    ON nexus_resource_audit(project_id, ts DESC);
CREATE INDEX IF NOT EXISTS nexus_resource_audit_action
    ON nexus_resource_audit(action);
CREATE INDEX IF NOT EXISTS nexus_resource_audit_outcome
    ON nexus_resource_audit(outcome)
    WHERE outcome IN ('blocked', 'killed');   -- index partial: usato per dashboard violazioni

COMMENT ON TABLE nexus_resource_audit IS
    'Audit trail allocazioni risorse di sistema per progetto. Scritto in batch da security/audit.rs.';
COMMENT ON COLUMN nexus_resource_audit.action IS
    'Verb_resource: port_allocate, port_violation_kill, db_query_blocked, container_create, env_rejected, command_blocked, ecc.';
COMMENT ON COLUMN nexus_resource_audit.outcome IS
    'allowed=eseguito, blocked=rifiutato pre-spawn, killed=terminato post-spawn da watcher.';
