-- Mig 0211 - FASE 3 "resa Figma Make": direttiva di strategia nei system prompt.
--
-- Contesto e problema:
--   La FASE 1 (mig 0210, crates/mcp-core/src/agent_tools/figma_tools.rs) ha
--   aggiunto il tool nexus_extract_figma_code che, dato un allegato Figma
--   .make, estrae il codice React+TypeScript+Tailwind GIA' generato da Figma
--   Make (salvato dentro ai_chat.json), lo materializza su disco sotto la
--   project_root (default figma_export/) e ritorna SOLO un manifest
--   (files_written, total_files, total_bytes, target_dir, entrypoints,
--   detected_dependencies, partial, notes). nexus_inspect_attachment, per un
--   .make con codice, raccomanda nexus_extract_figma_code in
--   next_action_recommended.
--
--   Oggi pero' i system prompt agente istruiscono il modello a "creare l'app
--   dalla specifica": il modello REINVENTA l'UI e perde il design fedele di
--   Figma. La FASE 3 cambia la STRATEGIA: quando l'allegato e' un .make con
--   codice, il modello deve ESTRARRE e ADATTARE quel codice, mai rigenerarlo.
--
-- Fix definitivo (CLAUDE.md sez. H): aggiunge un blocco <figma_make_strategy>
--   ai system prompt principali usati per lo scaffolding (system.nexus_base e
--   agent.coder.base, gli stessi target di mig 0192/0193). Il blocco rispetta
--   lo schema XML dei prompt agente (CLAUDE.md sez. D / mig 0086) e NON
--   duplica il blocco esplorazione/anti-loop gia' presente: si limita alla
--   strategia "estrai-e-adatta" specifica per i .make Figma.
--
-- Niente codice Python necessario: l'hint per il .make e' gia' veicolato da
--   nexus_inspect_attachment (next_action_recommended), la strategia vive
--   interamente nel prompt + DB (preferibile, CLAUDE.md). Nessun modello AI
--   hardcoded (sez. G): le dipendenze sono prese da detected_dependencies del
--   manifest, non da una lista fissa.
--
-- Idempotente: append condizionato su content NOT LIKE '%<figma_make_strategy>%'.
--
-- Riferimenti:
--   - FASE 1: db/migrations/0210_figma_make_code_extraction.sql
--   - Tool:   crates/mcp-core/src/agent_tools/figma_tools.rs
--   - Schema XML prompt: db/migrations/0086_*.sql
--   - Direttive allegati: db/migrations/0192_*.sql, 0193_*.sql

DO $$
DECLARE
    directive TEXT := E'\n\n<figma_make_strategy>\n'
        || E'STRATEGIA OBBLIGATORIA quando tra gli allegati c''e'' un file Figma .make (o quando nexus_inspect_attachment ritorna next_action_recommended con tool="nexus_extract_figma_code"):\n\n'
        || E'Un .make contiene GIA'' l''intera app React+TypeScript+Tailwind generata da Figma Make. Quel codice E'' LA FONTE DI VERITA'' DEL DESIGN. NON descrivere l''app, NON reinventarla, NON ridisegnare l''UI dalla specifica testuale. Estrai e adatta il codice esistente.\n\n'
        || E'Workflow:\n'
        || E'1. Chiama PRIMA `nexus_extract_figma_code`. Scrive i file su disco sotto target_dir (default figma_export/) e ritorna SOLO un manifest: {files_written, total_files, total_bytes, target_dir, entrypoints, detected_dependencies, partial, notes}. Il codice NON entra nel tuo contesto: lavora dai path scritti su disco (read_file/list_files su target_dir quando ti serve ispezionare un file).\n'
        || E'2. package.json: usa ESATTAMENTE le `detected_dependencies` del manifest come dependencies (niente versioni inventate a caso: usa range ^ ragionevoli o "latest" se incerto). Aggiungi le devDependencies di build SOLO se il codice estratto le richiede: se usa Vite (vite, @vitejs/plugin-react), se usa Tailwind (tailwindcss, postcss, autoprefixer), se usa TypeScript (typescript, @types/react, @types/react-dom). Non aggiungere dipendenze non referenziate dal codice.\n'
        || E'3. Config di runtime mancante: crea vite.config.(ts|js), tailwind.config.(ts|js) + postcss.config.js, tsconfig.json, e index.html con il div root e lo <script type="module"> che punta all''entrypoint indicato in `entrypoints`. Genera solo i file di config ASSENTI: se il manifest li ha gia'' scritti, non sovrascriverli.\n'
        || E'4. tailwind.config: DERIVALO dai token gia'' usati nel codice estratto. Le classi Tailwind sono gia'' nei file: il tuo compito e'' solo garantire che la config esista e che il campo `content` punti ai file giusti (es. ["./index.html","./src/**/*.{ts,tsx}"]). NON appiattire ne'' ridurre la palette/spacing/tipografia gia'' impliciti nel codice.\n'
        || E'5. Integra i file da target_dir nella struttura servita da Vite (tipicamente src/): spostali/copiali PRESERVANDONE IL CONTENUTO. Adattali SOLO per il runtime: correzione degli import path relativi e mock dei dati. MAI per "semplificare", accorpare o ridisegnare.\n'
        || E'6. Backend/servizi dati: se il codice referenzia servizi (es. services/bookingService, api client, fetch verso endpoint locali), genera lo stub backend minimo COERENTE con le firme effettivamente usate nel codice estratto (stessi nomi funzione, stessa forma di request/response). Non inventare API non chiamate.\n'
        || E'7. Asset: le immagini del design stanno in images/ DENTRO il .make. Se il codice estratto referenzia asset locali (import o src verso file in images/ o assets/), recuperali con `nexus_read_archive_entry` dall''archivio .make e scrivili nella cartella asset del progetto. Se la FASE 1 non li ha estratti, usa placeholder coerenti (stesse dimensioni/aspect ratio, colore neutro) e segnalalo. NON inventare MAI URL di immagini esterne.\n'
        || E'8. Installa le dipendenze ed esegui il dev server: registra ogni porta TCP con request_port(label=...) come da blocco <port_allocation>. Non hardcodare porte.\n\n'
        || E'VINCOLO ASSOLUTO: i file estratti da Figma sono la fonte di verita'' del design. Le uniche modifiche ammesse sono quelle necessarie a farli girare nel runtime (import path, mock dati, wiring config). Qualunque modifica che alteri layout, palette, tipografia, spaziatura o componenti e'' una violazione: stai perdendo il design fedele che l''utente ha pagato in Figma.\n'
        || E'</figma_make_strategy>';
BEGIN
    -- system.nexus_base (system prompt principale chat/scaffolding)
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now(),
           updated_by = 'mig_0211'
     WHERE key = 'system.nexus_base'
       AND content NOT LIKE '%<figma_make_strategy>%';

    -- agent.coder.base (agente coder, scaffolding/architecture)
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now(),
           updated_by = 'mig_0211'
     WHERE key = 'agent.coder.base'
       AND content NOT LIKE '%<figma_make_strategy>%';
END $$;

-- Verifica idempotente: entrambi i template target devono contenere il blocco.
DO $$
DECLARE
    missing INT;
BEGIN
    SELECT count(*) INTO missing
      FROM nexus_prompt_templates
     WHERE key IN ('system.nexus_base', 'agent.coder.base')
       AND content NOT LIKE '%<figma_make_strategy>%';
    IF missing > 0 THEN
        RAISE EXCEPTION 'Mig 0211: blocco <figma_make_strategy> assente in % template target (atteso 0). Verificare che le chiavi esistano.', missing;
    END IF;
END $$;
