-- 0015_agent_steps_recupero_identita_ed_esito.sql
-- Recupero dell'identita' e dell'esito degli step gia' persistiti.
--
-- Causa radice (corretta nel codice insieme a questa migrazione,
-- nexus-agent-graph/src/runtime/ports.rs + mcp-core/src/agent_graph_adapter/
-- agent_step_store.rs): il contratto di persistenza di uno step erano due JSON
-- opachi. Il produttore (tool_dispatch) vi scriveva
-- `{"tool_name": ..., "tool_input": ...}` e, nel risultato, uno `status`
-- derivato dal flag strutturato `is_error`. Il consumatore leggeva ALTRE chiavi
-- (`block.get("name")`, `block.get("input")`) e sovrascriveva lo status con un
-- letterale `"completed"`. Nessuno dei due lati era sbagliato da solo: mancava
-- la giunzione come contratto, e nessun tipo la imponeva.
--
-- Conseguenza sui dati: la colonna `tool_name` restava vuota, `tool_input`
-- riceveva l'INVOLUCRO invece dell'input, e `status` era `completed` su ogni
-- riga, fallimenti compresi.
--
-- MISURATO il 02/08/2026 sul DB del progetto bacheca-attivita:
--   SELECT COALESCE(NULLIF(tool_name,''),'(VUOTO)'), status, count(*)
--     FROM agent_steps GROUP BY 1,2;
--   -> (VUOTO) | completed | 8860      -- una sola riga: nessuna eccezione
-- Dentro quegli 8860 passi, l'esito vero (`tool_result`->>'status') diceva
-- 8324 completed e 536 FAILED, distribuiti su 159 run distinti: 150 edit_file,
-- 100 read_file, 64 run_command, 31 run_service, 14 write_file e altri. Ogni
-- fallimento risultava un successo a tutti i consumatori a valle.
--
-- L'informazione NON e' andata perduta, ed e' il motivo per cui questa
-- migrazione esiste invece di limitarsi a dichiarare il buco: l'involucro
-- scritto in `tool_input` contiene il nome del tool e l'input veri (verificato:
-- 8860 righe su 8860 con `tool_name` annidato presente e non vuoto), e
-- `tool_result` contiene lo status vero. Si rimettono in colonna.
--
-- Ambito: le sole righe che portano l'involucro. Le righe scritte dal percorso
-- storico (`insert_run_steps`), che erano gia' corrette, non matchano il WHERE
-- e restano intatte. Idempotente: dopo l'UPDATE `tool_input` non contiene piu'
-- la chiave `tool_name`, quindi una riesecuzione non tocca nulla.
--
-- Cio' che NON si recupera, dichiarato invece che lasciato intendere: i run
-- passati non si ri-eseguono, quindi le decisioni gia' prese su questi dati
-- (final_gate `outputs_exist` risolto come "N/A", digest di worklog che
-- dichiaravano zero errori, recap con `error_count = 0`) restano quelle che
-- furono. La riparazione rende di nuovo diagnosticabile lo STORICO, non annulla
-- gli esiti che quello storico ha prodotto.

UPDATE agent_steps
SET
    -- Il nome del tool torna nella sua colonna. NULLIF: un blocco che non e' una
    -- tool_use ha nome vuoto anche nell'involucro, e resta vuoto.
    tool_name = COALESCE(NULLIF(tool_input ->> 'tool_name', ''), tool_name),
    -- L'input torna PIATTO: e' la forma che i consumatori interrogano
    -- (`tool_input->>'path'`, `.get("command")`).
    tool_input = CASE
        WHEN jsonb_typeof(tool_input -> 'tool_input') = 'object'
            THEN tool_input -> 'tool_input'
        ELSE '{}'::jsonb
    END,
    -- L'esito torna dallo status che il produttore aveva derivato da `is_error`.
    -- Si accettano SOLO i due identificatori canonici che quel produttore emette
    -- (regola N): qualunque altro valore, o un `tool_result` non interpretabile,
    -- lascia lo status invariato invece di inventarne uno.
    status = COALESCE(
        CASE
            WHEN pg_input_is_valid(tool_result, 'jsonb')
                THEN NULLIF(
                    (tool_result::jsonb) ->> 'status',
                    ''
                )
        END,
        status
    ),
    -- Il risultato resta il testo per l'umano, senza l'involucro che ne portava
    -- lo status: quello ora sta in colonna e non va letto da qui (regola M).
    tool_result = CASE
        WHEN pg_input_is_valid(tool_result, 'jsonb')
             AND jsonb_typeof(tool_result::jsonb) = 'object'
             AND (tool_result::jsonb) ? 'content'
            THEN (tool_result::jsonb) ->> 'content'
        ELSE tool_result
    END
WHERE tool_name = ''
  AND tool_input ? 'tool_name'
  AND tool_input ? 'tool_input';

-- Guard: uno status fuori vocabolario non deve entrare da qui. Se l'UPDATE
-- avesse prodotto un valore inatteso, la migrazione fallisce invece di lasciare
-- una colonna che i consumatori interpretano come "non fallito".
DO $$
DECLARE
    fuori_vocabolario bigint;
BEGIN
    SELECT count(*) INTO fuori_vocabolario
    FROM agent_steps
    WHERE status NOT IN (
        'completed', 'failed', 'running', 'skipped',
        'awaiting_confirmation', 'awaiting_subagents', 'provider_unavailable'
    );
    IF fuori_vocabolario > 0 THEN
        RAISE EXCEPTION
            'agent_steps: % righe con status fuori vocabolario dopo il recupero',
            fuori_vocabolario;
    END IF;
END $$;
