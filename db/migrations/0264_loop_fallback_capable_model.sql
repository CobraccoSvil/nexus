-- 0264: il target di escalation per "modello in difficolta'" deve essere un
-- modello CAPACE, non lo stesso flash debole.
--
-- Razionale: nexus_purpose_model.loop_fallback_default era impostato a
-- google/gemini-2.5-flash come workaround temporaneo ("bypass openai cooldown",
-- "anthropic billing $0"). Questo modello e' pero' proprio quello che, sui
-- task agentici thinking, tende a DESCRIVERE le azioni senza eseguirle. Usarlo
-- come fallback di escalation (sia per la loop-detection sia per il nuovo cap
-- G1 narrazione-senza-azione) lascia il sistema sullo stesso modello debole o
-- causa un downgrade gemini-2.5-pro -> gemini-2.5-flash.
--
-- Con anthropic (billing zero) e openai (cooldown) non disponibili, il miglior
-- modello raggiungibile e' google/gemini-2.5-pro: la catena intra-provider
-- nexus_model_escalation_chain promuove gemini-2.5-flash -> gemini-2.5-pro, e
-- il fallback cross-provider deve essere coerente con quel tier.
--
-- Idempotente: UPDATE sulla riga esistente (purpose chiave).
UPDATE nexus_purpose_model
SET provider   = 'google',
    model_id   = 'gemini-2.5-pro',
    notes      = 'escalation target capace per loop-detection e cap G1 narrazione-senza-azione (mig 0264)',
    updated_at = NOW()
WHERE purpose = 'loop_fallback_default';
