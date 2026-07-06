-- 0534: allinea la DESCRIZIONE del setting agent.run_cost_budget_usd alla sua
-- semantica definitiva. La mig 0533 lo aveva introdotto come conversione del
-- tetto in dollari in un tetto TOKEN sul modello iniziale del run (approssimazione:
-- non ricalcolata dopo un'escalation). La versione pulita lo usa invece come freno
-- di SPESA diretto: l'executor accumula il costo REALE cumulativo del run (somma dei
-- costi per-turno, ognuno col prezzo del proprio modello -> esatto anche dopo
-- un'escalation cross-tier) e chiude d'autorita' quando supera questo valore.
--
-- Il VALORE (3.0) e la chiave restano invariati: cambia solo la documentazione. Il
-- codice (native_engine::load_executor_config -> ExecutorConfig.run_cost_budget_usd,
-- executor ramo close_runaway "cost_budget_usd") legge la chiave per nome.
UPDATE settings
   SET description = 'Tetto di SPESA in USD dell''intero run agentico. L''executor accumula il costo REALE cumulativo (somma dei costi per-turno, ognuno col prezzo del proprio modello -> esatto anche dopo un''escalation cross-tier) e chiude d''autorita'' quando lo supera. Complementare al budget token per-turno (agent.run_token_budget, TRIGGER del giudice) e all''hard-cap token (backstop). 0 = disabilitato. DB-driven, regola G.'
 WHERE key = 'agent.run_cost_budget_usd';
