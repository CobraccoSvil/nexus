-- 0363_final_summary_base_prompts.sql
--
-- Sintomo: in modalita' "Continuo" (modalita' agente principale) la chat
-- termina senza riepilogo: l'utente vede l'agente eseguire tool, propone
-- "Prossimi passi", scrive "Procedo a leggere X" come ultima riga, ma il
-- run finisce senza spiegare cosa ha fatto davvero -> sembra che debba
-- ancora continuare.
--
-- Root cause: la mig 0341 ha aggiunto la direttiva RIEPILOGO FINALE
-- OBBLIGATORIO SOLO in `automation.mode_automatic_instruction`, ma i
-- base prompts (`system.nexus_base`, `agent.coder.base`) — il cuore di
-- OGNI turno agentico, anche in Continuo — non la avevano. Il
-- NEXUS_CONTINUATION_PROTOCOL gia' presente vieta "Procedo a..." come
-- ultima riga, ma da solo non basta: il modello viola il protocollo se
-- nessun blocco esplicito chiede il riepilogo dei risultati eseguiti.
--
-- Fix: aggiunge `<final_summary>` ai due base prompts, idempotente
-- (`NOT LIKE`), append-safe (preserva append futuri). Pattern allineato
-- a mig 0341 ma esteso a tutti i path agentici.

BEGIN;

UPDATE nexus_prompt_templates
SET content = content || E'\n\n<final_summary>\nRIEPILOGO FINALE OBBLIGATORIO al termine del turno agentico, indipendentemente da automation_mode (Confirm/Automatico/Continuo).\n\nPrima di chiudere il turno (cioe'' prima del marker "TASK COMPLETATO" del NEXUS_CONTINUATION_PROTOCOL, o quando smetti di emettere tool call), DEVI scrivere un breve riepilogo (3-6 punti) di COSA HAI FATTO REALMENTE in questo turno:\n  - file creati / modificati / eliminati (con il path)\n  - comandi eseguiti con esito (status code, output rilevante, errori osservati)\n  - tool call eseguite con il risultato che hanno restituito\n  - eventuali problemi residui o passi non completati, con motivo\n\nSe non hai eseguito alcuna azione concreta in questo turno (es. solo lettura di file per capire), dillo esplicitamente: "In questo turno ho solo letto X, Y, Z per capire la struttura. Nessuna modifica applicata. Procedo nel prossimo turno con..." (se sei in Continuo) oppure "Aspetto la tua conferma per..." (se Confirm).\n\nVietato come ultima riga del turno:\n  - "Procedo a leggere ..." senza poi chiamare il tool nello stesso turno (annuncio senza azione, banditto dal NEXUS_CONTINUATION_PROTOCOL).\n  - "Ora analizzo / Ora verifico / Sto per ..." come chiusura prosa: o esegui il tool nello stesso turno, o concludi con un riepilogo reale.\n  - "Prossimi passi" come unica chiusura senza prima il riepilogo di cosa hai fatto IN QUESTO turno.\n\nObbligatorio: prima il riepilogo di cosa hai fatto IN QUESTO turno, poi (opzionale) la sezione "Prossimi passi" per la pianificazione, infine il marker di chiusura del NEXUS_CONTINUATION_PROTOCOL.\n</final_summary>',
    updated_at = NOW()
WHERE key IN ('system.nexus_base', 'agent.coder.base')
  AND content NOT LIKE '%<final_summary>%';

COMMIT;
