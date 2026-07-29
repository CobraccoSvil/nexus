-- 0653_remediation_verifica_esito_servizio.sql
-- Finestra di verifica dell'esito di una riparazione automatica di servizio.
--
-- Il difetto che accompagna: il ciclo di auto-remediation rilevava il guasto con
-- un segnale certo (bind error del SO), mandava l'AI a diagnosticare, e poi
-- dichiarava il successo su "processo avviato". Caso reale del 28/07/2026
-- (progetto gestione-spese): alle 21:29 "riavvio effettuato ... servizio
-- 'frontend' avviato", diagnosi chiusa come risolta; due ore dopo in ascolto
-- c'era solo il backend, il frontend era morto e la configurazione incoerente
-- che l'aveva ucciso era sopravvissuta alla "riparazione riuscita".
--
-- Da qui la chiusura passa da un contratto OSSERVABILE
-- (mcp-core::project_workspace::service_recovery): l'unit deve rispondere sulla
-- porta che il registro `nexus_port_allocations` assegna a lei, e continuare a
-- rispondere dopo un ulteriore riavvio (il servizio dell'incidente e' morto
-- proprio al secondo avvio). I servizi senza porta sono giudicati sulla liveness
-- OLTRE una finestra, mai sull'istante dello spawn. Un contratto non soddisfatto
-- non chiude: porta la diagnosi in `failed_remediation` con l'evidenza di cosa
-- non risponde e su quale porta.
--
-- Consumatore (regola G): service_recovery::readiness_window. Il default nel
-- codice vale solo a chiave assente ed e' identico al valore qui sotto.
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.remediation.verify_readiness_seconds', '45', 'agent',
    'Secondi di attesa perche'' un servizio appena riavviato da una riparazione automatica soddisfi il contratto: stato Running e almeno una delle porte allocate a quella unit che risponde. Scaduta la finestra senza contratto soddisfatto, la riparazione e'' dichiarata NON riuscita e la diagnosi resta visibile nel pannello Problemi con l''evidenza (quale porta e'' muta, con che stato e'' uscito il processo). Il minimo effettivo e'' 15 secondi: sotto quella soglia, per i servizi senza porta, "e'' vivo" tornerebbe a coincidere con l''istante dello spawn.'
)
ON CONFLICT (key) DO NOTHING;
