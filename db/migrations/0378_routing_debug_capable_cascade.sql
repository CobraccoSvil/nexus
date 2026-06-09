-- 0378_routing_debug_capable_cascade.sql
-- Completa il fix del routing per l'intent agentico debug (segue 0377).
--
-- Diagnosi dai test reali: i modelli thinking (gemini-2.5-pro, deepseek-v4-pro)
-- non convergono nel tool-loop; mistral-small-latest (fallback di
-- route_model_with_mode su matrix.default_model quando i primari sono in cooldown)
-- e' troppo debole. Per i task agentici complessi serve un modello CAPACE e
-- non-thinking come primario.
--
-- Il vincolo UNIQUE (intent, behavior_mode, provider) impone UN modello per
-- provider per (intent,mode): la riga anthropic per debug esiste gia' (era
-- claude-opus-4-8, inattiva). La portiamo a claude-sonnet-4-6 ATTIVA priority 40
-- (primario, vince il load: priority piu' bassa). Cascata risultante:
--   anthropic/claude-sonnet-4-6 (40) primario [capace, non-thinking]
--   mistral/mistral-large-latest (50) [non-thinking]  (vedi 0377)
--   gemini-2.5-pro / deepseek-v4-pro (80) ultima risorsa [thinking]
--
-- NB operativo: anthropic e openai sono attualmente in cooldown billing
-- (credit_balance_too_low). Finche' il credito non e' ripristinato, il routing
-- ripiega su mistral-large (0377). Questo fix rende claude-sonnet il primario
-- automaticamente non appena anthropic torna disponibile.
-- Regola H/G. Idempotente.

UPDATE nexus_routing_matrix
SET model_id = 'claude-sonnet-4-6',
    is_active = true,
    priority = 40,
    updated_at = NOW(),
    notes = 'Primario capace non-thinking per debug (fix 0378)'
WHERE intent = 'debug'
  AND behavior_mode IN ('bilanciata', 'approfondita')
  AND provider = 'anthropic';
