-- Migrazione 0521 — EmbeddingStore: compressione semantica del contesto agentico.
--
-- Aggiunge i settings che governano il continuity-trim SEMANTICO (scarto degli
-- atomi vecchi irrilevanti al focus del turno, via embedding + coseno) e l'offload
-- RAG retrievabile del contesto (tool_result compressi + originali del
-- rolling-summary). Le porte EmbeddingStore/ContextOffload sono SEMPRE iniettate nel
-- motore nativo (crates/nexus-agent-graph); questi flag decidono se scattano.
--
-- Tutti OFF/valori-neutri di default: con questi valori il comportamento del motore
-- e' BIT-IDENTICO a prima della migrazione (nessun embed, nessun offload). Un admin
-- attiva selettivamente. Idempotente: ON CONFLICT (key) DO NOTHING.
--
-- CONSOLIDAMENTO (regola L): rimuove la config ORFANA dell'era brain Python per lo
-- STESSO concern, mai portata nel motore Rust e non letta da alcun codice vivo — il
-- "continuity gate semantico" (mig 0397) e il "rag_offload" (mig 0217). Questa
-- migrazione e' l'unico set di chiavi per continuity-trim + offload del contesto.

BEGIN;

-- Rimozione config orfana (brain-era, nessun reader Rust): sostituita dai settaggi
-- continuity_trim_* / *_offload_enabled sotto. DELETE idempotente (no-op se assenti).
DELETE FROM settings WHERE key IN (
    'agent.context.continuity_gate_enabled',       -- mig 0397 (continuity gate)
    'agent.context.continuity_min_score',          -- mig 0397
    'agent.context.continuity_keep_recent',        -- mig 0397
    'agent.context.rag_offload.enabled',           -- mig 0217 (context_no_loss_rag)
    'agent.context.rag_offload.min_chars',         -- mig 0217
    'agent.context.rag_offload.max_chunks_per_item', -- mig 0217
    'agent.context.rag_offload.top_k',             -- mig 0217
    'agent.context.rag_offload.snippet_max_chars'  -- mig 0217
);

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    -- ── Continuity-trim SEMANTICO (EmbeddingStore) ──
    ('agent.context.continuity_trim_enabled', 'false', 'agent',
     'Se true, al cambio-fase il motore nativo scarta dal prefisso vecchio gli atomi (turno assistant + i suoi tool_result) semanticamente IRRILEVANTI al focus del turno, invece del solo troncamento posizionale. Usa l''embedder ONNX MiniLM in-process + coseno. Default false (bit-identico).',
     NOW()),
    ('agent.context.continuity_trim_min_score', '0.25', 'agent',
     'Soglia coseno (0..1) sotto la quale un atomo e'' considerato irrilevante al focus e viene scartato dal continuity-trim. Piu'' alta = piu'' aggressivo. Default 0.25.',
     NOW()),
    ('agent.context.continuity_trim_max_drop', '8', 'agent',
     'Cap massimo di messaggi scartabili dal continuity-trim in una singola passata (rete di sicurezza). Default 8.',
     NOW()),

    -- ── Offload RAG retrievabile del contesto (ContextOffload) ──
    ('agent.context.compress_offload_enabled', 'false', 'agent',
     'Se true, i tool_result compressi al cambio-fase vengono OFFLOADATI su RAG (source_kind=tool_result) e il marker porta un ref recuperabile via nexus_search_semantic, invece del solo "[... compresso ...]". Default false (degrado a marker come oggi).',
     NOW()),
    ('agent.context.rolling_summary_offload_enabled', 'false', 'agent',
     'Se true, gli originali del rolling-summary vengono indicizzati su RAG (source_kind=chat_history, filtrabili per session_id) PRIMA di essere sostituiti dal riassunto, cosi'' restano recuperabili via nexus_search_semantic. Default false.',
     NOW())
ON CONFLICT (key) DO NOTHING;

COMMIT;
