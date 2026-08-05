-- 0679: sorveglianza delle sospensioni che nessuno sciogliera' (rilievo A4 del
-- processo standard figure, ADR 0043).
--
-- Il gate duale (W3, mig 0677) sospende in HITL anche in Automatic: e' il punto
-- del requisito. Ma in Automatic/Continuous non c'e' nessun umano a raccogliere
-- la sospensione, il run_reaper esclude `awaiting_confirmation` per contratto
-- (mig 0392) e ACTIVE_RUN_STATUSES lo conta fra i run che occupano la sessione:
-- il run notturno restava appeso per sempre, ingorgando la sessione. Al mattino
-- non c'era un esito da leggere.
--
-- Questa chiave e' il TERMINE di quella sospensione. Alla scadenza il run non
-- viene "interrotto": chiude con esito STRUTTURATO `blocked_needs_input` e
-- blocker `safety` derivato dal kind (ADR 0034) — cioe' con la verita' di cosa
-- l'ha fermato, non con un timeout muto.
--
-- PERCHE' UNA CHIAVE DEDICATA e non il solo budget residuo del run. Il piano
-- indicava «budget residuo del run» come scadenza naturale, ed e' il tetto
-- corretto dove esiste (i SUB-RUN un budget ce l'hanno). Ma per il run PRIMARIO
-- `agent.run_time_budget_s` vale '0' — scelta dichiarata dalla mig 0604 e
-- ribadita dalla 0607 — e MISURATA sul DB vivo il 05/08/2026:
--     SELECT value FROM settings WHERE key = 'agent.run_time_budget_s';  -> 0
-- Con quel valore `run_time_remaining_s` ritorna None, quindi una scadenza
-- derivata dal solo residuo sarebbe rimasta INERTE esattamente nel caso per cui
-- nasce: reale nel codice, irraggiungibile nei dati. La chiave dedicata e' la
-- fonte, il residuo del run resta il tetto quando c'e' (punto unico
-- decisions::suspension_watch::classify_suspension).
--
-- Il default 1800 (30 minuti) e' scelto sui due lati del danno: abbastanza
-- lungo perche' chi stia guardando la UI in Automatic possa ancora approvare il
-- passo, abbastanza corto da non consumare una notte di lavoro su una sessione
-- ingorgata. La sweep e' quella del task_watchdog (60s di default), quindi la
-- chiusura avviene entro ~1 minuto dal termine.
--
-- La sospensione HITL ordinaria di Confirm NON e' toccata: li' l'utente e' al
-- terminale, e una scadenza chiuderebbe un run che stava per approvare. Il
-- discriminante e' la modalita' (punto unico decisions::hitl::automation_requires_hitl),
-- non l'origine della sospensione.
--
-- KILL-SWITCH (reversibile a caldo, cache 60s):
--   UPDATE settings SET value = '0'
--    WHERE key = 'orchestrator.suspension_watch_timeout_s';
-- A 0 la sorveglianza e' spenta e si torna al comportamento storico: nessuna
-- nuova scadenza viene scritta. Le scadenze GIA' scritte continuano a maturare
-- (spegnere il flag ferma la produzione, non riapre i run gia' chiusi).

INSERT INTO settings (key, value, description) VALUES
    ('orchestrator.suspension_watch_timeout_s', '1800',
     'Secondi dopo i quali una sospensione HITL che nessuno puo'' sciogliere (modalita'' Automatic/Continuous: nessun umano al terminale) matura e chiude il run con esito strutturato blocked_needs_input + blocker derivato dal kind (safety per il gate duale). 0 = sorveglianza spenta, comportamento storico. Il residuo della deadline di run (agent.run_time_budget_s), quando esiste, resta il tetto. Punto unico: decisions::suspension_watch::classify_suspension (mig 0679, project 0016).')
ON CONFLICT (key) DO NOTHING;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM settings WHERE key = 'orchestrator.suspension_watch_timeout_s'
    ) THEN
        RAISE EXCEPTION 'mig 0679: chiave suspension_watch_timeout_s assente dopo il seed';
    END IF;
END $$;
