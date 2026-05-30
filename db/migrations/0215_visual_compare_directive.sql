-- Mig 0215 - FASE 2 "resa Figma Make": direttiva di verifica visiva nei prompt.
--
-- Contesto: la mig 0211 ha aggiunto il blocco <figma_make_strategy> ai system
-- prompt principali (system.nexus_base, agent.coder.base) istruendo l'agente a
-- ESTRARRE e ADATTARE il codice del .make invece di reinventarlo. La FASE 2
-- (mig 0214 + crates/mcp-core/src/agent_tools/visual_compare.rs) aggiunge il
-- tool nexus_visual_compare per la VERIFICA VISIVA: screenshot dell'app avviata
-- confrontato con il design Figma di riferimento via modello vision.
--
-- Questa mig appende un sotto-blocco <visual_verification> agli stessi due
-- template: dopo aver avviato l'app, l'agente usa nexus_visual_compare e, se
-- similarity_score e' sotto la soglia configurata (default 85, setting
-- agent.visual_compare.similarity_threshold) o ci sono differenze di severita'
-- alta, corregge i file di stile/layout e ripete fino a convergenza o
-- esaurimento iterazioni.
--
-- NB (CLAUDE.md sez. D, fuori-chat vs chat): il loop e' guidato dall'agente in
-- modalita' Continuo, NON e' un ciclo hardcoded nel codice Rust. Il prompt e'
-- l'unico contratto del comportamento iterativo.
--
-- Niente modelli AI hardcoded (sez. G): il modello vision e' risolto da
-- nexus_purpose_model.visual_compare (mig 0214); la soglia dal setting.
--
-- Idempotente: append condizionato su content NOT LIKE '%<visual_verification>%'.
--
-- Riferimenti:
--   - FASE 1: db/migrations/0210_figma_make_code_extraction.sql
--   - FASE 3: db/migrations/0211_figma_make_strategy_directive.sql
--   - FASE 2 settings/purpose: db/migrations/0214_visual_compare_settings.sql
--   - Tool: crates/mcp-core/src/agent_tools/visual_compare.rs
--   - Schema XML prompt: db/migrations/0086_*.sql

DO $$
DECLARE
    directive TEXT := E'\n\n<visual_verification>\n'
        || E'VERIFICA VISIVA quando hai avviato un''app costruita a partire da un design Figma (.make) o da un mockup/screenshot allegato.\n\n'
        || E'Dopo aver avviato il dev server (porta da request_port, blocco <port_allocation>) e averne verificato il render senza errori console, confronta il risultato col design di riferimento usando il tool `nexus_visual_compare`:\n'
        || E'  nexus_visual_compare(url="http://localhost:<porta>/", reference="<attachment_id del .make o dell''immagine di riferimento>")\n\n'
        || E'Il tool screenshotta l''url, recupera l''immagine di riferimento (thumbnail.png dal .make oppure l''immagine allegata) e chiama un modello vision che ritorna: similarity_score (0-100), una lista di differences {category (colore|tipografia|layout|spaziatura|componente), severity (alta|media|bassa), description, suggested_fix}, screenshot_path (su disco), reference_source, model_used. Lo screenshot NON entra nel tuo contesto: e'' salvato su disco, lavora dal path se devi reispezionarlo.\n\n'
        || E'Iterazione (modalita'' Continuo): se similarity_score e'' sotto la soglia raccomandata (default 85, configurata in agent.visual_compare.similarity_threshold) OPPURE c''e'' almeno una differenza con severity="alta", applica i suggested_fix correggendo i file di STILE/LAYOUT (tailwind.config, classi Tailwind, CSS, spaziature, posizionamento dei componenti) e poi richiama nexus_visual_compare sullo stesso url. Ripeti fino a quando similarity_score supera la soglia e non restano differenze di severita'' alta, oppure fino a esaurire le iterazioni a tua disposizione. Privilegia le correzioni che il modello segnala come severity="alta", poi "media".\n\n'
        || E'VINCOLI: correggi SOLO stile/layout/spaziatura/palette/tipografia per avvicinarti al design; non stravolgere la logica applicativa ne'' la struttura dei componenti gia'' estratti dal .make (restano la fonte di verita'', vedi <figma_make_strategy>). Se nexus_visual_compare ritorna un errore strutturato (Playwright non disponibile, url non raggiungibile, vision non configurata) NON insistere in loop: segnala il problema e procedi con il resto del task.\n'
        || E'</visual_verification>';
BEGIN
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now(),
           updated_by = 'mig_0215'
     WHERE key = 'system.nexus_base'
       AND content NOT LIKE '%<visual_verification>%';

    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now(),
           updated_by = 'mig_0215'
     WHERE key = 'agent.coder.base'
       AND content NOT LIKE '%<visual_verification>%';
END $$;

-- Verifica idempotente: entrambi i template target devono contenere il blocco.
DO $$
DECLARE
    missing INT;
BEGIN
    SELECT count(*) INTO missing
      FROM nexus_prompt_templates
     WHERE key IN ('system.nexus_base', 'agent.coder.base')
       AND content NOT LIKE '%<visual_verification>%';
    IF missing > 0 THEN
        RAISE EXCEPTION 'Mig 0215: blocco <visual_verification> assente in % template target (atteso 0). Verificare che le chiavi esistano.', missing;
    END IF;
END $$;
