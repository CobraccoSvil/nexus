-- 0326_rag_chunker_algorithm_flag.sql
--
-- Feature flag per scegliere l'algoritmo di chunking testo usato dal servizio
-- embeddings (Wave 8a / Residuo R4, regola L + regola G).
--
-- Valori ammessi:
--   - 'legacy'  (DEFAULT): split per linee greedy, NO overlap. Algoritmo
--                storico di brain/embeddings/service.py::_chunk_text.
--                I chunk corrispondono a quelli gia' indicizzati nella
--                collection Qdrant esistente. Valore safe per default.
--   - 'unified': algoritmo unificato di brain/utils/text_chunk.py (paritetico
--                a crates/mcp-core/src/rag/chunker.rs, sliding window char con
--                overlap e smart trimming su whitespace).
--                ATTIVARE SOLO DOPO RE-INDEX della collection Qdrant: i chunk
--                saranno diversi -> vettori diversi -> recall RAG diverso se
--                misto a vettori 'legacy'.
--
-- Procedura switch (richiede downtime breve dell'indexing):
--   1. UPDATE settings SET value='unified' WHERE key='rag.chunker.algorithm';
--   2. Drop e ricrea la collection Qdrant (es. tool admin "Vector Maintenance").
--   3. Re-indicizza tutti i file da brain (workers/chat_indexer.py + manual).
--   4. Verifica recall su query benchmark prima di considerare il rollout completo.
--
-- Rollback: UPDATE settings SET value='legacy' ...; eventualmente re-index
-- inverso. Il flag e' DB-driven (regola G), zero patch codice.

INSERT INTO settings (key, value, description)
VALUES (
  'rag.chunker.algorithm',
  'legacy',
  'Algoritmo di chunking testo per indicizzazione embeddings: ''legacy'' (split per linee, NO overlap, default safe) oppure ''unified'' (sliding window char con overlap, paritetico al Rust chunker, richiede re-index Qdrant). Vedi migrazione 0326.'
)
ON CONFLICT (key) DO NOTHING;
