-- 0248_intake_request_aware.sql
--
-- M14.4 (intake request-aware). Estende l'intake gate (mig 0226) in modo che,
-- quando l'utente ripete una richiesta gia' presente nella KB, il brain
-- interroghi l'endpoint interno /api/internal/knowledge/request-history e dica
-- se la richiesta era gia' IMPLEMENTATA e verificata, se il CONTESTO e' cambiato
-- dopo, oppure se era rimasta non completata / bloccata dal regression gate.
--
-- Setting di controllo (regola G: unica fonte di verita' nel DB, niente
-- fallback hardcoded nel codice; il default nel codice Python serve solo se il
-- DB e' irraggiungibile).
--
-- confirm_if_implemented=true  -> una richiesta gia' implementata e verificata
--   (contesto invariato) chiede SEMPRE conferma all'utente, anche in modalita'
--   automatic/continuous, prima di rifarla.
-- confirm_if_implemented=false -> il ramo "gia' implementata" NON forza la
--   conferma in auto: degrada al comportamento attuale dell'intake gate.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('kb.intake.confirm_if_implemented', 'true', 'kb',
     'M14.4: se true, una richiesta gia'' implementata e verificata (contesto invariato) chiede conferma anche in modalita'' automatica prima di rifarla.',
     FALSE)
ON CONFLICT (key) DO NOTHING;
