-- 0635_verifier_todo_criteria_mode.sql
--
-- CAUSA: il VerifierNode non ha MAI eseguito un acceptance_criterion. Non per un
-- flag spento — per un buco nel trasporto: il tipo `Todo` del grafo non portava
-- il campo, quindi il verifier lo cercava nella ri-serializzazione di quel tipo
-- e trovava sempre assente, prendendo il ramo "nessun criterion" a ogni giro.
--
-- PROVA (misurata come la produzione, non stimata): sul cluster app, 104 todo su
-- 104 hanno almeno un criterio, per 163 criteri totali; `nexus_agent_verifier_runs`
-- e' a ZERO righe in tutti i progetti. `runs.record` e' chiamato solo dentro il
-- ramo criteri-non-vuoti: verifier acceso, criteri presenti, zero record.
--
-- Il trasporto e' ora chiuso nel codice. Questa migrazione decide cosa farne.
--
-- PERCHE' NON SI ACCENDE SUBITO L'ENFORCEMENT. Con i criteri che finalmente
-- arrivano, l'esito cambierebbe per ogni run pianificato, e in gran parte per
-- ragioni di forma:
--   - 59 todo su 104 (57%) hanno almeno un criterio che fallirebbe per
--     vocabolario, non per merito: `regex_in_output` e `command` non sono nel
--     match del criteria_runner (catch-all "tipo sconosciuto" -> passed=false), e
--     `command` non e' nemmeno nel vocabolario insegnato dal prompt (mig 0436) —
--     e' deriva del modello, che nessuna validazione impedisce;
--   - ogni todo cosi' consumerebbe `max_verify_cycles` (3) giri, cioe' due
--     iniezioni di <verification_failed> e altrettanti giri LLM, per poi finire
--     `Blocked`.
-- Accendere l'enforcement oggi significherebbe rompere il 57% dei piani per un
-- disallineamento di vocabolario che va prima sanato.
--
-- `observe`: i criteri vengono ESEGUITI e il loro esito PERSISTITO in
-- nexus_agent_verifier_runs, ma il verdetto del todo resta quello di prima.
-- Serve a rispondere con dati veri alla domanda "cosa succede accendendo
-- l'enforcement?", che finora non aveva risposta perche' quella tabella era
-- vuota. Un dry-run che NON esegue misurerebbe un'imitazione (regola O).
--
-- I tre valori sono identificatori canonici in inglese (regola N), parse dal
-- punto unico `TodoCriteriaMode::try_parse`; un valore ignoto ricade su `off`
-- con un WARN, mai su un enforcement accidentale.
--
-- Reversibile a caldo (regola G): il verifier rilegge la config a ogni run.
INSERT INTO settings (key, value, description, updated_at)
VALUES (
    'agent.verifier.todo_criteria_mode',
    'observe',
    'Cosa fa il verifier con gli acceptance_criteria di un todo: off (non li esegue) | observe (li esegue e ne registra l''esito, ma il verdetto del todo non cambia) | enforce (l''esito dei criteri decide il todo). Passare a enforce solo dopo aver letto nexus_agent_verifier_runs e sanato il vocabolario dei criteri.',
    NOW()
)
ON CONFLICT (key) DO UPDATE
    SET description = EXCLUDED.description,
        updated_at  = NOW();
