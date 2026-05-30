-- Mig 0217 — Context no-loss via offload RAG (zero perdita dati nel contesto LLM).
--
-- Decisione: il context window del modello e' un limite FISICO del provider, ma
-- il DATO non deve mai essere PERSO. Prima che il brain tronchi/comprima/scarti un
-- tool result o un messaggio vecchio (brain/agents/nodes.py + context_offload.py),
-- il contenuto COMPLETO viene indicizzato in Qdrant (collection tool_results_chunks,
-- gia' definita dalla mig 0200) e diventa recuperabile via nexus_search_semantic.
-- La compressione nel prompt resta necessaria, ma diventa LOSSLESS a livello di
-- sistema. Vedi regola H (causa radice: il troncamento distruttivo era la causa
-- della perdita; il fix non e' alzare i limiti ma eliminare la perdita).
--
-- Tutti i numeri sono DB-driven (regola G): nessun magic number nel codice tranne
-- i defaults safe usati se il DB e' down (cache 60s). Idempotente.
--
-- Note:
--   * agent.context.rag_offload.enabled e' il flag master (default true).
--   * I parametri di recupero RAG (top_k, snippet) sono alzati rispetto ai vecchi
--     valori hardcoded (5 -> 12, 400 -> 4000): con l'offload il RAG e' la fonte di
--     verita' del contenuto troncato, quindi il recupero non deve essere stretto.
--   * chunk_size / chunk_overlap / collection_tool_results sono riusati dalla
--     pipeline RAG unificata (mig 0200), nessuna duplicazione.

INSERT INTO settings (key, value, category, description, updated_at)
VALUES
    (
        'agent.context.rag_offload.enabled',
        'true',
        'agent',
        'Flag master offload RAG lossless. Se true (default), prima di troncare/comprimere/scartare un tool result o messaggio vecchio il brain indicizza il contenuto COMPLETO in Qdrant (tool_results_chunks) cosi'' nessun dato viene perso e resta recuperabile via nexus_search_semantic. Se false, degrada al vecchio troncamento distruttivo.',
        NOW()
    ),
    (
        'agent.context.rag_offload.min_chars',
        '2000',
        'agent',
        'Soglia minima caratteri sotto la quale NON si indicizza un contenuto in RAG: sotto soglia il contenuto sta gia'' intero nel prompt, nessuna perdita possibile. Default 2000.',
        NOW()
    ),
    (
        'agent.context.rag_offload.max_chunks_per_item',
        '500',
        'agent',
        'Numero massimo di chunk indicizzati per singolo contenuto offloadato (anti-abuso: un file enorme non deve generare migliaia di point in un colpo). Oltre il cap il resto NON viene indicizzato e l''evento e'' loggato come WARN. Default 500.',
        NOW()
    ),
    (
        'agent.context.rag_offload.top_k',
        '12',
        'agent',
        'Numero di interazioni/snippet recuperati dal RAG inline per turno. Alzato da 5 (vecchio hardcoded) a 12: con l''offload lossless il RAG e'' la fonte di verita'' del contenuto troncato, quindi il recupero non deve essere artificialmente stretto.',
        NOW()
    ),
    (
        'agent.context.rag_offload.snippet_max_chars',
        '4000',
        'agent',
        'Limite caratteri per ogni snippet RAG incluso nel contesto. Alzato da 400 (vecchio hardcoded) a 4000: snippet piu'' ampi riducono i round-trip e non perdono il cuore del match.',
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
