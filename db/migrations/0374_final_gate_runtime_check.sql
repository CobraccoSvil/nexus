-- 0374: verifica runtime E2E nel final_gate (anti "ho scritto il codice ma il
-- flusso reale fallisce"). Il final_gate, per i task software, oltre al criterio
-- anti-placeholder (no_orphan_imported) controlla ora i LOG dei servizi del
-- progetto: se contengono errori runtime (es. endpoint 500 perche' una tabella
-- manca / migrazione non applicata) il gate fallisce e reinietta all'agente la
-- diagnosi + l'ordine di correggere e RIVERIFICARE esercitando il flusso reale.
--
-- Causa: l'agente dichiarava "fatto" dopo aver scritto codice senza testare il
-- flusso (osservato su beauty-book: rotta /api/clients aggiunta ma tabella
-- clients mancante -> 500, mai verificato). Questo chiude il buco lato sistema.
--
-- Config DB-driven (regola G), letta da brain/agents/orchestrator_config.py.
-- Le liste sono CSV (parser _coerce). Il comando di default copre i progetti
-- docker-compose; per stack diversi l'admin puo' cambiarlo.

INSERT INTO settings (key, value, category, description) VALUES
    ('agent.final_gate.runtime_check_enabled', 'true', 'agent',
     'Se true, il final_gate per i task software controlla i log dei servizi del progetto (criterio service_logs_clean) e fallisce su errori runtime, obbligando l''agente a correggere e riverificare prima di chiudere.'),
    ('agent.final_gate.runtime_log_command', 'docker compose logs --tail 200 --no-color 2>&1 | tail -n 200', 'agent',
     'Comando eseguito nella project_root per leggere i log runtime dei servizi del progetto (usato dal criterio service_logs_clean del final_gate). Default per stack docker-compose.'),
    ('agent.final_gate.runtime_error_patterns', 'does not exist,ECONNREFUSED,Traceback (most recent call last),UnhandledPromiseRejection,Cannot find module,MODULE_NOT_FOUND,relation ",SequelizeDatabaseError,ER_NO_SUCH_TABLE,500 (Internal Server Error),Internal Server Error', 'agent',
     'CSV di pattern di errore runtime cercati nei log dei servizi dal final_gate. Se uno compare, il gate fallisce. Niente virgole DENTRO i singoli pattern (separatore CSV).')
ON CONFLICT (key) DO NOTHING;
