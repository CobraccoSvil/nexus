-- 0442: estrazione nel DB del system prompt del summarizer Python (regola G/D).
--
-- _SUMMARIZE_SYSTEM (brain/agents/summarizer.py:54) era hardcoded. Chiave
-- DEDICATA system.summarizer_system: NON si riusa system.session_compact_structured
-- (mig 0413) perche' hanno contratti di output DIVERSI (il compact Rust chiede
-- JSON {summary_markdown, decisions}, il summarizer Python chiede markdown con
-- sezioni ## e ne fa il parsing) e chiamanti diversi. Concern distinti, non
-- duplicazione.
--
-- Il codice Python lo legge via prompt_registry.get_prompt con FALLBACK alla
-- costante (graceful degradation se DB down). Caricabile dal brain grazie al
-- loader esteso ai system.% (mig 0441 / prompt_registry.py).
--
-- Idempotente: ON CONFLICT (key) DO NOTHING.

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
(
    'system.summarizer_system',
    'system',
    'System prompt del summarizer (compattazione conversazione, output markdown)',
    $sum$Sei un assistente che produce riassunti tecnici concisi di conversazioni multi-turno fra utente e agente AI sviluppatore.

<obiettivo>
Comprimere la cronologia in un singolo riassunto strutturato che preservi le informazioni operative critiche e permetta all'agente di continuare il lavoro senza perdere contesto.
</obiettivo>

<contenuto_obbligatorio>
- File letti / modificati (path completi)
- Errori incontrati e fix applicati
- Decisioni prese (architetturali, di scelta libreria, di refactor)
- Comandi eseguiti con esito
- Stato attuale del task: cosa e' fatto, cosa resta
</contenuto_obbligatorio>

<formato_output>
Markdown strutturato con sezioni: ## File toccati, ## Errori e fix, ## Decisioni, ## Stato. Niente preamboli ne' chiusure conversazionali. Massimo 800 token.
</formato_output>$sum$,
    'migration_0442'
)
ON CONFLICT (key) DO NOTHING;
