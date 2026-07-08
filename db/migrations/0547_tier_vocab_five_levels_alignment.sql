-- 0547_tier_vocab_five_levels_alignment.sql
-- Allineamento del VOCABOLARIO tier a 5 livelli (light < medium < high < heavy <
-- frontier) sui siti rimasti a 3 livelli dopo la mig 0528.
--
-- Causa radice (regola H + regola L): la 0528 ha esteso la scala di capacita' dei
-- modelli da 3 a 5 livelli SOLO su ai_price_catalog.performance_tier (+ vista
-- v_model_escalation_chain) e sul codice di escalation (fase 1, solo-codice). Il
-- vocabolario tier NON ha un punto unico: e' replicato come CHECK constraint su
-- piu' tabelle, come validatore Rust e nei prompt. Sono cosi' rimaste a 3 livelli
-- tre CHECK su tabelle VIVE (nexus_purpose_model, nexus_intent_capability,
-- nexus_routing_slots_matrix), una quarta *_tier viva SENZA guardia
-- (nexus_intent_routing_requirements), il validatore del pavimento agentico e il
-- validatore admin (model_routing.rs / admin/routing.rs, corretti nello stesso PR)
-- e il prompt dello scale-controller. Debito gia' annotato in 0546. Questa
-- migrazione lo chiude dove il tier E' il performance_tier del catalog; NON tocca i
-- concetti DIVERSI (speed_tier fast/medium/slow, complexity low/medium/high,
-- ContextPressure low/medium/high).
--
-- Tabelle obsolete NON toccate (verificato a mano: to_regclass IS NULL):
--   - nexus_model_escalation_chain.capability_tier (mig 0128, DROPpata dalla 0474)
--   - model_catalog.performance_tier               (mig 0032, sostituita da
--                                                    ai_price_catalog, gia' a 5 in 0528)
--
-- Estendere un CHECK a un SUPERSET non invalida alcuna riga esistente (i valori
-- light/medium/heavy correnti restano validi). Idempotente: DROP ... IF EXISTS +
-- ADD; replace() (non-regex) e' idempotente (la forma a 5 livelli non contiene la
-- sottostringa a 3). Nessun nome modello hardcoded (regola G): qui si tocca solo
-- il VOCABOLARIO dei tier, non le assegnazioni tier->modello.

-- (1) nexus_purpose_model.tier (mig 0203) — la tabella dell'affermazione.
ALTER TABLE nexus_purpose_model DROP CONSTRAINT IF EXISTS nexus_purpose_model_tier_check;
ALTER TABLE nexus_purpose_model
    ADD CONSTRAINT nexus_purpose_model_tier_check
    CHECK (tier IS NULL OR tier IN ('light', 'medium', 'high', 'heavy', 'frontier'));

COMMENT ON COLUMN nexus_purpose_model.tier IS
'Se valorizzato (light|medium|high|heavy|frontier, scala a 5 livelli mig 0528/0547), il modello e'' risolto dinamicamente dal catalog per quel tier via best_model_for_tier; provider/model_id diventano l''ultimo fallback.';

-- (2) nexus_intent_capability.base_tier (mig 0110). NOT NULL invariato (constraint
--     separato dal CHECK).
ALTER TABLE nexus_intent_capability DROP CONSTRAINT IF EXISTS nexus_intent_capability_base_tier_check;
ALTER TABLE nexus_intent_capability
    ADD CONSTRAINT nexus_intent_capability_base_tier_check
    CHECK (base_tier IN ('light', 'medium', 'high', 'heavy', 'frontier'));

-- (3) nexus_routing_slots_matrix.preferred_tier (mig 0357, constraint NAMED).
ALTER TABLE nexus_routing_slots_matrix DROP CONSTRAINT IF EXISTS slots_preferred_tier_valid;
ALTER TABLE nexus_routing_slots_matrix
    ADD CONSTRAINT slots_preferred_tier_valid
    CHECK (preferred_tier IN ('light', 'medium', 'high', 'heavy', 'frontier'));

-- (4) Prompt dello scale-controller (mig 0516, template system.scale.assess): la
--     scala che l'LLM puo' proporre deve elencare i 5 livelli, altrimenti il
--     controller non potrebbe MAI scegliere high/frontier benche' l'enum ScaleTier
--     li supporti gia' (nexus-agent-graph::decisions::scale_reason). Due token
--     sostituiti (<role> e <output_format>); il concetto DIVERSO context_pressure
--     (low/medium/high) non e' toccato.
UPDATE nexus_prompt_templates
   SET content = replace(
                     replace(content, 'light/medium/heavy', 'light/medium/high/heavy/frontier'),
                     '{light,medium,heavy}', '{light,medium,high,heavy,frontier}'
                 ),
       updated_at = NOW()
 WHERE key = 'system.scale.assess'
   AND (content LIKE '%light/medium/heavy%' OR content LIKE '%{light,medium,heavy}%');

-- (4b) Notes del purpose scale_assess (cosmetico, stesso vocabolario).
UPDATE nexus_purpose_model
   SET notes = replace(notes, 'light/medium/heavy', 'light/medium/high/heavy/frontier'),
       updated_at = NOW()
 WHERE purpose = 'scale_assess' AND notes LIKE '%light/medium/heavy%';

-- (5) Descrizioni dei setting tier (cosmetico: allineano il vocabolario mostrato in
--     admin; i valori restano DB-driven, regola G). Il codice consumatore
--     (model_upscale_port target_tier, model_routing agentic_min_tier) gia' fa
--     match esatto su performance_tier, quindi accetta i 5 livelli senza modifiche
--     oltre al validatore del pavimento (corretto nel codice Rust, stesso PR).
UPDATE settings
   SET description = replace(description, 'light|medium|heavy', 'light|medium|high|heavy|frontier'),
       updated_at = NOW()
 WHERE key IN ('agent.upscale.target_tier', 'agent.routing.agentic_min_tier')
   AND description LIKE '%light|medium|heavy%';

-- (6) nexus_intent_routing_requirements.preferred_tier (mig 0174): STESSO concetto
--     di capacita' (preferred_tier letto da routing_matrix_auto_promoter.rs). La
--     colonna nasce SENZA CHECK (TEXT NOT NULL DEFAULT 'medium'), quindi non c'e' un
--     vincolo a 3 livelli da estendere, ma resta l'unica *_tier viva senza guardia
--     sul vocabolario: la si porta a parita' con le altre (verificato a mano: i
--     valori esistenti sono solo light/medium/heavy, tutti nel nuovo insieme).
--     Idempotente: DROP ... IF EXISTS + ADD.
ALTER TABLE nexus_intent_routing_requirements DROP CONSTRAINT IF EXISTS intent_routing_preferred_tier_valid;
ALTER TABLE nexus_intent_routing_requirements
    ADD CONSTRAINT intent_routing_preferred_tier_valid
    CHECK (preferred_tier IN ('light', 'medium', 'high', 'heavy', 'frontier'));
