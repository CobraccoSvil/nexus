-- 0601_rimozione_billing_service.sql
-- Ritira billing-service dal catalogo infrastruttura: il servizio non esiste piu'.
--
-- PERCHE'. Il crate era un FORK di crates/mcp-core/src/billing.rs: 1.271 righe di
-- cui 812 (64%) duplicate. Non e' stato "abbandonato per svista" — non e' MAI stato
-- messo in produzione. Le prove, misurate e non dedotte da un grep a vuoto:
--
--   * ZERO scritture in 17 giorni, per attribuzione AFFERMATIVA: delle 10.634 righe
--     di ai_usage_ledger, 10.630 portano l'impronta `details` di nexus-gateway e 4
--     quella di mcp-core in-process. Righe non attribuite: 0. Gli status che solo un
--     ciclo reserve/finalize/release produce (reserved/rejected/released) sono a 0.
--   * Il FRONTEND era incompatibile, provato al wire e non leggendo il codice: il
--     web-ide chiama /api/admin/billing/prices e /api/billing/session-usage?session_id=,
--     mentre billing-service esponeva /api/billing/prices e session-usage/:id.
--     Sonda sui due servizi vivi: 404 su :4040, 401 su :4000 (la rotta esiste solo
--     in mcp-core). Non avrebbe potuto servirlo nemmeno puntandocelo.
--   * apps/web-ide/next.config.ts lo dichiarava gia': "/api/billing/* NON vengono
--     routati ai rispettivi microservizi (4050, 4030, 4040) perche' non ancora
--     attivi. Tutte queste route sono gia' implementate in mcp-core (porta 4000)".
--     L'unico riferimento vivo a :4040 nel repo era quel commento.
--   * ai_quota_policies: 0 righe. Nessuna quota e' mai stata configurata.
--
-- PERCHE' QUESTA MIGRAZIONE VIENE PRIMA della cancellazione del crate.
-- La voce di catalogo ha watchdog_managed=true, e services_watchdog.rs tiene lo
-- stato `given_up` in RAM: si perde a ogni riavvio di mcp-core. Cancellare il
-- binario lasciando la riga qui dentro produrrebbe una tempesta di 5 riavvii
-- falliti RIPETUTA a ogni avvio, piu' un pannello "Servizi Nexus" rosso in
-- permanenza. L'ordine non e' preferenza editoriale: e' il fix.
--
-- La PORTA 4040 resta riservata nel bucket 4000-4079 (crates/nexus-tool-kit/src/ports.rs),
-- annotata come "ex", esattamente come 4020 dopo la rimozione di chat-service: cosi'
-- un progetto utente non se la vede assegnare e un domani non collide con lo storico.

-- 1. Fuori dal catalogo infrastruttura (pannello Servizi + watchdog).
--    Filtro su ->>'name': l'identita' della voce e' quella, non la sua posizione.
UPDATE settings
SET value = (
      SELECT COALESCE(jsonb_agg(elem), '[]'::jsonb)::text
      FROM jsonb_array_elements(value::jsonb) AS elem
      WHERE elem->>'name' IS DISTINCT FROM 'billing-service'
    ),
    updated_at = NOW()
WHERE key = 'system.services_catalog'
  AND value::jsonb @> '[{"name": "billing-service"}]'::jsonb;

-- 2. La chiave della porta non ha piu' un lettore (era risolta da
--    nexus_auth::resolve_port in billing-service/src/main.rs).
DELETE FROM settings WHERE key = 'billing_service_port';

-- 3. Rete di sicurezza: se un giorno la voce rientrasse (seed, restore da un dump
--    vecchio), il watchdog non deve rincorrere un binario inesistente.
--    Idempotente e innocuo se la voce non c'e'.
UPDATE settings
SET value = (
      SELECT COALESCE(jsonb_agg(
               CASE WHEN elem->>'name' = 'billing-service'
                    THEN elem || '{"watchdog_managed": false, "panel_shown": false}'::jsonb
                    ELSE elem END
             ), '[]'::jsonb)::text
      FROM jsonb_array_elements(value::jsonb) AS elem
    ),
    updated_at = NOW()
WHERE key = 'system.services_catalog'
  AND value::jsonb @> '[{"name": "billing-service"}]'::jsonb;
