-- 0268_routing_matrix_working_models.sql
--
-- Allinea la routing matrix ai modelli che eseguono davvero le tool call sui
-- task agentici. Diagnosi live (catalog + log produzione):
--   - mistral/mistral-large-3   -> is_enabled=FALSE nel catalog (inutilizzabile)
--   - google/gemini-2.5-pro     -> finish_reason=MALFORMED_FUNCTION_CALL ricorrente
--                                  sui task con tool forcing (output vuoto)
--   - openai o1-2024-12-17 / gpt-4o-mini / gpt-4.1-mini-2025-04-14 -> 429 quota
--   - deepseek/deepseek-coder   -> is_enabled=FALSE nel catalog
--   - anthropic claude-*        -> 400/billing (gia' is_active=false, manual_override)
-- Modelli verificati funzionanti (enabled + supports_tool_use=true):
--   mistral/mistral-large-latest, deepseek/deepseek-v4-pro,
--   deepseek/deepseek-v4-flash, mistral/mistral-large-2512.
--
-- Ambito: SOLO intent agentici (eseguono tool call / producono codice o
-- modifiche): architecture, debug, refactor, fix_complesso, fix_semplice,
-- file_ops, system_admin, test, docs.
-- Intent conversazionali NON toccati: chat_breve, chat_lunga, chat_media.
--
-- La matrix e' cascade: piu' righe per (intent, behavior_mode) ordinate per
-- priority DESC. Vincolo unique: (intent, behavior_mode, provider) ->
-- nexus_routing_matrix_intent_behavior_mode_provider_key.
-- Idempotente: UPDATE mirati + INSERT ... ON CONFLICT (intent,behavior_mode,provider).
--
-- COME RIATTIVARE le righe openai una volta ripristinata la quota:
--   UPDATE nexus_routing_matrix SET is_active = true
--   WHERE provider = 'openai'
--     AND intent IN ('architecture','debug','refactor','fix_complesso',
--                    'fix_semplice','file_ops','system_admin','test','docs')
--     AND notes = '0268: disattivato 429 quota openai';

BEGIN;

-- Lista canonica degli intent agentici usata da tutti gli step.
-- (PostgreSQL non ha variabili in plain SQL: ripetiamo l'array IN(...) per chiarezza.)

-- ----------------------------------------------------------------------------
-- STEP a: garantisci mistral-large-latest (prio 300) e deepseek-v4-pro (prio 290)
--          attivi e in cima, per OGNI (intent agentico, behavior_mode) esistente.
--          Generiamo le coppie (intent, behavior_mode) gia' presenti nella
--          matrix per quell'intent, cosi' non inventiamo behavior_mode inesistenti.
-- ----------------------------------------------------------------------------

-- mistral/mistral-large-latest -> priority 300, attivo, autoritativo
INSERT INTO nexus_routing_matrix (intent, behavior_mode, provider, model_id, priority, is_active, manual_override, notes)
SELECT DISTINCT bm.intent, bm.behavior_mode, 'mistral', 'mistral-large-latest', 300, true, true,
       '0268: tool-robust primario'
FROM (
    SELECT DISTINCT intent, behavior_mode
    FROM nexus_routing_matrix
    WHERE intent IN ('architecture','debug','refactor','fix_complesso',
                     'fix_semplice','file_ops','system_admin','test','docs')
) bm
ON CONFLICT (intent, behavior_mode, provider) DO UPDATE
SET model_id = EXCLUDED.model_id,
    priority = GREATEST(nexus_routing_matrix.priority, EXCLUDED.priority),
    is_active = true,
    manual_override = true,
    notes = EXCLUDED.notes,
    updated_at = now();

-- deepseek/deepseek-v4-pro -> priority 290, attivo, autoritativo
INSERT INTO nexus_routing_matrix (intent, behavior_mode, provider, model_id, priority, is_active, manual_override, notes)
SELECT DISTINCT bm.intent, bm.behavior_mode, 'deepseek', 'deepseek-v4-pro', 290, true, true,
       '0268: tool-robust secondario'
FROM (
    SELECT DISTINCT intent, behavior_mode
    FROM nexus_routing_matrix
    WHERE intent IN ('architecture','debug','refactor','fix_complesso',
                     'fix_semplice','file_ops','system_admin','test','docs')
) bm
ON CONFLICT (intent, behavior_mode, provider) DO UPDATE
SET model_id = EXCLUDED.model_id,
    priority = GREATEST(nexus_routing_matrix.priority, EXCLUDED.priority),
    is_active = true,
    manual_override = true,
    notes = EXCLUDED.notes,
    updated_at = now();

-- ----------------------------------------------------------------------------
-- STEP b: declassa gemini-2.5-pro sugli intent agentici (MALFORMED_FUNCTION_CALL).
--          Resta attivo come ultimo fallback ma con priority molto bassa (50),
--          cosi' viene scelto solo se tutti i tool-robust falliscono.
-- ----------------------------------------------------------------------------
UPDATE nexus_routing_matrix
SET priority = 50,
    is_active = true,
    manual_override = true,
    notes = '0268: declassato (MALFORMED_FUNCTION_CALL su tool forcing)',
    updated_at = now()
WHERE provider = 'google'
  AND model_id = 'gemini-2.5-pro'
  AND intent IN ('architecture','debug','refactor','fix_complesso',
                 'fix_semplice','file_ops','system_admin','test','docs');

-- ----------------------------------------------------------------------------
-- STEP c: rimpiazza i model_id is_enabled=false nel catalog con equivalenti
--          enabled, sugli intent agentici.
--          mistral/mistral-large-3 (disabled) -> mistral-large-latest (enabled).
--          La riga mistral del nuovo modello e' gia' stata creata/aggiornata
--          allo STEP a; qui disattiviamo solo eventuali residui non-mistral.
--          deepseek/deepseek-coder (disabled) -> deepseek-v4-pro (enabled),
--          gia' creato allo STEP a. Disattiviamo la riga deepseek-coder se
--          ancora presente sotto provider deepseek (stesso provider della nuova
--          riga: l'INSERT dello STEP a l'ha gia' sovrascritta a deepseek-v4-pro,
--          quindi deepseek-coder non resta piu' sugli intent agentici).
--          Per sicurezza/idempotenza disattiviamo esplicitamente ogni riga che
--          punta ancora a un modello disabilitato nel catalog sugli agentici.
-- ----------------------------------------------------------------------------
UPDATE nexus_routing_matrix rm
SET is_active = false,
    manual_override = true,
    notes = '0268: model_id is_enabled=false nel catalog',
    updated_at = now()
FROM ai_price_catalog c
WHERE c.provider = rm.provider
  AND c.model = rm.model_id
  AND c.is_enabled = false
  AND rm.intent IN ('architecture','debug','refactor','fix_complesso',
                    'fix_semplice','file_ops','system_admin','test','docs');

-- ----------------------------------------------------------------------------
-- STEP d: disattiva le righe openai con modelli in 429 quota sugli agentici.
--          (Riattivazione documentata nell'header di questa migrazione.)
-- ----------------------------------------------------------------------------
UPDATE nexus_routing_matrix
SET is_active = false,
    manual_override = true,
    notes = '0268: disattivato 429 quota openai',
    updated_at = now()
WHERE provider = 'openai'
  AND model_id IN ('o1-2024-12-17','gpt-4o-mini','gpt-4.1-mini-2025-04-14')
  AND intent IN ('architecture','debug','refactor','fix_complesso',
                 'fix_semplice','file_ops','system_admin','test','docs');

COMMIT;
