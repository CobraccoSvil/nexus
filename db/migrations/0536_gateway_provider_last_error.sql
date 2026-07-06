-- 0536_gateway_provider_last_error.sql
-- Osservabilita' errori provider dal gateway (incidente run a5db0985 del
-- 2026-07-06: failover deepseek->mistral con PROVIDER_ERROR transiente,
-- errore HTTP esatto non ricostruibile: i dev-log venivano troncati al
-- riavvio, il ledger registra solo risposte con usage, e
-- nexus_provider_health.last_error era scritta SOLO dal long-cooldown
-- billing di mcp-core, mai dagli errori transienti del gateway).
--
-- 1. nexus_provider_health: il gateway aggiorna last_error a OGNI errore
--    provider (billing E transiente) con timestamp e sorgente dedicati.
--    billing_cooldown_until resta di proprieta' esclusiva di mcp-core
--    (put_provider_in_long_cooldown, writer unico -- regola L / ADR 0020):
--    il gateway NON la tocca.
-- 2. nexus_provider_health_history: distingue le righe del probe sintetico
--    ('probe', default retro-compatibile) dagli errori osservati su
--    richieste reali ('gateway').
--
-- Writer Rust: CooldownManager::mark_at in crates/nexus-gateway/src/cooldown.rs
-- (punto unico della marcatura cooldown, regola L).
-- Idempotente.

ALTER TABLE nexus_provider_health
    ADD COLUMN IF NOT EXISTS last_error_at timestamptz,
    ADD COLUMN IF NOT EXISTS last_error_source text;

COMMENT ON COLUMN nexus_provider_health.last_error_at IS
'Istante dell''ultimo errore osservato per il provider (billing o transiente). Scritto dal gateway (CooldownManager::mark_at) e da mcp-core (long cooldown billing).';
COMMENT ON COLUMN nexus_provider_health.last_error_source IS
'Chi ha osservato l''ultimo errore: ''gateway'' (errore su richiesta reale) o ''mcp-core'' (long cooldown billing/probe).';

ALTER TABLE nexus_provider_health_history
    ADD COLUMN IF NOT EXISTS source text NOT NULL DEFAULT 'probe';

COMMENT ON COLUMN nexus_provider_health_history.source IS
'Origine della riga: ''probe'' = health probe sintetico (mcp-core), ''gateway'' = errore su richiesta reale (nexus-gateway).';
