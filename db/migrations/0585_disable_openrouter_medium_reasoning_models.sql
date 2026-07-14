-- 0585_disable_openrouter_medium_reasoning_models.sql
--
-- REVERT OPERATIVO della mig 0584. I due modelli medium-reasoning aggiunti
-- (qwen/qwen3-32b, z-ai/glm-4.7-flash) erano stati curati verificando via list-models
-- di OpenRouter che esistessero ed esponessero supported_parameters con 'tools' e
-- 'reasoning'. Verifica INSUFFICIENTE: non e' stata provata l'inference NELLO STACK.
--
-- Evidenza di campo (run consiglio 2026-07-14, Chat 16): le 3 figure instradate su
-- z-ai/glm-4.7-flash sono andate TUTTE in timeout a it=0 (nessuna iterazione completata
-- in 300s) -> parere "n/d", mentre le 2 figure su google/gemini-3.1-pro-preview hanno
-- prodotto pareri reali (completed, it=27 e it=15). Log gateway:
--   "risposta degenere (content vuoto, zero tool-call, finish non terminale)
--    -> failure empty_completion provider=openrouter finish_reason=stop"
-- Il modello e' thinking: mette il ragionamento nel campo `reasoning` e, col prompt reale
-- della figura (8 tool), restituisce content VUOTO e ZERO tool-call con finish=stop.
-- is_degenerate_completion lo classifica (correttamente) come empty_completion -> failover
-- ad ogni turno -> la figura non avanza mai e muore al timeout del sub-agente.
-- In isolamento (1 tool, prompt corto) rispondevano in 1.7-4.2s con tool_calls=1: per
-- questo la curazione basata solo su list-models non li aveva scartati.
--
-- Poiche' il routing cost-first li preferiva (0.06 e 0.08 contro 0.14 di deepseek-v4-flash),
-- venivano scelti PROPRIO per gli intent medium+reasoning (agentic_default, file_ops) e
-- per le figure del consiglio, degradando l'esito invece di migliorarlo.
--
-- Si DISABILITANO (is_enabled=false) invece di eliminarli: la riga resta nel catalog con
-- prezzi/capability corretti, pronta a essere riabilitata se e quando il comportamento
-- thinking di questi modelli sara' gestito end-to-end (campo `reasoning` degli
-- openai-compat) e verificato con una prova di inference nello stack, non solo via
-- list-models. Nessun impatto sugli altri modelli openrouter (gpt-oss-120b/glm-5.2/
-- grok-4.5 della mig 0582 restano abilitati).

UPDATE ai_price_catalog
SET is_enabled = false
WHERE provider = 'openrouter'
  AND model IN ('qwen/qwen3-32b', 'z-ai/glm-4.7-flash');
