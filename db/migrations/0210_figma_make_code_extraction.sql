-- Mig 0210 - FASE 1 "resa Figma Make": estrazione code-snapshot su disco.
--
-- Contesto: un file .make Figma e' uno ZIP che dentro ai_chat.json contiene
-- GIA' l'intera app React/TypeScript/Tailwind, salvata come sequenza di
-- scritture file (fast_apply_tool/write_tool). Il tool
-- nexus_extract_figma_code (crates/mcp-core/src/agent_tools/figma_tools.rs)
-- ricostruisce l'ultima versione di ogni path e la materializza su disco
-- sotto la project_root (default figma_export/), ritornando solo un manifest
-- (niente contenuto nel contesto del modello).
--
-- Questa mig:
--   1) Alza agent.attachment.figma_make_ai_chat_max_load_bytes da 5 MB a
--      32 MB: gli ai_chat.json reali osservati pesano ~11 MB e il parsing del
--      code-snapshot richiede il documento completo. Il parser resta
--      parziale-tollerante (manifest partial=true se troncato).
--   2) Registra agent.attachment.figma_make_code_max_total_bytes (8 MB): tetto
--      ai byte totali scrivibili su disco dall'estrazione, per non saturare
--      il disco del progetto. Oltre la soglia il tool segnala partial=true.
--
-- I valori sono letti con cache 60s da
-- crates/mcp-core/src/agent_tools/attachment_settings.rs (regola G CLAUDE.md:
-- niente limiti hardcoded nei sorgenti).
--
-- Idempotente: UPDATE condizionato + INSERT ON CONFLICT DO NOTHING.

-- 1) Alza il limite di load di ai_chat.json (era 5 MB nella mig 0196).
--    Aggiorna solo se il valore corrente e' ancora il vecchio default 5 MB,
--    per non sovrascrivere una scelta manuale dell'amministratore.
UPDATE settings
   SET value = '33554432',
       description = 'Max byte caricati in RAM dal file ai_chat.json di un archivio Figma Make prima del parsing. Default 32 MB (gli ai_chat.json reali pesano ~11 MB e l''estrazione del code-snapshot richiede il documento completo). Se il file e'' piu'' grande viene troncato e l''estrazione e'' best-effort (partial=true).',
       updated_at = NOW()
 WHERE key = 'agent.attachment.figma_make_ai_chat_max_load_bytes'
   AND value = '5242880';

-- 2) Nuovo limite: tetto byte totali scrivibili su disco dal code-snapshot.
INSERT INTO settings (key, value, category, description, is_secret, updated_at)
VALUES
    (
        'agent.attachment.figma_make_code_max_total_bytes',
        '8388608',
        'agent',
        'Max byte totali scrivibili su disco dall''estrazione del code-snapshot Figma Make (nexus_extract_figma_code). Oltre la soglia il tool smette di scrivere e segnala partial=true nel manifest. Default 8 MB.',
        FALSE,
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
