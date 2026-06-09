-- 0377_routing_debug_non_thinking_primary.sql
-- Fix routing dell'intent agentico "debug".
--
-- Causa radice: nella routing matrix l'intent debug aveva attivi SOLO modelli
-- thinking (google/gemini-2.5-pro, deepseek/deepseek-v4-pro). Test reali sul
-- progetto Beauty-Book mostrano che questi NON convergono nel tool-loop agentico
-- (33 iterazioni / 249K token senza risolvere; esplorazione e ripetizione di
-- azioni identiche). I modelli non-thinking capaci erano disattivati. Il design
-- previsto (vedi RoutingMatrix::fallback_safe in crates/mcp-core/src/routing_matrix.rs:
-- "intent rischiosi ... mappati a un modello capable") instradava gli intent
-- agentici a un modello non-thinking.
--
-- Vincoli reali: anthropic e' vicino al limite di budget (~17.7/20); mistral ha
-- ampio margine (~9.8/20) e mistral-large-latest e' non-thinking
-- (is_thinking=false, agentic_thinking_policy='none', supports_tool_use=true):
-- candidato che soddisfa convergenza + budget.
--
-- Fix (regola H: corregge la config sbagliata, non aggira; regola G: dato nel DB
-- come unica fonte): attiva mistral-large-latest come modello PRIMARIO per debug
-- in bilanciata/approfondita. priority piu' bassa => vince il load
-- (lookup tiene 1 modello per (intent,mode); ORDER BY priority DESC, l'ultima
-- insert con priority minima sovrascrive). I thinking restano attivi (priority 80)
-- come fallback su cooldown/billing.
-- Idempotente.

UPDATE nexus_routing_matrix
SET is_active = true,
    priority = 50,
    updated_at = NOW(),
    notes = 'Primario non-thinking convergente per intent agentico debug (fix 0377)'
WHERE intent = 'debug'
  AND behavior_mode IN ('bilanciata', 'approfondita')
  AND provider = 'mistral'
  AND model_id = 'mistral-large-latest';
