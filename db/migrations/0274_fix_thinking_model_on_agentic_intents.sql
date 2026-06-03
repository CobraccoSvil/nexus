-- 0274: i modelli THINKING non devono essere primi sugli intent agentici
-- (tool-forcing), e i default-provider-model devono puntare a modelli reali.
--
-- Root cause (run reale fallito su "crea db + backend node.js"):
--   1. nexus_routing_matrix: per gli intent agentici architecture/docs/refactor/
--      test il PRIMO modello era google/gemini-2.5-pro (priority 50,
--      manual_override=true). gemini-2.5-pro e' un modello THINKING: sul percorso
--      agentic con tool forzati produce finish_reason=MALFORMED_FUNCTION_CALL ->
--      output vuoto -> hollow_completion. E' lo STESSO conflitto thinking+tool
--      gia' risolto per il planner (mig 0266) e l'executor, ma qui si ripresenta
--      sull'agente principale di questi 4 intent.
--      Log: "google_provider: gemini-2.5-pro output vuoto finish_reason=MALFORMED".
--   2. nexus_provider_default_model: il fallback (quando il modello primario e'
--      hollow) usa il default-provider-model, che puntava a:
--        - deepseek -> deepseek-chat  (is_enabled=false nel catalog: DISABILITATO)
--        - mistral  -> mistral-large-latest (alias che risolve a labs-leanstral-2603,
--          HTTP 403 non abilitato per l'account; gia' diagnosticato in mig 0270 ma
--          quella migrazione corresse solo matrix+purpose, NON questa tabella).
--      Risultato: gemini hollow -> fallback su deepseek-chat (disabilitato) ->
--      di nuovo vuoto -> "Nessuna risposta utile prodotta dall'agente".
--
-- Fix (config corretta, allineata alla realta' dell'account; regole G + H):
--   A. default-provider-model -> modelli realmente abilitati e funzionanti.
--   B. sui 4 intent agentici: promuovi i modelli tool-robust non-thinking
--      (deepseek-v4-pro, mistral-large-2411) davanti, declassa gemini-2.5-pro a
--      riserva di ragionamento (resta attivo come ultimo, NON piu' pinnato).
-- Idempotente.

-- A. Default-provider-model: allinea alla realta' del catalog.
UPDATE nexus_provider_default_model
SET model_id = 'deepseek-v4-pro'
WHERE provider = 'deepseek' AND model_id = 'deepseek-chat';

UPDATE nexus_provider_default_model
SET model_id = 'mistral-large-2411'
WHERE provider = 'mistral' AND model_id = 'mistral-large-latest';

-- B1. Declassa gemini-2.5-pro (thinking) da primo a riserva sugli intent agentici.
--     Niente piu' manual_override: deve poter essere gestito dall'auto-promoter.
UPDATE nexus_routing_matrix
SET priority = 200,
    manual_override = false,
    notes = '0274: declassato da primo - thinking model -> MALFORMED_FUNCTION_CALL sul tool-forcing agentic; resta come riserva ragionamento',
    updated_at = NOW()
WHERE provider = 'google'
  AND model_id = 'gemini-2.5-pro'
  AND priority <= 60
  AND intent IN ('architecture', 'docs', 'refactor', 'test');

-- B2. Promuovi deepseek-v4-pro come primo (tool-robust non-thinking).
UPDATE nexus_routing_matrix
SET priority = 10,
    notes = '0274: promosso primo - tool-robust non-thinking, affidabile sul tool-forcing agentic',
    updated_at = NOW()
WHERE provider = 'deepseek'
  AND model_id = 'deepseek-v4-pro'
  AND is_active = true
  AND intent IN ('architecture', 'docs', 'refactor', 'test');

-- B3. mistral-large-2411 come secondo tool-robust (fallback immediato).
UPDATE nexus_routing_matrix
SET priority = 20,
    notes = '0274: secondo tool-robust non-thinking (fallback di deepseek-v4-pro)',
    updated_at = NOW()
WHERE provider = 'mistral'
  AND model_id = 'mistral-large-2411'
  AND is_active = true
  AND intent IN ('architecture', 'docs', 'refactor', 'test');
