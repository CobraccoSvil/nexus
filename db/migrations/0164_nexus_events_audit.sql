-- Tabella di audit persistente per eventi dispatcher
CREATE TABLE IF NOT EXISTS nexus_events_audit (
    event_id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    seq BIGINT NOT NULL,
    ts TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    topic TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    enrichment JSONB,
    actor_user_id UUID REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS nexus_events_audit_project_ts ON nexus_events_audit(project_id, ts DESC);
CREATE INDEX IF NOT EXISTS nexus_events_audit_kind ON nexus_events_audit(kind);
