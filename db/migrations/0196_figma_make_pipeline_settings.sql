-- Mig 0196 - Settings pipeline Figma Make (ADR 0011 sezione "Figma Make handling").
--
-- Contesto: il tool `nexus_extract_figma_structure` (crates/mcp-core/src/
-- agent_tools/figma_tools.rs) ora distingue Figma Make (ZIP con ai_chat.json)
-- da Figma legacy binario. Per Figma Make il contenuto autoritativo e' il
-- thread chat user/assistant: la specifica originale dell'app data al
-- generatore. I limiti operativi devono restare DB-driven (regola G CLAUDE.md):
-- niente hardcoded nei sorgenti.
--
-- I valori vengono letti con cache 60s da
-- crates/mcp-core/src/agent_tools/attachment_settings.rs.
--
-- Idempotente.

INSERT INTO settings (key, value, category, description, is_secret, updated_at)
VALUES
    (
        'agent.attachment.figma_make_ai_chat_max_load_bytes',
        '5242880',
        'agent',
        'Max byte caricati in RAM dal file ai_chat.json di un archivio Figma Make prima del parsing. Default 5 MB. Se il file e'' piu'' grande viene troncato (segnalato con ai_chat_truncated_at_load=true nella risposta del tool).',
        FALSE,
        NOW()
    ),
    (
        'agent.attachment.figma_make_chat_messages_max_chars',
        '51200',
        'agent',
        'Max caratteri cumulativi del testo estratto dai messaggi user+assistant del thread chat Figma Make. Default 50 KB. Oltre la soglia i messaggi residui vengono scartati (chat_messages_truncated=true).',
        FALSE,
        NOW()
    ),
    (
        'agent.attachment.figma_make_chat_messages_max_count',
        '20',
        'agent',
        'Max numero di messaggi (user + assistant) restituiti dal thread chat Figma Make. Default 20.',
        FALSE,
        NOW()
    ),
    (
        'agent.attachment.figma_make_assistant_message_max_chars',
        '2000',
        'agent',
        'Max caratteri di un singolo messaggio assistant prima della truncatura. I messaggi user (prompt originale) non vengono mai troncati singolarmente. Default 2000.',
        FALSE,
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
