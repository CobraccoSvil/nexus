-- M63 guardrail: audit log dei comandi shell bloccati dal sanitizer.
--
-- Background: il 16/05/2026 un agent run ha droppato tabelle critiche del DB
-- Nexus eseguendo via run_command un psql/prisma con DB target sbagliato.
-- Da oggi tutti i `run_command` passano per il safety guardrail
-- (crates/mcp-core/src/agent_tools/safety.rs). Quando un comando matcha
-- la blacklist, viene rifiutato e l'evento e' registrato qui.
--
-- L'admin puo' interrogare questa tabella per:
-- - capire quali agenti hanno tentato comandi distruttivi
-- - validare l'efficacia delle regole (false positive vs true positive)
-- - bannare progetti / utenti compromessi

CREATE TABLE IF NOT EXISTS nexus_security_audit (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL,
    user_id         UUID,
    session_id      UUID,
    tool_name       TEXT NOT NULL,
    command_excerpt TEXT NOT NULL,
    category        TEXT NOT NULL,
    message         TEXT NOT NULL,
    blocked         BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_security_audit_project_created
    ON nexus_security_audit(project_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_security_audit_category
    ON nexus_security_audit(category, created_at DESC);
