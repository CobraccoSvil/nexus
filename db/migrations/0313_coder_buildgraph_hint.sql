-- Migrazione 0313: aggiunge la sezione <file_picking_policy> ai system prompt
-- coder/debugger per indirizzare l'agente verso `nexus_build_graph_info`
-- prima di modificare file di codice. Sostituisce l'eventuale raccomandazione
-- grep di ADR 0019 L4 (se applicata).
--
-- Sentinel string `ADR0020_BUILDGRAPH_v1` garantisce idempotenza su re-run.

DO $$
DECLARE
    sentinel TEXT := '<!-- ADR0020_BUILDGRAPH_v1 -->';
    block TEXT := E'\n\n<!-- ADR0020_BUILDGRAPH_v1 -->\n<file_picking_policy>\nPrima di modificare un file di codice (.ts, .tsx, .js, .jsx, .rs, .py, .go) USA SEMPRE `nexus_build_graph_info(project_id)` per ottenere la mappa autoritativa del build graph del progetto.\n\nOutput del tool:\n- include_globs: pattern di file inclusi dalla build\n- exclude_globs: pattern esclusi\n- generated_dirs: directory di output build (mai modificare manualmente)\n- entry_points: file entrypoint\n- sources: i file di config da cui e'' stata derivata la mappa (tsconfig.json, Cargo.toml, ecc.)\n\nVerifica che il file che stai per modificare matchi `include_globs` e NON matchi `exclude_globs`. Se ci sono duplicati (es. `BookingPage.tsx` esistente sia in `src/` che in `figma_export/`), scegli quello che e'' nel build graph.\n\nNexus emette warning automatici nel risultato di `write_file`/`edit_file` quando rileva un file fuori dal build graph. LEGGILI. Se vedi "ATTENZIONE: il file NON e'' nel build graph", FERMATI e cerca prima il file corretto.\n\nNON modificare mai file in `generated_dirs` (node_modules/, target/, dist/, build/, .next/, ecc.): la scrittura viene rifiutata automaticamente.\n</file_picking_policy>';
BEGIN
    UPDATE nexus_prompt_templates
    SET content = content || block
    WHERE key IN ('system.nexus_base', 'agent.coder.base', 'agent.general.debugger')
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    RAISE NOTICE 'Migrazione 0313 applicata: file_picking_policy ADR 0020 iniettata nei system prompt';
END
$$;
