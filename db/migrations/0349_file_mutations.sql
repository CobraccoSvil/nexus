-- 0349_file_mutations.sql
--
-- Tracking strutturato delle modifiche file fatte dai tool dell'agente
-- (write_file, edit_file). Abilita il ripristino punto-a-punto di una modifica
-- senza dover dipendere da git ne' da backup esterni.
--
-- Decisione di storage: contenuto salvato come TEXT in colonna dedicata. La
-- maggior parte dei file editati dall'agente sono sotto i 200 KB; per la coda
-- (file molto grandi) registriamo solo i metadati e saltiamo il tracking
-- (vedi cap in file_mutations.rs). Lo schema lascia spazio per migrare a un
-- blob-store filesystem in futuro senza cambiare API.
--
-- Idempotente: CREATE TABLE IF NOT EXISTS + DO $$ ... $$ per i vincoli.

CREATE TABLE IF NOT EXISTS file_mutations (
    id BIGSERIAL PRIMARY KEY,
    project_id UUID NOT NULL,
    -- session_id della chat che ha originato la modifica; nullable per
    -- tool invocati fuori da una sessione (es. worker schedulati).
    session_id UUID,
    -- user_id che ha autorizzato il run (lo stesso AgentToolContext.user_id).
    user_id UUID,
    -- Path relativo alla project root, identico al formato usato dai tool
    -- (es. "src/index.html"). NON usiamo path assoluti per evitare drift
    -- (lezione mig 0348 sul drift relativo/assoluto in project_documents).
    file_path TEXT NOT NULL,
    -- Tool che ha originato la mutazione: 'write_file' | 'edit_file' |
    -- 'delete_file' | 'rename_file' | 'revert' (per mutazioni create dal
    -- ripristino stesso, cosi' anche un revert e' annullabile).
    tool_name TEXT NOT NULL,
    -- Operazione semantica: 'created' (file non esisteva), 'modified'
    -- (esisteva e ha contenuto before), 'deleted' (cancellato), 'reverted'.
    op TEXT NOT NULL CHECK (op IN ('created', 'modified', 'deleted', 'reverted')),
    -- Contenuto PRE-modifica (NULL se op='created'). E' la chiave per il
    -- ripristino: revert sovrascrive il file con before_content.
    before_content TEXT,
    -- Contenuto POST-modifica (NULL se op='deleted'). Salvato per consentire
    -- "redo" e visualizzare il diff anche dopo che il file e' stato
    -- ulteriormente modificato sul filesystem.
    after_content TEXT,
    -- SHA-256 dei contenuti, in hex. Utili per dedup e per validare al revert
    -- che lo stato corrente del file sia ancora quello che abbiamo registrato
    -- come after (altrimenti il revert sovrascriverebbe modifiche manuali
    -- dell'utente fatte nel frattempo: meglio segnalare conflitto).
    before_sha256 TEXT,
    after_sha256 TEXT,
    before_size BIGINT,
    after_size BIGINT,
    -- Tracking del ripristino: se questa mutazione e' gia' stata revertita,
    -- reverted_at e' valorizzato e reverted_by_mutation_id punta alla nuova
    -- mutazione (op='reverted') generata dal revert. Cosi' la storia rimane
    -- lineare e ispezionabile.
    reverted_at TIMESTAMPTZ,
    reverted_by_mutation_id BIGINT REFERENCES file_mutations(id) ON DELETE SET NULL,
    -- Se questa mutazione e' essa stessa un revert, la colonna punta alla
    -- mutazione originale ripristinata.
    reverts_mutation_id BIGINT REFERENCES file_mutations(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indici per le query principali del pannello UI:
--   1) "ultime mutazioni del progetto" (lista chronologica DESC)
--   2) "ultima mutazione di un file" (per il revert "torna allo stato precedente")
--   3) "mutazioni di una sessione" (per il diff complessivo del run)
CREATE INDEX IF NOT EXISTS idx_file_mutations_project_created
    ON file_mutations (project_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_file_mutations_project_path_created
    ON file_mutations (project_id, file_path, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_file_mutations_session_created
    ON file_mutations (session_id, created_at DESC)
    WHERE session_id IS NOT NULL;

COMMENT ON TABLE file_mutations IS
    'Storico ripristinabile delle modifiche file dell''agente (mig 0349).';
COMMENT ON COLUMN file_mutations.before_content IS
    'Contenuto pre-modifica per il ripristino. NULL se il file non esisteva.';
COMMENT ON COLUMN file_mutations.after_content IS
    'Contenuto post-modifica registrato al momento della scrittura.';
