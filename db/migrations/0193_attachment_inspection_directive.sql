-- Migrazione 0193: ingestion intelligente allegati (ADR 0011).
--
-- Contesto: con la 0192 abbiamo introdotto nexus_list_attachments /
-- nexus_read_attachment per accedere agli allegati oltre il blocco <allegati>
-- inline. Restava un buco: file con MIME application/octet-stream (es. .make
-- che e' in realta' uno ZIP, .fig Figma, .dat opachi) facevano arrendere il
-- modello con "non posso leggere binari" perche' il prompt non gli diceva di
-- ispezionare i magic bytes prima.
--
-- Questa mig:
--   1) Aggiunge un blocco <attachment_investigation> ai system prompt
--      system.nexus_base e agent.coder.base che istruisce l'agente a chiamare
--      nexus_inspect_attachment davanti a MIME/estensioni sospette e a
--      usare il tool di estrazione corretto in base al campo "kind".
--   2) Registra i settings agent.attachment.* con default safe per i nuovi
--      tool (limiti su entries archivio, byte testo PDF, righe XLSX,
--      payload Figma). I valori vengono letti con cache 60s da
--      crates/mcp-core/src/agent_tools/attachment_settings.rs.
--
-- Idempotente: rileva la presenza del blocco / chiavi.
--
-- Riferimenti:
--  - ADR 0011 docs/.nexus-vault/adr/0011-attachment-inspection-pipeline.md
--  - Tool: crates/mcp-core/src/agent_tools/attachment_inspector.rs
--  - Tool: crates/mcp-core/src/agent_tools/archive_tools.rs
--  - Tool: crates/mcp-core/src/agent_tools/document_tools.rs
--  - Tool: crates/mcp-core/src/agent_tools/figma_tools.rs

DO $$
DECLARE
    directive TEXT := E'\n\n<attachment_investigation>\n'
        || E'Se vedi un allegato nel blocco <allegati> con mime application/octet-stream, application/x-zip, o estensione sospetta (.make, .dat, .bin, .pkg, .fig, .aux), NON dichiarare subito "non posso leggerlo". Chiama prima `nexus_inspect_attachment(attachment_id="<id>")` per scoprire il vero formato dai magic bytes.\n\n'
        || E'In base al campo "kind" nella risposta usa il tool di estrazione adeguato:\n'
        || E'  - kind=zip|tar|tar.gz: `nexus_list_archive_entries` + `nexus_read_archive_entry`\n'
        || E'  - kind=pdf: `nexus_extract_pdf_text(page_start?, page_end?)`\n'
        || E'  - kind=docx: `nexus_extract_docx_text`\n'
        || E'  - kind=xlsx: `nexus_extract_xlsx_data(sheet_name?)`\n'
        || E'  - kind=pptx: `nexus_list_archive_entries` (esplora ppt/slides/)\n'
        || E'  - kind=figma: `nexus_extract_figma_structure` (MVP: stringhe + hint)\n'
        || E'  - kind=image: il framework instradera'' automaticamente verso un modello vision se la richiesta lo merita (segnala all''utente la necessita'')\n'
        || E'  - kind=binary (sconosciuto): come ultimo resort `nexus_read_attachment(encoding="base64")`\n\n'
        || E'Non rinunciare al primo "sembra binario": investiga sempre prima con nexus_inspect_attachment. Un file .make e'' tipicamente uno ZIP che contiene canvas.fig (Figma).\n'
        || E'</attachment_investigation>';
BEGIN
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'system.nexus_base'
       AND content NOT LIKE '%<attachment_investigation>%';

    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'agent.coder.base'
       AND content NOT LIKE '%<attachment_investigation>%';
END $$;

-- Settings con limiti operativi (cache 60s lato Rust).
INSERT INTO settings (key, value, category, description, is_secret)
VALUES
    (
        'agent.attachment.archive_entry_max_bytes',
        '204800',
        'agent',
        'Max byte letti da una singola entry di archivio (nexus_read_archive_entry). Default 200KB.',
        FALSE
    ),
    (
        'agent.attachment.archive_max_entries',
        '1000',
        'agent',
        'Max entries elencate da nexus_list_archive_entries prima della troncatura. Default 1000.',
        FALSE
    ),
    (
        'agent.attachment.pdf_max_text_bytes',
        '102400',
        'agent',
        'Max byte di testo estratto da nexus_extract_pdf_text in totale (su tutte le pagine richieste). Default 100KB.',
        FALSE
    ),
    (
        'agent.attachment.xlsx_max_rows',
        '1000',
        'agent',
        'Max righe restituite da nexus_extract_xlsx_data. Default 1000.',
        FALSE
    ),
    (
        'agent.attachment.figma_max_bytes',
        '51200',
        'agent',
        'Max byte del payload canvas.fig estratti da nexus_extract_figma_structure. Default 50KB.',
        FALSE
    )
ON CONFLICT (key) DO NOTHING;
