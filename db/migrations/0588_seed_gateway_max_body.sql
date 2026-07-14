-- 0588_seed_gateway_max_body.sql
--
-- Seeda `gateway.max_request_body_mb`: il limite del body delle richieste al
-- gateway diventa ESPLICITO e configurabile (regola G), invece di essere il
-- default nascosto di una libreria.
--
-- Perche' nasce: il router del gateway non montava alcun `DefaultBodyLimit`,
-- quindi axum applicava il proprio default di 2 MB. mcp-core — che e' il
-- chiamante — monta invece `DefaultBodyLimit::max(50 MB)` (routes/mod.rs).
-- L'asimmetria faceva rifiutare dal gateway prompt che il chiamante considera
-- legittimi, non appena il contesto agentico superava i 2 MB.
--
-- Verificato sul campo (2026-07-14) contro il gateway vivo:
--   body 1.90 MB -> passa (la richiesta raggiunge i provider)
--   body 2.10 MB -> HTTP 413 "Failed to buffer the request body: length limit exceeded"
--
-- Aggravante: il rifiuto era INVISIBILE. `tower_http::trace` classifica come
-- failure solo i 5xx, quindi un 413 non lascia una riga di log; lato mcp-core
-- arrivava "error sending request for url (http://127.0.0.1:4060/v1/complete)",
-- cioe' un errore di TRASPORTO al posto del segnale strutturato "richiesta
-- troppo grande" (regola M) — il motore non poteva reagire compattando il
-- contesto e il turno falliva senza spiegazione.
--
-- Default 50 MB: allineato a mcp-core. Il gateway non deve accettare meno di chi
-- lo chiama. Alzarlo/abbassarlo qui non richiede redeploy (letto all'avvio del
-- gateway; per applicarlo serve il restart del processo).

INSERT INTO settings (key, value) VALUES
  ('gateway.max_request_body_mb', '50')
ON CONFLICT (key) DO NOTHING;
