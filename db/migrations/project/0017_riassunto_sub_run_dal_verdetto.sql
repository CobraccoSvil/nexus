-- 0017_riassunto_sub_run_dal_verdetto.sql
-- Recupero del RIASSUNTO dei sub-run chiusi da un finalizzatore muto.
--
-- Causa radice (corretta nel codice insieme a questa migrazione: punto unico
-- nexus-agent-graph/src/decisions/run_summary.rs, innestato in
-- mcp-core/src/agent_tools/subagent_native.rs::finalize_success e in
-- mcp-core/src/chat_messages/agent_run.rs::native_outcome_to_run_result).
--
-- La domanda «qual e' il riassunto di questo run?» aveva DUE risposte diverse e
-- nessun punto unico:
--   - il run PRINCIPALE aveva un ripiego, ma per un solo finalizzatore
--     (`declared_outcome.summary`, cioe' `task_complete`);
--   - la chiusura del SUB-run non ne aveva nessuno: scriveva il solo testo
--     libero (`NativeRunOutcome.final_answer`), e basta.
--
-- Gli schemi di `advisory_verdict`, `review_verdict`, `debate_position` e
-- `task_complete` dichiarano `summary` come campo OBBLIGATORIO, e le loro
-- descrizioni ordinano di chiamare il tool «come ULTIMISSIMA azione». Una figura
-- che obbedisce chiude con la sola tool_use, senza prosa: il suo parere era
-- scritto, strutturato e obbligatorio, e il riassunto restava la stringa vuota.
-- Non e' una differenza fra i tool — `task_complete` perdeva un run su quattro
-- esattamente come gli altri: e' l'assenza del punto unico.
--
-- Conseguenza sui dati: `nexus_subagent_runs.final_summary` e la gemella
-- `agent_runs.final_answer` (scritte dalla STESSA statement, `mark_run`) restano
-- vuote, e con esse il payload `summary` della narrazione da cui il nastro
-- attivita' compone la riga di chiusura del sub-agente.
--
-- MISURATO il 08/08/2026 sui tre DB-progetto vivi (gestione_corsi_nexus,
-- agenda_medica_nexus, biblioteca_scolastica_nexus), 148 sub-run storici:
--   advisory_verdict  75 run, 23 vuoti (31%) -- 23 su 23 con summary dichiarato
--   review_verdict    19 run,  3 vuoti (16%) --  3 su 3
--   task_complete     17 run,  4 vuoti (24%) --  4 su 4
--   debate_position    6 run,  0 vuoti
--   nessuno (timeout) 31 run,  3 vuoti       --  0 su 3 (nessuna dichiarazione)
-- 30 riassunti vuoti su 30 avevano il campo `summary` compilato.
--
-- L'informazione NON e' andata perduta, ed e' il motivo per cui questa
-- migrazione esiste invece di limitarsi a dichiarare il buco: la colonna
-- `verdict` (mig project/0009) porta i blocchi normalizzati dei finalizzatori,
-- `summary` compreso. Si rimette in colonna.
--
-- PRECEDENZA identica a quella del punto unico (`run_summary::ORDINE`): i tre
-- verdetti di RUOLO prima della chiusura generica `declared`. Oggi la
-- precedenza non e' osservabile -- MISURATO: nessun run ha piu' di un blocco
-- valorizzato (0 o 1 su 148, mai 2) -- ma un ordine diverso qui e nel codice
-- sarebbe una divergenza silenziosa fra lo storico e cio' che il sistema
-- scrivera' da qui in avanti.
--
-- TAGLIO a 4000 caratteri: e' il taglio che `mark_run` applica alla fonte
-- (`c.summary.chars().take(4000)`), e `left()` conta caratteri come lui. Senza,
-- lo storico recuperato avrebbe una lunghezza che il codice non produce.
--
-- Ambito: le sole righe col riassunto VUOTO e una dichiarazione non vuota. Un
-- run senza dichiarazione (timeout, errore del motore) non ha un riassunto da
-- recuperare e resta com'e': l'assenza li' e' un fatto, non un buco.
-- Idempotente: dopo l'UPDATE `final_summary` non e' piu' vuoto, quindi una
-- riesecuzione non matcha il WHERE.
--
-- Cio' che NON si recupera, dichiarato invece che lasciato intendere: le
-- decisioni gia' prese leggendo quel vuoto restano quelle che furono -- un
-- coordinatore che ha visto un sub-run muto ha pianificato su quel silenzio, e
-- riempire la colonna oggi non riscrive il run che ne e' seguito.

WITH dichiarato AS (
    SELECT
        s.id,
        left(
            btrim(COALESCE(
                s.verdict->'review'->>'summary',
                s.verdict->'advisory'->>'summary',
                s.verdict->'debate'->>'summary',
                s.verdict->'declared'->>'summary'
            )),
            4000
        ) AS testo
    FROM public.nexus_subagent_runs s
    WHERE s.final_summary IS NULL OR btrim(s.final_summary) = ''
), recuperabili AS (
    SELECT id, testo FROM dichiarato WHERE testo IS NOT NULL AND testo <> ''
), sub AS (
    UPDATE public.nexus_subagent_runs s
       SET final_summary = r.testo
      FROM recuperabili r
     WHERE s.id = r.id
)
-- La gemella `agent_runs` del figlio: stessa fonte, stesso taglio, stessa
-- statement. Le due righe sono scritte insieme da `mark_run` e allinearle in
-- due passaggi separati lascerebbe una finestra in cui dicono cose diverse.
UPDATE public.agent_runs a
   SET final_answer = r.testo
  FROM recuperabili r
 WHERE a.id = r.id
   AND (a.final_answer IS NULL OR btrim(a.final_answer) = '');
