-- 0013_cleanup_anchor_identity_phantom_services.sql
-- Bonifica delle voci fantasma nate dal ripiego sull'identita' di ancoraggio.
--
-- Causa radice (corretta nel codice insieme a questa migrazione,
-- mcp-core/src/agent_tools/service.rs): la cartella da cui un servizio gira e'
-- dichiarabile in due posti — il parametro `working_dir` del tool e il `cd` in
-- testa al comando (`cd frontend && npm run dev`, eseguito con `bash -c` a
-- partire da `working_dir`) — ma solo il primo veniva letto. Un comando lanciato
-- DALLA RADICE col `cd` dentro di se' non aveva quindi nessun segnale di ruolo, e
-- l'identita' ripiegava sull'ancoraggio `service-<primi 8 esadecimali
-- dell'uuid>`. Essendo lo stesso per ogni servizio del progetto, backend e
-- frontend collassavano in un'unica voce: nel pannello Servizi comparivano TRE
-- servizi per un'app che ne ha due (misurato il 30-31/07/2026 su
-- bacheca-attivita, 3 righe: `cd frontend && npm run dev`, `cd backend && npm
-- start` x2).
--
-- Quelle righe non rappresentano nessun servizio esistente: l'identita' e' nata
-- da un ripiego che oggi non si produce piu' per quei comandi, e al prossimo
-- avvio ciascuno prendera' la propria (`frontend`, `backend`). Restano visibili
-- per sempre finche' non si tolgono, perche' `visible_windows_services` nasconde
-- le label morte solo se generiche o superseded da una label SIMILE, e
-- `similar_service_labels('service-66f4bf72', 'frontend')` e' falso per
-- costruzione. Si eliminano come fece la 0004 con le voci one-shot.
--
-- Cosa NON si tocca, e perche':
-- - running/starting: li' c'e' un processo vivo, che dal pannello va potuto
--   fermare. Prendera' l'identita' giusta al prossimo avvio;
-- - le righe di ancoraggio SENZA `cd`: quelle sono legittime. Un progetto
--   mono-servizio avviato dalla radice riceve quell'identita' di proposito
--   (`ServiceIdentity::SoloAncoraggio`), perche' un server ha bisogno di un nome
--   stabile fra i riavvii per ricevere e conservare la sua porta.
--
-- Il predicato sul comando non duplica il parse del punto unico (regola L): non
-- deve dire QUALE cartella — quello lo decide solo `cd_dichiarato` — gli basta
-- distinguere le righe in cui una cartella era dichiarata. Se risulta piu' largo
-- del parse cancella una riga terminale in piu', che era comunque un ripiego; se
-- piu' stretto, la riga resta finche' il prossimo avvio non la sostituisce.
--
-- L'identita' e' ricostruita come la costruisce il codice (`service-` + primi 8
-- esadecimali del project_id), non con una regex che le somigli: cosi' non puo'
-- colpire una label scelta dall'agente che capiti di avere quella forma.
DELETE FROM agent_processes
WHERE kind = 'service'
  AND status NOT IN ('running', 'starting')
  AND label = 'service-' || left(replace(project_id::text, '-', ''), 8)
  AND command ~ '^\s*[cC][dD]\s';
