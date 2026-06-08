-- 0365_loop_fallback_capable_tier.sql
--
-- Ripristina l'intento della mig 0264 dentro il modello tier-only della mig 0344.
--
-- `loop_fallback_default` e' il modello di ESCALATION quando un modello debole
-- "narra senza agire" (cap G1 narrazione-senza-azione) o entra in loop. La mig
-- 0264 aveva imposto esplicitamente un target CAPACE (gemini-2.5-pro), perche'
-- escalare verso lo stesso flash/small debole lascia il sistema bloccato. La mig
-- 0344 (tier-only) ha pero' tierizzato questo purpose a 'light' insieme a tutti
-- gli altri leggeri: 'light' = gemini-2.5-flash / mistral-small, cioe' proprio i
-- modelli che narrano a vuoto. Risultato osservato su Beauty-Book: routing intent
-- 'fix' -> mistral-small-latest -> narrazione a vuoto -> "escalation" risolta di
-- nuovo a mistral-small-latest (loop_fallback tier light) -> agente bloccato in
-- attesa, nessun recupero.
--
-- Fix: il fallback di escalation deve puntare a un tier CAPACE. 'heavy' contiene
-- gemini-2.5-pro (supports_tool_use, enabled) ed e' coerente con l'intento
-- esplicito di 0264. Resta tier-only (regola L): la scelta del modello specifico
-- passa al routing (best_model_for_tier), che promuove al miglior modello heavy
-- raggiungibile (gemini-2.5-pro con anthropic/openai non disponibili).
--
-- Idempotente: UPDATE sulla riga del purpose.
UPDATE nexus_purpose_model
SET tier       = 'heavy',
    notes      = 'escalation capace per loop-detection e cap G1 narrazione-senza-azione: tier heavy non light (mig 0365, ripristina intento 0264 sotto tier-only 0344)',
    updated_at = NOW()
WHERE purpose = 'loop_fallback_default';
