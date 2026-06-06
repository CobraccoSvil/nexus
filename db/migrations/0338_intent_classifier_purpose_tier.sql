-- 0338_intent_classifier_purpose_tier.sql
--
-- Il classificatore di intent LLM (brain/router/agentic_classifier.py) risolveva
-- il proprio modello da DUE fonti hardcoded, non dal router:
--   1. nexus_classifier_provider_chain (mig 0134): una chain fissa di provider con
--      mistral-small-latest come PRIMARIO (priority 100). E' il modello che
--      classificava davvero, ed essendo piccolo sbagliava le domande informative
--      ("perche' ci sono due index.html?" -> chat).
--   2. settings.routing.classifier_provider/model: modello fisso (fallback).
-- Entrambe violano la regola G (modello scelto a mano, non dal router) e la
-- regola L (routing duplicato): il sistema unico e' nexus_purpose_model + tier,
-- risolto da resolve_purpose_model (best_model_for_tier, cooldown-aware).
--
-- Fix: il classificatore diventa il purpose 'intent_classifier' con tier='light'
-- e required_capability='reasoning' (come intake_gate / understanding: task di
-- comprensione semantica leggero). Il router seleziona dinamicamente il miglior
-- modello light+reasoning dal catalog, con cooldown/fallback automatici; il
-- (provider, model_id) statico resta come ultimo fallback se il catalog e' vuoto.
--
-- Disattiva inoltre la chain nexus_classifier_provider_chain: il routing del
-- classifier e' ora governato dal solo purpose+tier (punto unico, regola L).
-- Idempotente.

BEGIN;

INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES (
    'intent_classifier', 'google', 'gemini-2.5-flash', 'light', 'reasoning', false,
    'Classificatore intent LLM (mig 0338). Risolto via tier=light+reasoning dal router (nessun modello hardcoded, regola G). Lo statico google/gemini-2.5-flash e'' solo ultimo fallback se il catalog non ha modelli light+reasoning.'
)
ON CONFLICT (purpose) DO UPDATE
SET tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = now();

-- La chain hardcoded (mig 0134) e' sostituita dal purpose+tier: disattivala per
-- non avere due fonti di routing del classifier in conflitto.
UPDATE nexus_classifier_provider_chain
SET is_active = false,
    rationale = 'DISATTIVATA mig 0338: routing classifier ora via purpose intent_classifier + tier (regola L)',
    updated_at = now()
WHERE is_active = true;

COMMIT;
