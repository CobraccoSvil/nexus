-- 0364_intent_classifier_no_reasoning.sql
-- Causa radice del timeout del classifier ad OGNI messaggio (incidente 2026-06-08).
--
-- Il purpose 'intent_classifier' (mig 0338) era tier='light' + required_capability=
-- 'reasoning'. La risoluzione tier-based (orchestrator::best_model_for_tier, caso
-- non-agentico) filtra i candidati con `capabilities @> '["reasoning"]'`: tra i
-- modelli performance_tier='light' SOLO i magistral-* dichiarano "reasoning" (sono
-- la linea di RAGIONAMENTO di Mistral), mentre i veloci (mistral-small-latest,
-- gemini-2.5-flash, gpt-4.1-nano) NON ce l'hanno. Effetto: il filtro escludeva
-- tutti i modelli veloci e forzava magistral-small-2509 (reasoning-only, lento
-- perche' genera reasoning tokens) -> TIMEOUT del classifier -> degrado a fallback
-- keyword a OGNI richiesta utente, con pop-up di errore lato UI.
--
-- Il classifier e' LATENCY-CRITICAL (gira su ogni messaggio, timeout stretto): a
-- differenza di intake_gate/understanding NON deve "ragionare", deve solo emettere
-- un JSON di classificazione in fretta. Rimosso il requisito 'reasoning',
-- best_model_for_tier('light') ordina per (is_featured DESC, costo ASC) e sceglie
-- mistral-small-latest (featured, $0.06, capabilities [code,chat,fix], niente
-- reasoning tokens) -> veloce, niente timeout. gemini-2.5-flash resta candidato.
--
-- NB ANTI-TOPPA (regola H): NON si modifica ai_price_catalog.performance_tier.
-- magistral-small e' legittimamente 'light' per costo/dimensione; spostarlo a mano
-- sarebbe una TOPPA perche' model_catalog_sync::infer_tier_from_name lo re-inferisce
-- dal nome a ogni sync (lo sovrascriverebbe). Il fix definitivo e' sul purpose
-- latency-critical (qui), non sul catalog. ui_hint_classifier non e' toccato:
-- gia' senza required_capability.
-- Idempotente.

UPDATE nexus_purpose_model
SET required_capability = NULL,
    updated_at = NOW()
WHERE purpose = 'intent_classifier';
