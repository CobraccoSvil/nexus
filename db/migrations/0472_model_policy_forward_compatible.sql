-- 0472_model_policy_forward_compatible.sql
-- Allarga la policy di selezione modelli (nexus_model_selection_policy) da
-- ALLOWLIST-STRETTA a ALLOWLIST-PER-FAMIGLIA forward-compatible + DENYLIST mirata,
-- e riabilita nel catalog i modelli adatti gia' presenti che la vecchia policy
-- (o una blacklist superata) aveva spento.
--
-- CAUSA RADICE (regola H): la policy era una allowlist curata e troppo stretta:
--   google   = {^gemini-2\.5}     -> esclude gemini-2.0 E QUALSIASI gemini-3.x/4 futuro
--   anthropic= {^claude-...-4}    -> esclude claude-3-7 / claude-3-5 (validi, tool-capable)
--   deepseek = {^deepseek-v4,...} -> denylist su chat/coder/r1 (i modelli reali di deepseek)
-- Doppio difetto: (1) pochi modelli abilitati ORA -> catena di escalation povera
-- (1-3 voci); (2) FRAGILE nel tempo -> all'uscita di claude-5 / gemini-4 / gpt-6 il
-- modello NON entra finche' un umano non riscrive il pattern di versione. Era proprio
-- il "rimanere bloccato in configurazione sbagliata" che si voleva evitare.
--
-- FIX strutturale: la policy diventa una allowlist PER FAMIGLIA (^gemini-, ^claude-,
-- ^deepseek-, ^gpt-[4-9]/^o[1-9], ^mistral-(large|medium|small)/...) che cattura
-- automaticamente le versioni future, accoppiata a una DENYLIST robusta che esclude
-- i NON-chat che il flag supports_tool_use NON distingue (su Google/OpenAI bert, veo,
-- imagen, embedding, tts, audio, realtime, transcribe, search, image, deep-research,
-- gemma, robotics, computer-use sono tutti supports_tool_use=TRUE per artefatto del
-- discovery) e i legacy costosi (gpt-4 a 30/60, o1-pro a 150/600, claude-3-opus).
-- Cosi':
--   (1) si abilita "il piu' possibile DEI MODELLI ADATTI" senza far entrare spazzatura;
--   (2) self-updating: un nuovo modello di famiglia nota entra da solo al prossimo sync;
--   (3) punto unico (regola L): catalog_sync (riga ~952/1012) E questa migrazione
--       leggono la STESSA tabella nexus_model_selection_policy -> niente regex duplicate.
--
-- COMPLEMENTO (non sostituito): is_chat_compatible_model (Rust) resta come gate
-- strutturale per tts/whisper/embedding/realtime/instruct/imagen di QUALSIASI famiglia.
-- La denylist qui copre i non-chat DENTRO le famiglie allowed, cosi' UPDATE e sync
-- concordano (nessun flapping enable<->disable).
--
-- NON tocca i modelli spenti per SALUTE/BILLING/DISCOVERY (regola H: non mascherare un
-- fallimento): il re-enable e' filtrato su auto_disabled_reason ammesso (NULL = mai
-- spento, default-off; oppure "%model_selection_policy%" = spento dalla vecchia policy
-- che qui cambia). Esclude billing_cooldown, missing_from_api, error, hollow_completion,
-- tool_probe_failed, manual:non_chat_endpoint, migrazione 0186 (filter retroattivo).
-- Conseguenza onesta: i gemini-3.x oggi "missing_from_api" (l'API key non li espone)
-- NON vengono forzati; rientreranno dal catalog_sync quando l'API li riespone, perche'
-- ora passano la policy.

BEGIN;

-- 1. Policy forward-compatible per famiglia (allowlist) + denylist non-chat/legacy.
UPDATE nexus_model_selection_policy SET
    allowed_patterns = ARRAY['^claude-'],
    denied_patterns  = ARRAY['^claude-2','^claude-instant','^claude-3-opus','^claude-3-sonnet-2024','v1:0']
WHERE provider = 'anthropic';

UPDATE nexus_model_selection_policy SET
    allowed_patterns = ARRAY['^deepseek-'],
    denied_patterns  = ARRAY[]::text[]
WHERE provider = 'deepseek';

UPDATE nexus_model_selection_policy SET
    allowed_patterns = ARRAY['^gemini-'],
    denied_patterns  = ARRAY['embedding','image','imagen','tts','audio','live','gemma','robotics','computer-use','aqa','^gemini-1','nano-banana']
WHERE provider = 'google';

UPDATE nexus_model_selection_policy SET
    allowed_patterns = ARRAY['^gpt-[4-9]','^o[1-9]'],
    denied_patterns  = ARRAY['^gpt-4$','^gpt-4-','^gpt-3','audio','image','realtime','transcribe','-tts','search','deep-research','-instruct','moderation','^davinci','^babbage','whisper','sora','^o1-pro','^o3-pro']
WHERE provider = 'openai';

UPDATE nexus_model_selection_policy SET
    allowed_patterns = ARRAY['^mistral-(large|medium|small)','^magistral','^ministral','^codestral','^devstral','^pixtral','^open-mistral-nemo','^open-codestral'],
    denied_patterns  = ARRAY['^mistral-tiny','^mistral-7b','mixtral','embed','ocr','moderation','voxtral','-tts','vibe','code-agent','code-fim','c21211']
WHERE provider = 'mistral';

-- 2. Re-enable nel catalog dei modelli che ORA passano la policy (derivato dalla
--    tabella appena aggiornata: punto unico, regola L) e che erano spenti SOLO per
--    policy/blacklist (mai per salute/billing/discovery).
UPDATE ai_price_catalog c SET
    is_enabled = TRUE,
    auto_disabled_at = NULL,
    auto_disabled_reason = NULL,
    effective_from = NOW(),
    updated_at = NOW()
FROM nexus_model_selection_policy p
WHERE c.provider = p.provider
  AND c.is_enabled = FALSE
  AND (c.model ~ ANY(p.allowed_patterns) OR cardinality(p.allowed_patterns) = 0)
  AND NOT (c.model ~ ANY(p.denied_patterns))
  AND (c.auto_disabled_reason IS NULL
       OR c.auto_disabled_reason ILIKE '%model_selection_policy%');

COMMIT;
