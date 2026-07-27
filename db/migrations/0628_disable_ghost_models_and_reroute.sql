-- 0628_disable_ghost_models_and_reroute.sql
--
-- CAUSA RADICE (regola O, oggetto reale — request_diag del gateway sui 400 live):
-- la creazione app falliva perche' il routing serviva NOMI DI MODELLO INESISTENTI
-- ai provider, che rispondono HTTP 400 (invalid_request_error / invalid_argument).
-- Confermati con 400 live: anthropic 'claude-sonnet-4-6', google
-- 'gemini-omni-flash-preview'; noto-fallace: google 'gemini-3.5-flash'. Sono nomi
-- 'fantasma' (plausibili ma inesistenti) entrati enabled nel catalog SENZA probe
-- (vedi la via auto_upgrade_models_and_routing: abilita per FAMIGLIA se il nome
-- matcha l'allowlist '^claude-(opus|sonnet|haiku)-4' e ha un prezzo).
--
-- Questa e' BONIFICA DI DATI CONFERMATI (equivalente a 0556), NON la soluzione:
-- il gate preventivo definitivo (trigger enable-gate + last_probe_healthy_at) e'
-- la migrazione 0629. Necessaria perche' nexus_routing_matrix e
-- nexus_provider_default_model sono letti STATICI, NON joinati a
-- ai_price_catalog.is_enabled (routing_matrix.rs:283/292): disabilitare il catalog
-- da solo NON basta, il routing continuerebbe a puntare al fantasma.
--
-- Applicazione a caldo (regola G): la routing matrix ha cache 60s, nessun redeploy.
-- Idempotente e replay-safe: gli UPDATE colpiscono 0 righe su DB gia' corretto o
-- fresco (catalog popolato a runtime).

-- 1) Disabilita nel catalog i fantasma CONFERMATI con reason PUNITIVA. La reason
--    'invalid_model:...' (stesso vocabolario del probe, mig 0556) li rende immuni
--    ai re-enable postumi: reconcile_enable_returning_to_policy richiede reason
--    NULL o '%policy%'; auto_upgrade_models_and_routing esclude reason valorizzata.
--    Scrittura FORZATA (non COALESCE sulla reason) per sovrascrivere una eventuale
--    reason debole/vuota che li renderebbe di nuovo eleggibili.
UPDATE ai_price_catalog
   SET is_enabled = false,
       auto_disabled_at = COALESCE(auto_disabled_at, NOW()),
       auto_disabled_reason = 'invalid_model:confirmed_bogus',
       updated_at = NOW()
 WHERE (provider = 'anthropic' AND model = 'claude-sonnet-4-6')
    OR (provider = 'google'    AND model IN ('gemini-3.5-flash', 'gemini-omni-flash-preview'));

-- 2) Routing matrix: rimappa i fantasma ai successori LIVE di PARI TIER
--    (sonnet -> sonnet, flash -> flash): nessuna regressione di comportamento,
--    solo il nome corretto a un modello esistente e probe-verificato (enabled,
--    auto_disabled_reason NULL nel catalog).
UPDATE nexus_routing_matrix
   SET model_id = 'claude-sonnet-5', updated_at = NOW()
 WHERE provider = 'anthropic' AND model_id = 'claude-sonnet-4-6';

UPDATE nexus_routing_matrix
   SET model_id = 'gemini-2.5-flash', updated_at = NOW()
 WHERE provider = 'google' AND model_id = 'gemini-3.5-flash';

-- 3) Default per-provider (nexus_provider_default_model): il default anthropic era
--    il fantasma.
UPDATE nexus_provider_default_model
   SET model_id = 'claude-sonnet-5', updated_at = NOW()
 WHERE provider = 'anthropic' AND model_id = 'claude-sonnet-4-6';

UPDATE nexus_provider_default_model
   SET model_id = 'gemini-2.5-flash', updated_at = NOW()
 WHERE provider = 'google' AND model_id = 'gemini-3.5-flash';

-- 4) purpose_model per completezza (oggi nessuna riga usa i fantasma; UPDATE
--    idempotente a 0 righe, difensivo per future regressioni).
UPDATE nexus_purpose_model
   SET model_id = 'claude-sonnet-5', updated_at = NOW()
 WHERE provider = 'anthropic' AND model_id = 'claude-sonnet-4-6';

UPDATE nexus_purpose_model
   SET model_id = 'gemini-2.5-flash', updated_at = NOW()
 WHERE provider = 'google' AND model_id = 'gemini-3.5-flash';
