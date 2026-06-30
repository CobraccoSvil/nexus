-- Row-Level Security per isolamento multi-tenant
-- Eseguire DOPO init-schemas.sql.
-- Il parametro app.current_tenant_id deve essere settato con
-- SET LOCAL prima di ogni query (vedi TenantContext.withTenant).

-- ── audit_llm_calls ──────────────────────────────────────────────
ALTER TABLE audit_llm_calls ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_llm_calls FORCE ROW LEVEL SECURITY;   -- protegge anche il table owner

DROP POLICY IF EXISTS tenant_isolation ON audit_llm_calls;
CREATE POLICY tenant_isolation ON audit_llm_calls
  USING (tenant_id = current_setting('app.current_tenant_id', true));

-- ── embeddings ───────────────────────────────────────────────────
ALTER TABLE embeddings ENABLE ROW LEVEL SECURITY;
ALTER TABLE embeddings FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON embeddings;
CREATE POLICY tenant_isolation ON embeddings
  USING (tenant_id = current_setting('app.current_tenant_id', true));

-- ── rate_limits ──────────────────────────────────────────────────
ALTER TABLE rate_limits ENABLE ROW LEVEL SECURITY;
ALTER TABLE rate_limits FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON rate_limits;
CREATE POLICY tenant_isolation ON rate_limits
  USING (tenant_id = current_setting('app.current_tenant_id', true));

-- ── tenants ──────────────────────────────────────────────────────
-- La tabella tenants è read-only per il ruolo applicazione;
-- solo i superadmin possono inserire/aggiornare tenant.
ALTER TABLE tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenants FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_self_read ON tenants;
CREATE POLICY tenant_self_read ON tenants
  FOR SELECT
  USING (tenant_id = current_setting('app.current_tenant_id', true));

-- ── Ruolo applicazione ───────────────────────────────────────────
-- Il ruolo nexus_app NON è superuser → RLS si applica sempre.
-- Sostituire 'nexus_app_password' prima del deploy.
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nexus_app') THEN
    CREATE ROLE nexus_app WITH LOGIN PASSWORD 'nexus_app_password';
  END IF;
END $$;

GRANT SELECT, INSERT, UPDATE, DELETE ON audit_llm_calls TO nexus_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON embeddings       TO nexus_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON rate_limits      TO nexus_app;
GRANT SELECT                          ON tenants          TO nexus_app;
