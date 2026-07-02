-- Bonifica one-time delle voci fantasma del pannello Servizi.
--
-- Causa radice (corretta nel codice insieme a questa migrazione): i launcher
-- delle run configuration e l'auto-routing background di run_command
-- registravano OGNI processo in agent_processes con kind='service', ignorando
-- il role della configurazione (frontend/backend/service/test/tool). Cosi'
-- task one-shot (pnpm install, playwright test, pnpm add) e tentativi morti
-- comparivano per sempre nel pannello Servizi, che su Windows elenca le label
-- distinte con kind='service' (list_services_windows).
--
-- Le righe kind='service' in stato terminale sono voci storiche non piu'
-- rappresentative: si eliminano (stesso criterio dell'endpoint "pulisci
-- processi", processes.rs). I servizi attivi (running/starting) restano;
-- quelli morti sono ri-lanciabili dalle run configuration del pannello Run.
DELETE FROM agent_processes
WHERE kind = 'service'
  AND status IN ('stopped', 'failed');
