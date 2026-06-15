-- 0426_plan_intents_extend_debug_fix.sql
-- Estende la lista degli intent per cui il planner Plan/Act/Verify e' eligibile.
--
-- Problema: i task con intent 'debug' (e potenzialmente 'fix_semplice' /
-- 'fix_complesso' qualora vengano introdotti come refinement futuri) non
-- attivavano il planner_node anche se il task era complesso. Causa: il guard
-- interno `orchestrator_config.is_eligible` (brain/agents/planner_node.py:69)
-- richiede `intent in plan_intents`, e la CSV salvata in settings era
-- 'code,implement,fix,refactor,scaffold_app,architecture' (mancava 'debug').
-- Il gate adattivo (route_after_router in graph.py) ignora gia' la lista, ma
-- il secondo guardiano del nodo la riapplica, di fatto bloccando il planner
-- sui task di debug. Risultato osservato: l'agente improvvisa senza
-- decomporre.
--
-- Fix DB-driven (regola G + regola L): non si tocca il codice, si estende
-- l'unica fonte di verita' (settings.plan_intents) appendendo i token mancanti
-- solo se NON gia' presenti. La modifica e' reversibile dall'admin via UI
-- (override sullo stesso setting). I gate HARD restano in vigore:
--   - plan_phase_enabled=true (gia' on)
--   - plan_min_token_budget=1500 (gia' on) — task piccoli non passano il gate
--   - behavior_mode coerente con plan_behavior_modes
--
-- Sui token nuovi: 'debug' e' un intent canonico del classifier
-- (brain/router/intents.py::ALLOWED_INTENTS). 'fix_semplice' / 'fix_complesso'
-- NON sono attualmente emessi dal classifier Python; vengono aggiunti come
-- hook anticipato (zero rischio: matchano solo se in futuro il classifier o
-- un rewriter li produrra'; vedi anche routing_matrix lato Rust che gia' usa
-- queste varianti). Idempotente.

-- Helper inline: append CSV token se assente. La colonna `settings.value` e'
-- text (CSV separata da virgole, parsata da _coerce in orchestrator_config.py).
-- Strategia: rebuild della lista come union ordinata, evitando duplicati. La
-- riga viene aggiornata solo se la lista finale differisce dalla corrente
-- (idempotente: re-eseguire la migrazione non modifica piu' nulla).

WITH current_row AS (
    SELECT value AS current_value
    FROM settings
    WHERE key = 'orchestrator.plan_intents'
), tokens_attesi AS (
    SELECT unnest(ARRAY[
        'code','implement','fix','refactor','scaffold_app','architecture',
        'debug','fix_semplice','fix_complesso'
    ]) AS token
), tokens_correnti AS (
    SELECT trim(unnest(string_to_array(coalesce((SELECT current_value FROM current_row), ''), ','))) AS token
), tokens_uniti AS (
    SELECT DISTINCT token FROM (
        SELECT token FROM tokens_correnti WHERE token <> ''
        UNION ALL
        SELECT token FROM tokens_attesi
    ) AS t
), nuova_lista AS (
    SELECT string_agg(token, ',' ORDER BY token) AS nuovo_value FROM tokens_uniti
)
UPDATE settings s
SET value = nl.nuovo_value,
    updated_at = now()
FROM nuova_lista nl, current_row cr
WHERE s.key = 'orchestrator.plan_intents'
  AND nl.nuovo_value IS NOT NULL
  AND nl.nuovo_value <> cr.current_value;

-- Insert difensivo: se la chiave non esiste (deploy nuovo / DB pulito), la crea
-- con il superset esteso. Mantiene la categoria 'orchestrator' (coerente con
-- _KEY_PREFIX in orchestrator_config.py).
INSERT INTO settings (key, value, category, description)
VALUES (
    'orchestrator.plan_intents',
    'architecture,code,debug,fix,fix_complesso,fix_semplice,implement,refactor,scaffold_app',
    'orchestrator',
    'Intent classifier per cui il planner Plan/Act/Verify e attivo. CSV. Override admin.'
)
ON CONFLICT (key) DO NOTHING;
