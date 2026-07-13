-- 0579_observer_boot_grace.sql
--
-- Boot-grace del service_observer (regola H, causa radice "la chat riparte da sola"
-- dopo un deploy). Subito dopo un restart di mcp-core i servizi di progetto sono nel
-- transitorio di riavvio (porte ancora occupate, servizio non ancora in ascolto):
-- l'observer li scambiava per crash e auto-triggerava un run di auto-debug che nessuno
-- ha chiesto (incidente Chat 11 Beaty-Book). Entro questa finestra dall'avvio del
-- processo gli auto-trigger di remediation restano inerti. Regola G: nessun hardcode,
-- soglia dal DB (default nel codice 90s se la chiave manca).

BEGIN;

INSERT INTO settings (key, value, category, description) VALUES (
    'agent.observer.boot_grace_seconds',
    '90',
    'agent',
    'Finestra (secondi) dopo l''avvio di mcp-core durante la quale il service_observer NON auto-diagnostica i servizi di progetto: evita che il transitorio di riavvio da deploy (porte occupate, non-listening) venga scambiato per crash e generi run di auto-debug non richiesti. 0 disabilita la guardia.'
)
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, description = EXCLUDED.description;

COMMIT;
