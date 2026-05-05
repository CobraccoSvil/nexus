-- Estende project_database_config per supportare piu' connessioni DB
-- per singolo progetto (es. app con DB primario + DB analytics).
--
-- Ogni connessione ha un nome logico (case-insensitive unique per progetto)
-- e un flag is_primary: la connessione primaria e' quella usata di default
-- dagli endpoint migrazioni legacy che non specificano connection_id.

ALTER TABLE project_database_config
    ADD COLUMN IF NOT EXISTS name TEXT NOT NULL DEFAULT 'primary';

ALTER TABLE project_database_config
    ADD COLUMN IF NOT EXISTS is_primary BOOLEAN NOT NULL DEFAULT true;

-- Rimuove il vincolo di unicita' globale su project_id per permettere piu' DB.
ALTER TABLE project_database_config
    DROP CONSTRAINT IF EXISTS project_database_config_project_id_key;

-- Unicita' per (project_id, name) case-insensitive.
CREATE UNIQUE INDEX IF NOT EXISTS uq_project_database_config_project_name
    ON project_database_config(project_id, LOWER(name));

-- Solo una connessione primaria per progetto.
CREATE UNIQUE INDEX IF NOT EXISTS uq_project_database_config_project_primary
    ON project_database_config(project_id)
    WHERE is_primary = true;

COMMENT ON COLUMN project_database_config.name IS
    'Nome logico della connessione DB all''interno del progetto (es. primary, analytics).';
COMMENT ON COLUMN project_database_config.is_primary IS
    'Indica la connessione DB di default del progetto. Una sola riga primaria per progetto.';
