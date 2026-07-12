-- 0569_ricerca_web_intent.sql
-- Intent `ricerca_web` (Fase 2 Perplexity): ricerca web citata come flusso
-- NON-agentico verso i modelli Sonar (capability web_search, supports_tool_use
-- =false). Parte DATI dell'intent; il wiring del classifier/dispatch/selezione e'
-- nel codice (intent_classifier.rs, orchestrator, handlers).
--
-- I modelli sonar restano is_enabled=false (opt-in, mig 0568): l'admin li abilita
-- con la api_key. Finche' sono disabilitati o la key manca, l'intent classificato
-- ricerca_web non ha candidati e il flusso ricade sul routing normale.

-- 1) Estende il CHECK di base_capability per ammettere 'web_search' (oggi solo
--    chat/code/reasoning/docs). DO-block robusto: droppa il check qualunque sia il
--    nome, poi ricrea coi 5 valori.
DO $$
DECLARE cname text;
BEGIN
    SELECT conname INTO cname
      FROM pg_constraint
     WHERE conrelid = 'nexus_intent_capability'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) ILIKE '%base_capability%';
    IF cname IS NOT NULL THEN
        EXECUTE format('ALTER TABLE nexus_intent_capability DROP CONSTRAINT %I', cname);
    END IF;
END $$;

ALTER TABLE nexus_intent_capability
    ADD CONSTRAINT nexus_intent_capability_base_capability_check
    CHECK (base_capability IN ('chat', 'code', 'reasoning', 'docs', 'web_search'));

-- 2) Seed dell'intent: dichiara tier + capability richiesta. base_capability=
--    'web_search' -> il selettore filtra sul JSONB capabilities del catalog (dove
--    i sonar hanno web_search=true, mig 0568). preferred_provider=perplexity.
INSERT INTO nexus_intent_capability
    (intent, base_tier, base_capability, preferred_provider, medium_token_threshold, heavy_token_threshold, notes)
VALUES
    ('ricerca_web', 'medium', 'web_search', 'perplexity', NULL, NULL,
     'Ricerca web citata via Perplexity Sonar (flusso non-agentico, supports_tool_use=false).')
ON CONFLICT (intent) DO NOTHING;

-- 3) Routing matrix (FALLBACK): il path primario e' il ramo non-agentico
--    best_non_agentic_model(web_search, pin perplexity); queste righe coprono solo
--    il fallback per behavior_mode. veloce/economica -> sonar (economico),
--    bilanciata -> sonar-pro, approfondita -> sonar-reasoning-pro.
INSERT INTO nexus_routing_matrix
    (intent, behavior_mode, provider, model_id, priority, is_active, manual_override, notes)
VALUES
    ('ricerca_web', 'veloce',      'perplexity', 'sonar',               100, true, true, 'ricerca_web fallback'),
    ('ricerca_web', 'economica',   'perplexity', 'sonar',               100, true, true, 'ricerca_web fallback'),
    ('ricerca_web', 'bilanciata',  'perplexity', 'sonar-pro',           100, true, true, 'ricerca_web fallback'),
    ('ricerca_web', 'approfondita','perplexity', 'sonar-reasoning-pro', 100, true, true, 'ricerca_web fallback')
ON CONFLICT DO NOTHING;
