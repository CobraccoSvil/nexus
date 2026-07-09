-- 0555: Diversita' tier-aware per gli stadi ultracode (implement / verify / review).
--
-- Obiettivo: preferire provider/modelli DIVERSI tra analisi, implementazione e
-- verifica, sempre tramite nexus_purpose_model + catalog (regola G). Nessun
-- hardcode nel codice Rust: i sub-agent legano al purpose via model_purpose.
--
-- Stadi ultracode gia' cablati:
--   Fase A implement -> worker_implement (mig 0203)
--   Fase B review    -> reviewer (mig 0151), con esclusione provider padre in codice
--   verify kind      -> worker_verify (nuovo purpose, tier piu' leggero)
--
-- Il runtime risolve il modello dal tier via select_models_tierchain /
-- resolve_purpose_model_db; la diversita' emerge da tier+capability distinti e
-- dall'esclusione del provider padre sul kind review.

INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES
    ('worker_verify', 'openai', 'gpt-4.1-nano', 'light', 'code', true,
     'Ultracode Fase verify: verifica oggettiva leggera, tier light (mig 0555)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- Allinea il purpose del kind verify al nuovo purpose tier-only.
UPDATE nexus_subagent_definitions
   SET model_purpose = 'worker_verify',
       updated_at = NOW()
 WHERE kind = 'verify'
   AND (model_purpose IS NULL OR model_purpose = '' OR model_purpose = 'worker_implement');

-- Tier reviewer piu' alto di worker_implement per indipendenza avversaria (gia'
-- escluso il provider del padre in resolve_worker_model).
UPDATE nexus_purpose_model
   SET tier = 'high',
       required_capability = 'reasoning',
       requires_tool_use = true,
       notes = 'Ultracode Fase B review: tier high + reasoning, indipendente dal worker (mig 0555)',
       updated_at = NOW()
 WHERE purpose = 'reviewer'
   AND (tier IS NULL OR tier <> 'high');

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.ultracode_tier_diversity_enabled', 'true', 'orchestrator',
   'Abilita la diversita tier-aware tra stadi ultracode (implement/verify/review). I purpose sono in nexus_purpose_model; nessun hardcode runtime.'),
  ('orchestrator.ultracode_implement_purpose', 'worker_implement', 'orchestrator',
   'Purpose model per sub-agent kind implement (Fase A ultracode).'),
  ('orchestrator.ultracode_verify_purpose', 'worker_verify', 'orchestrator',
   'Purpose model per sub-agent kind verify (verifica oggettiva pre-review).'),
  ('orchestrator.ultracode_review_purpose', 'reviewer', 'orchestrator',
   'Purpose model per sub-agent kind review (Fase B ultracode, esclude provider padre).')
ON CONFLICT (key) DO NOTHING;
