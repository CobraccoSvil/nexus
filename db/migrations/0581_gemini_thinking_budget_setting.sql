-- 0581_gemini_thinking_budget_setting.sql
-- (rinumerata da 0579 -> 0581 per collisione con 0579_observer_boot_grace di una
--  sessione concorrente, gia' applicata come version 579)
--
-- Tuning latenza (segue 0578): il budget di thinking per i modelli a thinking
-- OBBLIGATORIO (gemini-3, policy 'native') era ricavato da default_max_output_tokens
-- (8192, che e' il budget di OUTPUT, concetto diverso): troppo alto come THINKING
-- budget -> gemini-3-pro spende troppo tempo nel reasoning e sfonda il timeout
-- sub-agente (300s), producendo n/d da TIMEOUT (non piu' da empty).
--
-- Setting dedicato, DB-driven (regola G): l'adapter mcp-core lo legge per dimensionare
-- il thinkingBudget bounded. Valore piu' piccolo = gemini-3 ragiona ABBASTANZA da non
-- andare vuoto (il budget resta > 0, bounded) ma piu' VELOCE. Tunabile senza redeploy
-- (cache 60s lato Rust). Clampato a [2048, 24576] nel resolver (capability.rs).

INSERT INTO settings (key, value, category)
VALUES ('orchestrator.gemini_thinking_budget', '4096', 'orchestrator')
ON CONFLICT (key) DO NOTHING;
