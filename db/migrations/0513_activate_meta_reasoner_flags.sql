-- 0513: attivazione dei FEATURE FLAG del meta-reasoner (rollout deciso
-- dall'utente: tutti ON).
--
-- I flag erano seminati OFF di default dalle mig 0510/0511/0512 (rollout
-- graduale, comportamento bit-identico). Questa migrazione li porta a ON in modo
-- VERSIONATO e persistente (sopravvive a wipe + re-migrazione), invece di un
-- UPDATE a mano (regola H: dati modificati veicolati da migrazione versionata).
--
-- Config DB-driven (regola G): il cambio e' letto a runtime dai loader con cache
-- 60s, nessun redeploy necessario per un futuro spegnimento (UPDATE ... 'false').
--
-- Effetto:
--   agent.stall_recovery.enabled                -> il meta-reasoner LLM viene
--     consultato quando un detector strutturato segnala uno stallo.
--   agent.orchestration.enabled                 -> il gate del planner chiede a
--     un LLM se fare la plan-phase (Fase 1), con is_eligible come fallback.
--   gateway.redaction.skip_pii_in_user_messages -> le PII scritte volontariamente
--     dall'utente nei propri messaggi non vengono oscurate verso il modello
--     (i segreti restano SEMPRE redatti).

UPDATE settings
   SET value = 'true', updated_at = NOW()
 WHERE key IN (
   'agent.stall_recovery.enabled',
   'agent.orchestration.enabled',
   'gateway.redaction.skip_pii_in_user_messages'
 );
