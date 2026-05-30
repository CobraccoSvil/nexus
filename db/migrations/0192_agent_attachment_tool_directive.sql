-- Migrazione 0192: direttiva esplicita per accesso allegati + flag
-- enforcement porte hardcoded.
--
-- Problema A (allegati): il blocco <allegati> nel prompt iniziale e' limitato
-- a 50KB totali. Quando l'utente carica file grandi (PRD, CSV, log), il
-- contenuto viene mostrato solo come metadata. Senza una direttiva esplicita
-- l'agente non sa che esistono `nexus_list_attachments` e
-- `nexus_read_attachment` per leggere i contenuti a richiesta.
--
-- Problema B (porte): il check `port_scanner` blocca write_file/edit_file
-- che contengono porte hardcoded fuori dal bucket 20000-39999. Il flag
-- `agent.enforce_port_allocation` controlla questa enforcement (default true).
--
-- Fix: aggiunge un blocco <attachment_access> ai system prompt principali e
-- registra il setting. Idempotente: rileva la presenza del blocco / chiave.
--
-- Riferimenti:
--  - ADR 0010 docs/.nexus-vault/adr/0010-port-and-attachment-enforcement.md
--  - Tool: crates/mcp-core/src/agent_tools/attachments.rs
--  - Scanner: crates/mcp-core/src/agent_tools/port_scanner.rs

DO $$
DECLARE
    directive TEXT := E'\n\n<attachment_access>\n'
        || E'Quando l''utente allega uno o piu'' file al messaggio (PRD, CSV, log, sorgenti, immagini), trovi un blocco <allegati> nel prompt iniziale con i loro metadata. Se il blocco contiene gia'' il contenuto inline, leggilo direttamente. Se invece il contenuto e'' marcato come "non incluso" (perche'' il totale supera il budget inline di 50KB) oppure ti serve leggere oltre la porzione mostrata:\n\n'
        || E'1. Chiama `nexus_list_attachments` per ottenere la lista completa degli allegati della sessione con i loro UUID (campo `id`).\n'
        || E'2. Chiama `nexus_read_attachment(attachment_id="<uuid>", offset=0, length=102400)` per leggere fino a 100KB per chiamata.\n'
        || E'3. Se il file e'' piu'' grande di 100KB, chiama nexus_read_attachment piu'' volte con offset crescente finche'' il campo `truncated` ritorna false.\n'
        || E'4. Encoding "auto" (default) decide testo o base64 in base al MIME. Forza "text" o "base64" se necessario.\n\n'
        || E'NON inventare contenuti che non hai effettivamente letto. NON usare read_file su path inventati: gli allegati vivono in chat_message_attachments, non sul filesystem del progetto utente.\n'
        || E'</attachment_access>';
BEGIN
    -- system.nexus_base
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'system.nexus_base'
       AND content NOT LIKE '%<attachment_access>%';

    -- agent.coder.base
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'agent.coder.base'
       AND content NOT LIKE '%<attachment_access>%';
END $$;

-- Flag globale enforcement porte hardcoded.
-- Default true: ogni write_file/edit_file con porte fuori bucket 20000-39999
-- viene rifiutato. Disabilita solo se davvero serve (debug locale).
INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'agent.enforce_port_allocation',
    'true',
    'agent',
    'Se true, write_file/edit_file rifiutano sorgenti con porte TCP hardcoded fuori dal bucket Nexus 20000-39999 (vedi ADR 0010).',
    FALSE
)
ON CONFLICT (key) DO NOTHING;
