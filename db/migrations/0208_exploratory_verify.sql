-- Migrazione 0208: verifica esplorativa RAG-informed (Cluster 3).
--
-- Dopo che i criteri deterministici del verifier sono PASSATI, un passo LLM
-- economico cerca anomalie NON coperte dai criterion pre-definiti, informato
-- dai pattern di fallimento passati (RAG via nexus_search_semantic su kb +
-- chat_history). Il deterministico (criteria_runner) resta primario e 100%
-- deterministico: la parte LLM vive nel verifier_node, gated, con cap dedicato.
--
-- Tutto default-OFF. Modello risolto via purpose 'exploratory_verify' tier
-- light (regola G, riusa la risoluzione tier-based della mig 0203).

INSERT INTO settings (key, value, category, description) VALUES
    ('orchestrator.exploratory_verify_enabled', 'false', 'orchestrator',
     'Se true, dopo i criteri deterministici passati il verifier esegue un controllo LLM esplorativo (RAG-informed) per anomalie non coperte.'),
    ('orchestrator.exploratory_verify_max_cycles', '1', 'orchestrator',
     'Cap di cicli della verifica esplorativa per todo (anti-loop). Al cap si promuove comunque (deterministico primario).'),
    ('orchestrator.exploratory_verify_topk', '5', 'orchestrator',
     'Quanti pattern di fallimento passati recuperare via ricerca semantica.'),
    ('orchestrator.exploratory_verify_min_score', '0.5', 'orchestrator',
     'Soglia minima di similarita'' per i pattern di fallimento recuperati.')
ON CONFLICT (key) DO NOTHING;

-- Purpose model per il controllo esplorativo: tier light (economico),
-- capability reasoning, no tool use. Provider/model_id solo come ultimo
-- fallback (risolto dinamicamente dal catalog).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes) VALUES
    ('exploratory_verify', 'openai', 'gpt-4o-mini', 'light', 'reasoning', false,
     'Cluster 3: verifica esplorativa economica RAG-informed (mig 0208)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();
