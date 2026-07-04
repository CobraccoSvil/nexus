-- 0520_run_token_budget_limits.sql
-- Due limiti anti-runaway basati sui TOKEN nel motore agentico nativo
-- (crates/nexus-agent-graph, nodo executor). Complementari a
-- `agent.executor.iteration_cap` (mig 0506), che conta ITERAZIONI: qui si conta
-- il COSTO (token cumulativi) e si fa fast-fail sul modello che DESCRIVE senza
-- AGIRE.
--
-- Contesto (incidente): un modello che ignora `force_tool_choice` (pattern
-- gemini) produce turni solo-testo. Questi NON triggerano il signature-loop
-- (che rileva ripetizioni di TOOL, non testo) e non esiste un budget token
-- cumulativo per run: osservato un run a 1.8M token senza convergere. Le due
-- chiavi chiudono entrambi i buchi.
--
-- Regola G: le soglie vivono nel DB, niente fallback hardcoded nel codice. I
-- `Default` dell'ExecutorConfig (400000 / 3) valgono SOLO a DB irraggiungibile;
-- il wiring load_executor_config (native_engine.rs) passa i valori reali.
-- Regola M: i contatori sono alimentati da SEGNALI STRUTTURATI (usage.total_tokens
-- del gateway; presenza/assenza di tool_use nella risposta), mai dal parsing del
-- testo. Cache 60s lato Rust (get_setting).
--
-- Retro-compatibilita': impostando una qualsiasi delle due chiavi a '0' il
-- rispettivo limite e' DISABILITATO -> comportamento bit-identico a prima.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.run_token_budget', '400000', 'agent',
   'Budget TOKEN cumulativo per run del motore agentico (somma input+output di ogni risposta LLM, dal segnale strutturato usage.total_tokens). Al raggiungimento (>=) l''executor chiude deterministicamente il run (forced_close_unverified, mai "completed") PRIMA della prossima chiamata LLM. Safety net anti-runaway per COSTO, complementare a agent.executor.iteration_cap (che conta iterazioni). 0 = disabilitato (bit-identico). Cache 60s (mig 0520).'),
  ('agent.max_consecutive_text_only_turns', '3', 'agent',
   'Numero di turni solo-testo CONSECUTIVI (risposta LLM senza tool_use mentre il loop si aspetta azioni) oltre cui (>=) l''executor chiude deterministicamente il run. Fast-fail sul modello che DESCRIVE senza AGIRE (pattern gemini che ignora force_tool_choice). Il contatore si azzera appena il modello emette un tool_use (segnale strutturato LlmResponse.tool_calls, regola M). 0 = disabilitato (bit-identico). Cache 60s (mig 0520).')
ON CONFLICT (key) DO NOTHING;
