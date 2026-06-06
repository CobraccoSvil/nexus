-- 0341_automatic_mode_final_summary.sql
--
-- Sintomo: in modalita' automatica l'agente esegue i tool (es. delete_file,
-- write_file) ma chiude il run SENZA spiegare cosa ha fatto. L'utente vede le
-- azioni nelle "Decisioni del turno" ma la risposta finale e' vuota o generica.
--
-- Root cause: il prompt `automation.mode_automatic_instruction` (mig 0144) vieta
-- il "riepilogo del problema" (regola 3) intendendo quello PRELIMINARE (analisi
-- prima di agire), ma il modello lo interpreta come "niente riepiloghi affatto"
-- e termina senza il riepilogo FINALE. Il "Report finale" era previsto SOLO nella
-- sezione SCAFFOLDING APP ("Fai una app per X"), non per i task generici; la
-- sezione "A FINE TASK" parlava solo di commit/git.
--
-- Fix: (1) chiarire che il divieto riguarda SOLO il riepilogo preliminare;
--      (2) imporre un RIEPILOGO FINALE OBBLIGATORIO delle azioni a fine task.
-- REPLACE chirurgico (non SET) per preservare gli append di migrazioni
-- successive (es. 0293 protocollo di continuazione). Idempotente: la WHERE +
-- la natura di REPLACE evitano doppie applicazioni.

UPDATE nexus_prompt_templates
SET content = REPLACE(
        content,
        '3. Niente analisi preliminari lunghe. Niente "riepilogo del problema". Solo azioni concrete.',
        '3. Niente analisi preliminari lunghe ne "riepilogo del problema" PRIMA di agire: vai diretto alle azioni. Questo vieta solo il riepilogo PRELIMINARE, NON quello finale (vedi sezione A FINE TASK: il riepilogo delle azioni svolte resta obbligatorio).'
    ),
    updated_at = NOW()
WHERE key = 'automation.mode_automatic_instruction'
  AND content LIKE '%Niente "riepilogo del problema". Solo azioni concrete.%';

UPDATE nexus_prompt_templates
SET content = REPLACE(
        content,
        'A FINE TASK (chiusura del run agente)',
        E'A FINE TASK (chiusura del run agente)\n- RIEPILOGO FINALE OBBLIGATORIO: prima di chiudere il turno scrivi sempre un breve riepilogo (max 3-6 punti) di COSA HAI FATTO realmente: file creati/modificati/eliminati con il path, comandi eseguiti con l''esito, eventuali problemi residui o passi non completati. Se non hai eseguito alcuna azione, dillo esplicitamente e spiega perche''. Mai chiudere un run in cui hai eseguito tool senza spiegarne il risultato all''utente.'
    ),
    updated_at = NOW()
WHERE key = 'automation.mode_automatic_instruction'
  AND content LIKE '%A FINE TASK (chiusura del run agente)%'
  AND content NOT LIKE '%RIEPILOGO FINALE OBBLIGATORIO%';
