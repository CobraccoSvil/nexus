-- 0275: flag esplicito is_thinking nel catalog + routing agentico tool-safe.
--
-- Root cause (run reale "crea db + backend", behavior_mode=dinamico):
--   Il routing dinamico dell'agente NON usa nexus_routing_matrix ma
--   route_model_from_catalog(), che seleziona dal catalog il modello featured del
--   tier+capability richiesti con supports_tool_use=TRUE. Per intent=architecture
--   (tier heavy, capability reasoning/code) sceglieva google/gemini-2.5-pro:
--   has supports_tool_use=TRUE (supporta i tool IN GENERALE) ed e' is_featured.
--   Ma gemini-2.5-pro e' un modello THINKING: quando l'agente passa per il planner
--   (che forza tool_choice su nexus_todo_write) thinking + tool-forcing produce
--   finish_reason=MALFORMED_FUNCTION_CALL -> output vuoto -> hollow_completion ->
--   run fallito. Stesso conflitto gia' noto (mig 0266 planner), ma sul percorso
--   dinamico catalog-based che il fix dati sulla matrix (mig 0274) non copre.
--
-- supports_tool_use=TRUE e' corretto (il modello SUPPORTA i tool), ma NON cattura
-- "affidabile col tool-forcing". Serve un attributo distinto.
--
-- Fix strutturale (regole G + H):
--   1. Colonna is_thinking nel catalog (default FALSE).
--   2. Marca i modelli reasoning/thinking che falliscono col tool-forcing:
--      Gemini 2.x thinking, OpenAI o-series, DeepSeek r1/reasoner, Mistral magistral.
--      (Claude NON e' marcato: gestisce tool-use + extended-thinking correttamente.)
--   3. Il codice (orchestrator.rs route_model_from_catalog / best_model_for_tier)
--      esclude is_thinking dal routing AGENTICO (sempre tool-forcing) con
--      degradazione di tier controllata. Vedi commit collegato.
--
-- catalog_sync usa ON CONFLICT DO NOTHING + UPDATE mirati solo su is_enabled/
-- auto_disabled_*: NON tocca is_thinking, quindi il flag sopravvive ai sync.
-- Idempotente.

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS is_thinking BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN ai_price_catalog.is_thinking IS
    'TRUE se il modello e'' reasoning/thinking inaffidabile col tool_choice forzato (MALFORMED_FUNCTION_CALL). Escluso dal routing agentico tool-forcing. Mig 0275.';

-- Reset (idempotenza: ri-deriva sempre dai pattern correnti).
UPDATE ai_price_catalog SET is_thinking = FALSE;

-- Gemini 2.x thinking (MALFORMED_FUNCTION_CALL confermato col tool-forcing).
UPDATE ai_price_catalog SET is_thinking = TRUE
WHERE provider = 'google'
  AND (model LIKE 'gemini-2.5-pro%' OR model LIKE 'gemini-2.5-flash%'
       OR model LIKE 'gemini-2.0-flash-thinking%');

-- OpenAI o-series (reasoning: tool_choice forzato non supportato/instabile).
UPDATE ai_price_catalog SET is_thinking = TRUE
WHERE provider = 'openai'
  AND model ~ '^o[0-9]';

-- DeepSeek reasoning.
UPDATE ai_price_catalog SET is_thinking = TRUE
WHERE provider = 'deepseek'
  AND (model LIKE '%reasoner%' OR model LIKE 'deepseek-r1%');

-- Mistral magistral (reasoning).
UPDATE ai_price_catalog SET is_thinking = TRUE
WHERE provider = 'mistral'
  AND model LIKE 'magistral%';
