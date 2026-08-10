-- Chi puo' creare FILE deve poter creare CARTELLE, o usera' i file per farle.
--
-- MISURATO il 10/08/2026 sul progetto batteria-todo-app: il piano dell'agente
-- diceva «Crea la struttura di base: index.html, style.css, app.js, e directory
-- assets/ (vuota)», e sul disco `assets` e' un FILE da 0 byte. Non era una
-- svista del modello: il tool `fs_mkdir` esiste ed e' dispatchato
-- (`agent_tools/dispatch.rs`), ma non compare nella whitelist di NESSUNA delle
-- 21 figure, mentre sette di esse hanno `write_file`. Chiesta una cartella,
-- l'unico strumento a disposizione ne produce un file vuoto.
--
-- IL CRITERIO, e perche' non e' un elenco arbitrario: la capacita' si concede
-- alle figure che gia' SCRIVONO file. Non e' un privilegio nuovo — chi puo'
-- creare `assets/logo.svg` puo' gia' materializzare l'albero delle cartelle come
-- effetto collaterale — ma il modo ONESTO di fare cio' che gia' fa. Alle figure
-- di sola lettura e ai panel advisory non serve, e infatti non lo ricevono: una
-- whitelist che cresce «per sicurezza» smette di dire chi fa cosa.
--
-- Idempotente: `array_append` solo dove il tool manca, cosi' una riesecuzione
-- non duplica la voce.

UPDATE nexus_subagent_definitions
   SET tool_whitelist = array_append(tool_whitelist, 'fs_mkdir')
 WHERE tool_whitelist @> ARRAY['write_file']
   AND NOT (tool_whitelist @> ARRAY['fs_mkdir']);
