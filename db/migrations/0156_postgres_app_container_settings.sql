-- M74 — Settings per il container Postgres separato dedicato ai DB applicativi
-- dei progetti gestiti dall'agente. Sono DB-driven (cache TTL 60s in mcp-core)
-- così operations/admin possono ruotare la password senza redeploy.
--
-- Default puntano al container `postgres-app` definito in docker-compose.local.yml.
INSERT INTO settings (key, value, updated_at) VALUES
    ('nexus_app_db_host',     'localhost', NOW()),
    ('nexus_app_db_port',     '5434',      NOW()),
    ('nexus_app_db_user',     'nexus_app', NOW()),
    ('nexus_app_db_password', 'nexus_app_dev_secret', NOW()),
    -- Connessione admin allo STESSO container (per CREATE DATABASE idempotente).
    -- Usa il superuser locale del container postgres-app — NON quello di nexus.
    ('nexus_app_admin_user',     'nexus_admin',         NOW()),
    ('nexus_app_admin_password', 'nexus_admin_secret',  NOW())
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value,
    updated_at = NOW()
WHERE settings.value IS DISTINCT FROM EXCLUDED.value;

-- Documentazione audit-friendly delle chiavi.
COMMENT ON TABLE settings IS
  'Settings runtime di Nexus, cache TTL 60s. Chiavi nexus_app_db_* configurano '
  'il container Postgres separato per i DB applicativi (M74) — mai usare le '
  'credenziali nexus/nexus del container infrastruttura per i progetti utente.';
