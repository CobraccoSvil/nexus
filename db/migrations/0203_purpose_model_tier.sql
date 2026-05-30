-- Migrazione 0203: risoluzione tier-based per nexus_purpose_model.
--
-- Contesto: architettura "orchestrator-worker". Il purpose 'planner' (chi
-- organizza/scompone il lavoro) deve usare un modello FORTE selezionato
-- dinamicamente dal catalog (tier 'heavy'), non un modello fisso. I worker
-- che eseguono i sotto-task usano modelli economici (tier 'medium'/'light').
--
-- Approccio: aggiungiamo a nexus_purpose_model una colonna `tier` opzionale.
-- Quando valorizzata, il resolver Rust (internal_routing.rs::resolve_purpose)
-- sceglie a runtime il miglior modello del catalog per quel tier+capability
-- invece del (provider, model_id) statico. I purpose senza tier restano
-- risolti staticamente (retrocompatibile). Il (provider, model_id) della riga
-- resta come ULTIMO fallback se il catalog non ha candidati per quel tier
-- (es. tutti in cooldown). Nessun nome modello hardcoded nel codice (regola G).

ALTER TABLE nexus_purpose_model
    ADD COLUMN IF NOT EXISTS tier TEXT
        CHECK (tier IS NULL OR tier IN ('light', 'medium', 'heavy'));
ALTER TABLE nexus_purpose_model
    ADD COLUMN IF NOT EXISTS required_capability TEXT;
ALTER TABLE nexus_purpose_model
    ADD COLUMN IF NOT EXISTS requires_tool_use BOOLEAN NOT NULL DEFAULT false;

COMMENT ON COLUMN nexus_purpose_model.tier IS
'Se valorizzato (light/medium/heavy), il modello e'' risolto dinamicamente dal catalog per quel tier; provider/model_id diventano l''ultimo fallback.';

-- planner -> tier heavy dinamico (capability reasoning, tool use richiesto).
-- provider/model_id aggiornati a un heavy ragionevole come ultimo fallback
-- (prima era openai/gpt-4o-mini, un modello LIGHT: errato per chi pianifica).
UPDATE nexus_purpose_model
   SET tier = 'heavy',
       required_capability = 'reasoning',
       requires_tool_use = true,
       provider = 'google',
       model_id = 'gemini-2.5-pro',
       notes = 'orchestrator-worker: planner forte, tier heavy dinamico (mig 0203)',
       updated_at = NOW()
 WHERE purpose = 'planner';

-- Worker dedicati ai sotto-task: economici ma capaci (tier medium).
-- worker_implement: scrittura/modifica di un singolo task atomico gia'
-- pianificato dal planner forte. worker_plan: sub-piano raro, mai il tier
-- heavy del planner principale.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes) VALUES
    ('worker_implement', 'openai', 'gpt-4.1', 'medium', 'code',      true,  'orchestrator-worker: esecuzione task atomico, economico ma capace (mig 0203)'),
    ('worker_plan',      'openai', 'o4-mini', 'medium', 'reasoning', false, 'orchestrator-worker: sub-piano raro, mai il tier heavy del planner (mig 0203)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- I sub-agent 'implement' e 'plan' usano i nuovi purpose worker (prima
-- 'implement' puntava a 'planner' -> avrebbe usato il modello forte: errato).
UPDATE nexus_subagent_definitions SET model_purpose = 'worker_implement' WHERE kind = 'implement';
UPDATE nexus_subagent_definitions SET model_purpose = 'worker_plan'      WHERE kind = 'plan';
