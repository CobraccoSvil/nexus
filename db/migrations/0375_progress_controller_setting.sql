-- 0375: interruttore del progress_controller (punto unico di controllo
-- avanzamento del ciclo agentico).
--
-- Causa radice corretta: il loop-control era frammentato in N meccanismi
-- indipendenti (G1, esplorazione, comando ripetuto, signature-loop) che
-- ABORTIVANO il run senza prima costringere all'azione, e instradavano l'abort
-- dritto al learner SCAVALCANDO la verifica E2E del final_gate. Risultato
-- osservato: l'agente esplorava all'infinito e veniva interrotto senza mai
-- agire, oppure chiudeva "fatto" senza che il flusso reale fosse provato.
--
-- Il progress_controller (brain/agents/progress_controller.py) centralizza la
-- reazione allo stallo nella gerarchia coordinata:
--   GUIDE (forza-azione: rimuovi i tool di sola lettura + tool_choice required)
--   -> ESCALATE (modello piu' capace) -> ABORT (solo dopo, e verso la verifica
--   E2E, non al learner morto).
--
-- Flag DB-driven (regola G): default true. OFF ripristina il comportamento
-- legacy (abort immediato) senza redeploy, utile per debug/rollback.
-- Letto da brain/agents/nodes/helpers.py::_load_progress_controller_enabled
-- (cache 60s, get_bool_setting non solleva: default true se DB down).

INSERT INTO settings (key, value, category, description) VALUES
    ('agent.progress_controller_enabled', 'true', 'agent',
     'Se true (default), il ciclo agentico usa il punto unico progress_controller: di fronte a uno stallo (esplorazione/loop) forza prima l''azione (rimuove i tool di sola lettura e obbliga una tool call) e, solo dopo aver esaurito guida ed escalation, abortisce passando per la verifica E2E (final_gate) invece di chiudere il run senza verifica. OFF = comportamento legacy (abort immediato).')
ON CONFLICT (key) DO NOTHING;
