-- 0728_registry_service_tier.sql
--
-- Tier di servizio per endpoint nel registry provider (fase 4, lotto 3).
--
-- Alcuni fornitori OpenAI-compat offrono tier di servizio dichiarati nel body
-- (`service_tier`): groq 'flex' = stesso prezzo, ~10x rate limit, fail-fast
-- 498 `capacity_exceeded` (gia' in tassonomia, mig 0713 -> overloaded ->
-- Transient -> retry/failover di chain). La colonna sta nel REGISTRY e non nel
-- codice (regola G, stesso pattern di usage_accounting mig 0717): il bootstrap
-- la legge, la passa al provider generico e il client OpenAI-compat la applica
-- nel punto unico corpo_della_richiesta (complete e stream insieme). La
-- richiesta che dichiara un proprio service_tier (campo del contratto,
-- LlmRequest.service_tier) VINCE sul default d'endpoint.
--
-- NULL = non emettere: e' il default di tutti e il comportamento di ieri.
--
-- IL FLIP DI GROQ NON E' QUI, ed e' una decisione MISURATA (17/08/2026, probe
-- live sull'API reale): `service_tier:"flex"` su questa org risponde HTTP 400
-- invalid_request_error "flex is not available for this org" — il tier
-- richiede un piano a pagamento. Il flip e' una migrazione dati successiva
-- (UPDATE nexus_provider_registry SET service_tier='flex' WHERE name='groq')
-- da scrivere DOPO l'upgrade del piano, quando un probe la conferma.

ALTER TABLE nexus_provider_registry
    ADD COLUMN IF NOT EXISTS service_tier TEXT NULL;

COMMENT ON COLUMN nexus_provider_registry.service_tier IS
    'service_tier emesso su OGNI richiesta di questo endpoint (dialetto OpenAI-compat, es. groq flex). NULL = non emettere (default di tutti). La richiesta che dichiara un proprio service_tier vince su questo default. Consumato dal solo provider generico openai_compat.';
