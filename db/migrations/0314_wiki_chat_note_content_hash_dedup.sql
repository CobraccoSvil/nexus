-- ═══════════════════════════════════════════════════════════════════════════
-- 0314_wiki_chat_note_content_hash_dedup.sql
--
-- Root cause (regola H): i wiki_docs kind='chat_note' venivano duplicati quando
-- l'utente inviava lo STESSO testo in messaggi distinti. La colonna body_hash
-- esistente NON intercetta questi duplicati perche' body_md include metadati
-- per-messaggio (created_at con microsecondi, session_id, message_id): due
-- messaggi con testo identico producono body_hash diversi (verificato: 25/25
-- body_hash distinti, ma 13 righe sono duplicati di contenuto reale).
--
-- Fix definitivo: deduplicare sul CONTENUTO UTENTE normalizzato, non sul
-- body_md arricchito. Introduciamo wiki_docs.content_hash = sha256(btrim(testo
-- utente)) e un indice UNIQUE parziale su (scope, COALESCE(project_id,''),
-- content_hash) WHERE kind='chat_note'. Il worker chat_note popola content_hash
-- e usa ON CONFLICT DO NOTHING (vedi chat_note_worker.rs).
--
-- Ordine obbligatorio (l'indice UNIQUE fallirebbe con duplicati presenti):
--   1) aggiunge la colonna content_hash
--   2) back-fill per le righe chat_note esistenti (testo = parte dopo "---")
--   3) deduplica i dati esistenti tenendo la riga piu' vecchia (created_at ASC)
--   4) crea l'indice UNIQUE parziale
--
-- Righe collegate: tutte le FK verso wiki_docs sono ON DELETE CASCADE
--   (wiki_links.from_doc_id/to_doc_id, wiki_concept_triples.subj_doc_id/
--   obj_doc_id, wiki_doc_revisions.doc_id) -> la DELETE dei doc duplicati
--   propaga automaticamente, nessuna cancellazione manuale necessaria.
-- ═══════════════════════════════════════════════════════════════════════════

BEGIN;

-- pgcrypto fornisce digest(): gia' presente in questo DB, CREATE IF NOT EXISTS
-- e' idempotente e protegge ambienti freschi.
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- 1) Colonna content_hash (nullable: valorizzata solo per chat_note).
ALTER TABLE wiki_docs ADD COLUMN IF NOT EXISTS content_hash text;

-- 2) Back-fill per le righe chat_note esistenti.
--    Il body_md ha forma "...metadati...\n---\n\n<testo utente>\n". Estraiamo il
--    testo utente con split_part sul separatore e applichiamo btrim (stessa
--    normalizzazione del worker Rust: content.trim()).
UPDATE wiki_docs
SET content_hash = encode(
        digest(btrim(split_part(body_md, E'---\n\n', 2)), 'sha256'),
        'hex'
    )
WHERE kind = 'chat_note'
  AND content_hash IS NULL;

-- 3) Deduplica i chat_note esistenti: per ogni (scope, project_id, content_hash)
--    teniamo la riga piu' vecchia (created_at ASC, id ASC come tie-breaker
--    deterministico). Le FK CASCADE rimuovono wiki_links / wiki_concept_triples
--    / wiki_doc_revisions collegate alle righe cancellate.
WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (
               PARTITION BY scope, COALESCE(project_id::text, ''), content_hash
               ORDER BY created_at ASC, id ASC
           ) AS rn
    FROM wiki_docs
    WHERE kind = 'chat_note'
      AND content_hash IS NOT NULL
)
DELETE FROM wiki_docs
WHERE id IN (SELECT id FROM ranked WHERE rn > 1);

-- 4) Indice UNIQUE parziale: previene nuovi duplicati di contenuto chat_note.
--    Scope allineato a uq_wiki_docs_slug (COALESCE(project_id::text,'')).
CREATE UNIQUE INDEX IF NOT EXISTS uq_wiki_docs_chat_note_content
    ON wiki_docs (scope, COALESCE((project_id)::text, ''::text), content_hash)
    WHERE kind = 'chat_note';

COMMIT;
