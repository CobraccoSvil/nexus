-- 0464_project_cascade_orphan_tables.sql
-- Cancellazione progetto incompleta (FIX a): chiusura di un buco di residui orfani.
--
-- Contesto: DELETE /api/projects/:id (crates/mcp-core/src/projects/crud.rs
-- delete_project) esegue `DELETE FROM projects WHERE id = $1` confidando nei
-- CASCADE delle FK figlie. Tre tabelle Nexus hanno la colonna `project_id` (uuid)
-- ma NON hanno una FK verso projects(id), quindi i loro record sopravvivono alla
-- cancellazione del progetto e si accumulano come orfani:
--   * service_diagnoses
--   * nexus_provider_empty_responses
--   * nexus_security_audit
-- Verificato cancellando il progetto Beauty-Book: le righe restano con project_id
-- che non corrisponde piu' ad alcun progetto.
--
-- Le tre colonne sono di tipo uuid e projects.id e' uuid (verificato via
-- information_schema), quindi la FK ON DELETE CASCADE e' applicabile: e' il punto
-- unico corretto (il DB garantisce la pulizia, non un DELETE manuale sparso nel
-- codice -- regola L/H). Nessuna FK preesistente su queste colonne (verificato),
-- quindi basta DELETE orfani + ADD CONSTRAINT.
--
-- Ordine per CIASCUNA tabella:
--   1. DELETE degli orfani gia' presenti: aggiungere una FK su dati orfani
--      fallirebbe (violazione del vincolo sui record esistenti).
--   2. ADD CONSTRAINT FOREIGN KEY ... ON DELETE CASCADE: d'ora in poi il
--      `DELETE FROM projects` ripulisce automaticamente queste tabelle.
--
-- Idempotente: i DELETE su righe gia' assenti sono no-op; gli ADD CONSTRAINT sono
-- protetti da un guard che salta se il vincolo esiste gia'.

-- ── service_diagnoses ────────────────────────────────────────────────────────
DELETE FROM service_diagnoses
WHERE project_id IS NOT NULL
  AND project_id::text NOT IN (SELECT id::text FROM projects);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'service_diagnoses_project_id_fkey'
    ) THEN
        ALTER TABLE service_diagnoses
            ADD CONSTRAINT service_diagnoses_project_id_fkey
            FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE;
    END IF;
END $$;

-- ── nexus_provider_empty_responses ───────────────────────────────────────────
DELETE FROM nexus_provider_empty_responses
WHERE project_id IS NOT NULL
  AND project_id::text NOT IN (SELECT id::text FROM projects);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'nexus_provider_empty_responses_project_id_fkey'
    ) THEN
        ALTER TABLE nexus_provider_empty_responses
            ADD CONSTRAINT nexus_provider_empty_responses_project_id_fkey
            FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE;
    END IF;
END $$;

-- ── nexus_security_audit ─────────────────────────────────────────────────────
DELETE FROM nexus_security_audit
WHERE project_id IS NOT NULL
  AND project_id::text NOT IN (SELECT id::text FROM projects);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'nexus_security_audit_project_id_fkey'
    ) THEN
        ALTER TABLE nexus_security_audit
            ADD CONSTRAINT nexus_security_audit_project_id_fkey
            FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE;
    END IF;
END $$;
