-- Aggiunge metadati di classificazione alle configurazioni di run.
-- `role`          : categoria semantica (frontend, backend, service, test, tool)
-- `essential`     : true se la configurazione serve per avviare l'app in modo minimale
-- `group_label`   : etichetta di raggruppamento nel wizard (es. "apps/web-ide", "crates/mcp-core", "docker")
ALTER TABLE run_configurations
    ADD COLUMN IF NOT EXISTS role        TEXT,
    ADD COLUMN IF NOT EXISTS essential   BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS group_label TEXT;
