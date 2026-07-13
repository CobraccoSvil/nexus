-- 0582_new_providers_reasoning_capability.sql
--
-- CAUSA RADICE (diagnosi 2026-07-13): groq/openrouter non venivano MAI selezionati
-- dal routing agentico. Il ramo vivo (settings.nexus_behavior_mode='dinamico') per
-- gli intent con base_capability='reasoning' (nexus_intent_capability mig 0358:
-- agentic_default/file_ops=medium, architecture/debug/fix_complesso/system_admin=heavy)
-- filtra i candidati con `capabilities @> ["reasoning"]` (select_models_tierchain,
-- model_selection.rs). I modelli reasoning-capable di groq/openrouter erano stati
-- seedati (mig 0566/0567) con capabilities INCOMPLETE ["chat","code"], SENZA
-- "reasoning" -> esclusi dal filtro, per questo non entravano mai nella selezione.
--
-- FIX (regola G: il catalog deve riflettere le capability REALI dei modelli). I
-- GPT-OSS (OpenAI open-weight) e glm-5.2 / grok-4.5 SONO reasoning-capable: si aggiunge
-- il tag "reasoning" solo a questi (NON ai llama-3.x, che restano chat/code puri).
--
-- EFFETTO ATTESO: negli intent a tier heavy che degradano al tier high (la tier-chain
-- scende: heavy->high->medium; accade tipicamente quando i modelli heavy sono in
-- cooldown, es. google 429 RESOURCE_EXHAUSTED), gpt-oss-120b (0.15) e glm-5.2 (0.42)
-- diventano candidati e vincono per costo (AGENTIC_COST_FIRST_ORDER = input_cost ASC).
--
-- LIMITE NOTO (non risolto qui): gli intent a tier MEDIUM reasoning (agentic_default,
-- file_ops) restano scoperti: groq/openrouter non hanno un modello MEDIUM reasoning
-- (llama-3.3-70b e' medium ma chat/code). Per coprirli serve aggiungere un modello
-- medium+reasoning ai nuovi provider (decisione di catalog separata).
--
-- Idempotente: aggiunge "reasoning" solo dove non e' gia' presente. capability_source
-- resta 'manual' cosi' il catalog_sync non lo sovrascrive.

UPDATE ai_price_catalog
SET capabilities = capabilities || '["reasoning"]'::jsonb,
    capability_source = 'manual'
WHERE provider IN ('groq', 'openrouter')
  AND model IN (
        'openai/gpt-oss-120b',
        'openai/gpt-oss-20b',
        'z-ai/glm-5.2',
        'x-ai/grok-4.5'
      )
  AND NOT (capabilities @> '["reasoning"]'::jsonb);
