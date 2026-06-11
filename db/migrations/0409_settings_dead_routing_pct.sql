-- 0409: cleanup setting orfana del rollout A/B del router neurale.
--
-- La costante SETTINGS_KEY_ACTIVE_ROUTING_PCT e le funzioni
-- read_nexus_active_routing_pct / should_override_ab (nexus_routing.rs) sono
-- state rimosse dalla bonifica dead code 2026-06-11: erano il meccanismo di
-- rollout percentuale A/B del routing neurale, mai piu' invocato da quando il
-- routing e' passato alla matrice DB-driven (regola G). La chiave seminata
-- dalla mig 0061 resta senza lettori: via (stessa regola della mig 0406).
--
-- Idempotente.

DELETE FROM settings WHERE key = 'nexus_active_routing_pct';

-- Stessa sorte per le soglie dell'interpretazione DETERMINISTICA dell'intent
-- (keyword/euristiche): il refactoring 5e142e8 ha reso l'intent solo
-- semantico (LLM) e la bonifica dead code ha rimosso gli ultimi lettori.
DELETE FROM settings WHERE key IN (
    'routing.intent_deterministic_high',
    'routing.intent_deterministic_min',
    'routing.token_threshold_long_context'
);
