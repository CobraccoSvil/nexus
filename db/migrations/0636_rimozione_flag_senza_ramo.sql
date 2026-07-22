-- 0636_rimozione_flag_senza_ramo.sql
--
-- CAUSA: dieci chiavi in `settings` non hanno un solo lettore nel codice. Non
-- sono spente: due gruppi su tre risultano ACCESI, quindi il DB dichiara attive
-- funzioni che non esistono. Chi apre i settings vede un pannello di verifica
-- avversariale configurato (dimensione 3, consenso 2, tre lenti) e un
-- worker-mode abilitato, e non ha modo di sapere che nessuno dei due esiste.
--
-- MISURA: `grep` di ognuna delle dieci chiavi su crates/ e apps/ -> 0 occorrenze.
-- I punti in cui dovrebbero agire sono commenti: verifier.rs ("rami esplorativo
-- e panel: OFF + NON portati... NIENTE LLM qui") ed executor.rs ("worker-mode:
-- TODO PR-J, NON portarli ora"). Nessun campo nelle rispettive Config, nessuna
-- lettura in `load_verifier_config`.
--
-- Storia di come sono arrivate ad "acceso":
--   - `verify_panel_*` nasce gia' a 'true' dalla mig 0439, insieme ai suoi
--     parametri. La mig 0564 lo ESCLUDE dall'accensione di massa motivando
--     "gia' seedato 'true' dalla 0439": una feature inesistente e' stata quindi
--     confermata attiva due volte.
--   - `exploratory_verify_enabled` e `worker_mode_enabled` nascono a 'false'
--     (mig 0208 e 0205) e sono stati portati a 'true' dalla mig 0564, che
--     accendeva in blocco i feature flag dell'orchestratore.
--
-- Le chiavi si rimuovono invece di spegnerle: un flag spento resta un flag, e
-- prima o poi qualcuno lo riaccende aspettandosi un effetto. Il codice
-- corrispondente non esiste, quindi la configurazione non deve esistere
-- (regola: un flag o ha un ramo che lo legge, o non e' un flag).
--
-- REVERSIBILE: reintrodurre queste chiavi e' una `INSERT` di poche righe, e
-- andra' fatta INSIEME al codice che le legge — non prima. I valori storici
-- restano leggibili in 0205, 0208 e 0439.

-- Ramo esplorativo del verifier: check LLM RAG-informed dopo i criteri
-- deterministici. Mai portato: servirebbero un LlmGateway nel VerifierNode (che
-- per contratto e' deterministico) e una porta RAG che non esiste.
DELETE FROM settings WHERE key IN (
    'orchestrator.exploratory_verify_enabled',
    'orchestrator.exploratory_verify_max_cycles',
    'orchestrator.exploratory_verify_min_score',
    'orchestrator.exploratory_verify_topk'
);

-- Panel di verifica avversariale (K verificatori con lenti diverse, voto a
-- consenso). Oltre a non esistere, si sovrappone per funzione al panel
-- avversariale gia' vivo in `decisions/adversarial_review`: se un giorno
-- servisse, la domanda giusta e' se non sia un doppione (regola L), non come
-- implementarlo da zero.
DELETE FROM settings WHERE key IN (
    'orchestrator.verify_panel_enabled',
    'orchestrator.verify_panel_size',
    'orchestrator.verify_panel_consensus',
    'orchestrator.verify_panel_lenses'
);

-- Worker-mode: l'executor del run principale userebbe un prompt da
-- orchestratore e un set ridotto di tool, delegando ai worker. La delega ai
-- sub-agenti pero' passa gia' da un'altra strada viva (`dispatch_subagent` +
-- todo_runner): accenderlo significherebbe avere DUE modi di delegare. Se
-- tornera', sara' come consolidamento dei due, non come feature nuova.
DELETE FROM settings WHERE key IN (
    'orchestrator.worker_mode_enabled',
    'orchestrator.worker_mode_tool_whitelist'
);

-- NB: `orchestrator.dag_parallel_enabled` NON viene rimossa. Il suo ramo esiste
-- davvero (`route_after_planner`), ma il campo non e' popolato dal DB di
-- proposito: abilitarlo senza il dispatch DAG nell'executor lascerebbe i todo
-- orfani. Resta come chiave inerte documentata, e il toggle che la esponeva
-- nell'admin e' stato rimosso nello stesso commit: prometteva un parallelismo
-- che non poteva attivare.
