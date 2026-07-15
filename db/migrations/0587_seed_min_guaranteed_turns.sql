-- 0587_seed_min_guaranteed_turns.sql
--
-- Seeda `agent.llm.min_guaranteed_turns`, la grandezza primaria da cui il punto
-- unico `nexus_auth::llm_timeouts` DERIVA tutti i timeout delle chiamate LLM.
--
-- Perche' nasce: la gerarchia dei timeout era INVERTITA. Il budget di UNA
-- chiamata (300s: max(complete 120, stream 300) sul client condiviso, senza
-- alcun timeout logico nel gateway) era UGUALE al budget dell'INTERO run
-- multi-turno che la conteneva (orchestrator.subagent_default_timeout_s = 300),
-- e il client mcp-core->gateway attendeva persino di piu' (435s = 120*3+45+30).
-- Conseguenza deterministica: una singola chiamata appesa consumava il 100%
-- della vita del run, che moriva per RunTimeout con ZERO iterazioni completate
-- (it=0). Misurato sul campo: buco di 197s in cui ne' mcp-core ne' il gateway
-- registravano attivita', con le figure del consiglio ferme tutte insieme su
-- provider diversi (deepseek E google) -> non era il provider, ed era gia'
-- presente prima del commit a cui era stato attribuito (stesso binario: 2/5
-- figure in timeout alle 10:07Z, 5/5 alle 10:26Z).
--
-- Il valore risponde a: "quanti turni un run deve poter completare anche nel
-- caso PEGGIORE, in cui ogni chiamata esaurisce il proprio budget?".
--   request_budget = subagent_default_timeout_s / min_guaranteed_turns
--   4 turni su 300s -> 75s per chiamata (deadline end-to-end, retry inclusi).
-- Riferimento empirico: una figura che produce un parere valido chiude in ~7
-- iterazioni a ~33s l'una, quindi 75s e' oltre il doppio della media osservata.
--
-- Alzare questo numero STRINGE il budget per chiamata (piu' turni garantiti,
-- meno tempo ciascuno); abbassarlo lo allarga. L'invariante
-- `request_budget * min_guaranteed_turns <= run_timeout` e' garantito per
-- costruzione dalla derivazione e coperto da unit test: nessun valore qui puo'
-- violarlo. Pavimento: 2 turni.

INSERT INTO settings (key, value) VALUES
  ('agent.llm.min_guaranteed_turns', '4')
ON CONFLICT (key) DO NOTHING;
