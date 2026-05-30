-- Mig 0216 — Eliminazione dei limiti che TRONCANO/PERDONO dati durante
-- l'estrazione e la lettura di allegati e file (backend Rust mcp-core).
--
-- PRINCIPIO: "mai troncare-e-buttare". Ogni estrazione/lettura processa
-- l'INTERO contenuto. Quando il risultato e' grande viene scritto su disco
-- (es. nexus_extract_figma_code) e/o indicizzato in RAG, restituendo
-- all'agente un puntatore invece del troncamento. Nessuna perdita di dati.
--
-- Coerenza col codice (regola G: configurazione in DB, niente fallback
-- hardcoded): i campi corrispondenti sono stati rimossi da
-- `AttachmentLimits` (crates/mcp-core/src/agent_tools/attachment_settings.rs)
-- e dalla relativa query/match in `load_from_db`. Di conseguenza qui
-- ELIMINIAMO le chiavi `settings` ormai inutilizzate.
--
-- RESTANO (non sono cap di contenuto):
--   * agent.attachment.read_cache_ttl_seconds          (igiene cache letture)
--   * agent.attachment.figma_make_ai_chat_max_load_bytes (guardia anti-OOM)
-- La guardia anti-OOM viene alzata a un valore altissimo (512 MB): rete di
-- sicurezza contro file patologici, NON un budget di contenuto.
--
-- Idempotente.

-- 1) Cap di contenuto sull'estrazione: eliminati (mig 0193, 0196, 0210).
DELETE FROM settings WHERE key IN (
    'agent.attachment.archive_entry_max_bytes',
    'agent.attachment.archive_max_entries',
    'agent.attachment.pdf_max_text_bytes',
    'agent.attachment.xlsx_max_rows',
    'agent.attachment.figma_max_bytes',
    'agent.attachment.figma_make_chat_messages_max_chars',
    'agent.attachment.figma_make_chat_messages_max_count',
    'agent.attachment.figma_make_assistant_message_max_chars',
    'agent.attachment.figma_make_code_max_total_bytes'
);

-- 2) Budget di pre-extraction inline / sessione: eliminati (mig 0195). Non
--    piu' referenziati dal codice: la pre-extraction passa ora interamente per
--    il RAG (rag::index_attachment + rag::search_semantic), che indicizza il
--    contenuto completo senza budget arbitrario che tagli dati.
DELETE FROM settings WHERE key IN (
    'agent.attachment.preextract_enabled',
    'agent.attachment.preextract_max_chars',
    'agent.attachment.session_read_budget_bytes'
);

-- 3) Guardia anti-OOM sul caricamento in RAM di ai_chat.json (Figma Make):
--    NON e' un cap di contenuto, e' una rete di sicurezza estrema. La alziamo
--    a 512 MB. Manteniamo la chiave (il codice la legge ancora come guardia).
INSERT INTO settings (key, value, category, description, updated_at)
VALUES (
    'agent.attachment.figma_make_ai_chat_max_load_bytes',
    '536870912',
    'agent',
    'Guardia anti-OOM ESTREMA (NON un cap di contenuto) sul caricamento in RAM del file ai_chat.json di un archivio Figma Make prima del parsing. Default 512 MB: rete di sicurezza contro file patologici. I .make reali stanno nell''ordine dei MB.',
    NOW()
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description,
        updated_at = NOW();
