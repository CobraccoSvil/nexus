-- 0637: ricostruisce gli oggetti che le migrazioni 0104 e 0105 dichiarano ma
-- non creano.
--
-- 0104_quality_scans_async.sql e 0105_quality_vector_enhancements.sql hanno per
-- corpo `SELECT 1;`: furono neutralizzate al bootstrap del monorepo perche' i
-- loro oggetti erano gia' presenti sul DB di sviluppo. Il contenuto originale
-- non e' recuperabile (git log --follow su quei file da' un solo commit, e li'
-- sono gia' stub). Conseguenza: un DB ricostruito da zero NON riceve quegli
-- oggetti, mentre il codice li usa.
--
-- Il DDL qui sotto NON e' dedotto dal codice: e' letto dal DB vivo del cluster
-- di sviluppo (information_schema.columns, pg_constraint, pg_indexes), che e'
-- l'unica fonte autoritativa rimasta. Su un DB gia' popolato la CREATE e le ADD
-- COLUMN sono no-op (IF NOT EXISTS); i COMMENT ON eseguono sempre e
-- sovrascrivono per idempotenza.
--
-- Le 0104/0105 NON si toccano: sono immutabili, riscriverle cambierebbe il
-- checksum sqlx sui DB gia' migrati.
--
-- Le altre due stub della stessa serie sono innocue e restano tali:
--   * 0106 modificava il template `agent.coder.base`, che 0437 riscrive per
--     intero (verificato: su DB ricostruito e su DB vivo il `content` di quella
--     chiave ha lo stesso md5);
--   * 0107 correggeva righe `nexus_routing_matrix` per intent `fix_semplice`,
--     che 0268 sovrascrive imponendo mistral-large-latest a priority 300 con
--     manual_override su ogni behavior_mode di quell'intent.

-- ── nexus_quality_scans ─────────────────────────────────────────────────────
-- Una riga per esecuzione della scansione qualita'. Scritta e letta da
-- crates/mcp-core/src/projects/quality.rs (`insert_running_scan`,
-- `finalize_scan_completed`, `finalize_scan_failed`, `get_quality_scan_status`)
-- e da crates/mcp-core/src/task_watchdog.rs (`terminate_stale_tasks`).
-- Vive nel meta-DB, non nel DB per-progetto.
CREATE TABLE IF NOT EXISTS nexus_quality_scans (
    id              BIGSERIAL PRIMARY KEY,
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    status          TEXT NOT NULL DEFAULT 'running',
    files_scanned   INTEGER,
    total_findings  INTEGER,
    by_severity     JSONB,
    by_category     JSONB,
    error_message   TEXT,
    duration_ms     INTEGER,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ
);

-- Stesso nome e stessa definizione dell'indice presente sul DB vivo: su un DB
-- gia' popolato IF NOT EXISTS lo salta invece di duplicarlo.
CREATE INDEX IF NOT EXISTS idx_quality_scans_project
    ON nexus_quality_scans(project_id, started_at DESC);

COMMENT ON TABLE nexus_quality_scans IS
'Una riga per esecuzione della scansione qualita'' di un progetto. La riga nasce in stato running e viene chiusa dal completamento o dal watchdog, che termina le scansioni rimaste appese.';

-- ── project_quality_findings: colonne della fase vettoriale ─────────────────
-- La scansione fa due passate: una regex, che riempie i campi base, e una
-- vettoriale, che usa l''embedder e il code index Qdrant. Queste quattro
-- colonne appartengono alla seconda, ma non sono opzionali per lo schema: tutte
-- le query che alimentano il pannello Ottimizzazione le elencano nel SELECT,
-- quindi su un DB privo di esse quel pannello non degrada, si rompe.
ALTER TABLE project_quality_findings
    ADD COLUMN IF NOT EXISTS confidence         TEXT,
    ADD COLUMN IF NOT EXISTS context_snippet    TEXT,
    ADD COLUMN IF NOT EXISTS related_files      TEXT[],
    ADD COLUMN IF NOT EXISTS is_auto_suppressed BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN project_quality_findings.confidence IS
'low, medium o high, assegnata dalla passata vettoriale in base allo snippet e al punteggio del miglior match nel code index. Resta NULL se Qdrant o l''embedder non rispondono: la passata vettoriale viene saltata, non simulata.';

COMMENT ON COLUMN project_quality_findings.context_snippet IS
'Righe di codice attorno al finding, cinque per lato. Riempita gia'' dalla passata regex quando il finding ha un numero di riga; e'' l''input su cui la passata vettoriale decide confidence e auto-soppressione.';

COMMENT ON COLUMN project_quality_findings.related_files IS
'File semanticamente simili al finding, trovati nel code index Qdrant durante la passata vettoriale.';

COMMENT ON COLUMN project_quality_findings.is_auto_suppressed IS
'true quando la passata vettoriale riconosce un falso positivo. La riga resta in tabella ma non entra nei conteggi aggregati della scansione.';
