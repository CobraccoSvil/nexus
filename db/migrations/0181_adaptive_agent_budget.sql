-- Adaptive agent budget: sposta MAX_AGENT_ITERATIONS e affini dalle costanti hardcoded
-- a settings DB, modificabili a runtime. Il brain stima la complessita' del prompt
-- e calcola un budget proporzionato (base + per_complexity_point * score).
--
-- Senza questi setting il sistema usa i fallback Python (60/4/300) per retro-compatibilita'.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.iteration_budget.base', '60', 'agent',
    'Numero base iterazioni LangGraph per ogni run agente. Sommato a per_complexity_point*complexity_score.'),
  ('agent.iteration_budget.per_complexity_point', '4', 'agent',
    'Iterazioni aggiuntive per ogni punto di complessita del prompt (score 0-100).'),
  ('agent.iteration_budget.max', '300', 'agent',
    'Tetto massimo iterazioni anche per task molto complessi. Safety net runaway.'),
  ('agent.complexity.keyword_weights',
    '{"create":3,"write_file":2,"install":2,"build":2,"systemctl":2,"docker":2,"pnpm":2,"npm":1,"deploy":3,"migrate":3,"refactor":4,"fullstack":10,"end-to-end":8,"backend":2,"frontend":2,"database":2,"crea":3,"installa":2,"esegui":2,"avvia":2,"configura":2}',
    'agent',
    'Mappa keyword->punti complessita rilevati nel prompt. Match case-insensitive, somma capped a 100.'),
  ('agent.complexity.step_marker_points', '5', 'agent',
    'Punti per ogni marker di step esplicito (1., 2., step, task, phase) nel prompt.'),
  ('agent.complexity.file_path_points', '2', 'agent',
    'Punti per ogni path o file menzionato nel prompt (es. /home/, src/, *.json).'),
  ('agent.complexity.weak_model_multiplier', '1.5', 'agent',
    'Moltiplicatore budget se il modello iniziale e gpt-4o-mini / haiku / nano (necessita piu iter per G1 nudge).')
ON CONFLICT (key) DO UPDATE SET
  value = EXCLUDED.value,
  description = EXCLUDED.description,
  category = EXCLUDED.category,
  updated_at = NOW();
