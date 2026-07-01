-- 0501: cleanup post-rimozione chat-service (commit 1519c1b).
--
-- Il crate crates/chat-service era uno stub abbandonato mai attivato: la chat
-- e' interamente servita da mcp-core (next.config.ts instrada /api/chat/* a
-- :4000). La rimozione del crate ha lasciato due orfani nel DB:
--
--   * settings.chat_service_port (mig 0239): nessun lettore nel codice ->
--     chiave MORTA nell'audit settings (gate ratchet).
--   * entry "chat-service" in agent.watchdog.services (mig 0272): il watchdog
--     avrebbe probato e tentato di riavviare un servizio inesistente.
--
-- La porta 4020 resta riservata nel bucket Nexus (nexus-tool-kit/src/ports.rs):
-- il range HTTP 4000-4079 e' riservato a prescindere dalla singola chiave.
--
-- Idempotente: DELETE + UPDATE condizionato dal predicato @>.

DELETE FROM settings WHERE key = 'chat_service_port';

UPDATE settings
SET value = (
        SELECT COALESCE(jsonb_agg(elem ORDER BY ord), '[]'::jsonb)::text
        FROM jsonb_array_elements(value::jsonb) WITH ORDINALITY AS t(elem, ord)
        WHERE elem->>'name' <> 'chat-service'
    ),
    updated_at = NOW()
WHERE key = 'agent.watchdog.services'
  AND value::jsonb @> '[{"name":"chat-service"}]';
