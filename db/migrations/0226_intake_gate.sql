-- Migrazione 0226: Intake Gate multi-asse (Componente 1).
--
-- Il gate, all'arrivo di una richiesta, fa UNA ricerca KB + UNA classificazione
-- LLM della RELAZIONE con la knowledge esistente: nuova | duplicate |
-- refinement | correction. Assorbe il decision-lookup del Cluster 4 (gate
-- unico). Tutto gated default-OFF. Le colonne off_topic/relevance_score sono
-- gia' in 0224. Il prompt del classificatore e' inline nel brain (compito
-- interno autocontenuto), quindi qui solo settings + purpose model.

-- Settings (category 'orchestrator', prefisso 'clarify.' per riuso del loader
-- esistente in clarify_or_expand_node._load_config).
INSERT INTO settings (key, value, category, description) VALUES
    ('clarify.intake_gate_enabled', 'false', 'orchestrator',
     'Comp.1: abilita il gate di intake (classifica la relazione richiesta vs KB: nuova/duplicate/refinement/correction). Assorbe il decision-lookup del Cluster 4.'),
    ('clarify.intake_match_min_score', '0.7', 'orchestrator',
     'Comp.1: soglia minima di similarita per considerare la richiesta correlata a una nota esistente.'),
    ('clarify.intake_topk', '5', 'orchestrator',
     'Comp.1: numero di note candidate recuperate dal gate di intake.')
ON CONFLICT (key) DO NOTHING;

-- Purpose model per il gate: tier 'light' dinamico (risolto dal catalog),
-- richiede tool use (il gate emette un tool_use intake_classify). provider/
-- model_id sono solo l'ultimo fallback se il catalog non ha candidati (stesso
-- pattern di understanding, mig 0207). Coerente con la colonna reale `purpose`.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes) VALUES
    ('intake_gate', 'openai', 'gpt-4o-mini', 'light', 'reasoning', true,
     'Comp.1: classificazione relazione richiesta vs KB (gate intake, tier light dinamico)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();
