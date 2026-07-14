-- 0586_reseed_gateway_timeout_settings.sql
-- Ripristina le due settings di timeout HTTP del gateway, tornate FANTASMA
-- (lette dal codice ma assenti dal DB) e rosse nel gate `audit-settings --gate`.
--
-- Storia del difetto:
--   * mig 0421 le seedava (120 / 300): all'epoca le leggeva il brain Python
--     (gateway_provider.py).
--   * mig 0463 le ha ELIMINATE con il brain, motivando "non lette dal client
--     gateway Rust ne' dal gateway. Nessun consumatore." Vero in quel momento.
--   * 2026-07-09, commit 73c7fe46: nasce crates/nexus-gateway/src/http_timeouts.rs
--     che le rilegge (resolve_provider_http_timeout), e mcp-core/src/nexus_gateway.rs
--     fa lo stesso per il budget /v1/complete. Nessuna migrazione le ha riseedate
--     -> da allora sono fantasma e il gate e' rosso.
--
-- Perche' NON basta il fallback nel codice (regola G): senza la riga in DB la
-- chiave non e' configurabile: `nexus_auth::get_setting` ritorna sempre None e i
-- lettori cadono sulle costanti DEFAULT_COMPLETE_TIMEOUT_SECS / _STREAM_. Un
-- admin che volesse alzare il timeout non ha nulla da modificare. Seedandole, il
-- valore torna DB-driven come il modulo gia' dichiara ("seedate in mig 0421").
--
-- I valori sono ESATTAMENTE i default dichiarati dal codice, quindi la
-- migrazione NON cambia il comportamento a runtime: chiude solo il buco di
-- configurabilita' e il gate.
--   - gateway.complete_timeout_seconds -> DEFAULT_COMPLETE_TIMEOUT_SECS = 120
--     (crates/nexus-gateway/src/http_timeouts.rs)
--   - gateway.stream_timeout_seconds   -> DEFAULT_STREAM_TIMEOUT_SECS   = 300
--     (idem; il client HTTP condiviso usa max(complete, stream))
--
-- Idempotente: ON CONFLICT DO NOTHING, cosi' un valore gia' regolato
-- dall'amministratore non viene sovrascritto.

INSERT INTO settings (key, value) VALUES
  ('gateway.complete_timeout_seconds', '120'),
  ('gateway.stream_timeout_seconds', '300')
ON CONFLICT (key) DO NOTHING;
