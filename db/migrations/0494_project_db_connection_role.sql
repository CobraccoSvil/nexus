-- 0494_project_db_connection_role.sql
-- Fase 0 separazione DB per-progetto: distingue il DB applicativo dell'utente
-- (<slug>_app, visibile nel pannello SQL) dal DB metadati Nexus per-progetto
-- (<slug>_nexus, interno: chat/run/costi, mai esposto all'utente).
--
-- Le righe esistenti sono tutte DB applicativi -> default 'app'.

ALTER TABLE project_database_config
    ADD COLUMN IF NOT EXISTS connection_role text NOT NULL DEFAULT 'app';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'project_database_config_connection_role_chk'
    ) THEN
        ALTER TABLE project_database_config
            ADD CONSTRAINT project_database_config_connection_role_chk
            CHECK (connection_role IN ('app', 'nexus_metadata'));
    END IF;
END
$$;

COMMENT ON COLUMN project_database_config.connection_role IS
    'app = DB applicativo dell''utente (visibile nel pannello); nexus_metadata = DB interno Nexus per-progetto (mai esposto). Fase 0 separazione DB.';
