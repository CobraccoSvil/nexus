-- 0387_action_oriented_single_source.sql
-- Soglia del punto unico "action-oriented" (regola L + G).
--
-- Causa radice (incidente "riassumi in due righe" 2026-06-10): la domanda
-- "questo turno richiede azione con tool o una risposta testuale?" era decisa
-- da un'euristica keyword (_detect_action_request) valutata sul PRIMO
-- messaggio umano della history, dispersa in 5 call site (tool_choice forcing,
-- G1 anti-descrittivo x2, resoconto onesto, route_after_executor). In una
-- sessione iniziata con una richiesta operativa OGNI turno successivo
-- risultava "azione" per sempre: "riassumi cosa hai sistemato" finiva con
-- tool_choice=required e ri-esecuzione di npm run dev invece della risposta.
--
-- Fix infrastrutturale: router_node calcola UNA volta per turno il campo
-- `action_oriented` dalla semantica del classifier LLM del TURNO CORRENTE
-- (requires_tools OR agentic_score >= soglia); tutti i consumatori leggono
-- via helpers.turn_action_oriented(). Niente piu' euristiche keyword.
--
-- Questo setting governa la soglia sull'agentic_score (0..1 = probabilita'
-- che il task richieda tool use multi-step, stimata dal classifier).
-- Idempotente.

INSERT INTO settings (key, value, category, description)
VALUES (
    'routing.action_oriented_min_agentic_score',
    '0.5',
    'routing',
    'Soglia minima di agentic_score (classifier LLM, 0..1) oltre cui il turno corrente e'' considerato action-oriented dal punto unico turn_action_oriented (tool_choice forcing, G1, resoconto onesto). requires_tools=true del classifier rende comunque il turno action-oriented.'
)
ON CONFLICT (key) DO NOTHING;
