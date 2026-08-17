-- 0738_avvio_servizio_vivo.sql
--
-- VINCOLO DI NUMERAZIONE: massimo effettivo sul disco alla stesura = 0733.
-- 0734 (codice eseguibile), 0735 (capienza TPM), 0736 (step validation) e 0737
-- (pavimento del piano di verifica) sono prenotate da cantieri in volo e non
-- ancora scritte. Questa e' la 0738 per non collidere con nessuna di quelle:
-- due file con lo stesso numero e sqlx ne applica UNO SOLO, in silenzio.
--
-- «Servizio avviato» deve voler dire che il servizio E' VIVO.
--
-- MISURATO il 17/08/2026 in esercizio (app libri): l'agente lancia
-- `node backend/src/index.js`, il processo esce con codice 1 perche' l'npm
-- install era fallito a meta' (`Cannot find module 'express'`), e
-- `agent_processes` registra `status=failed, exit_code=1`. Il sistema aveva
-- scritto la morte nella propria tabella; nessuno lo ha detto all'agente, che
-- ha proseguito.
--
-- Il tool osservava un fatto solo: la porta risponde? Dell'altro — il processo
-- e' ancora vivo? — non c'era ne' l'osservazione ne' un campo in cui metterla,
-- quindi una morte accertata restava indistinguibile da un avvio lento, e la
-- natura del fallimento era `Transitorio`: cioe' l'istruzione a ritentare
-- identico, data a un processo che non puo' tornare su senza una correzione.
--
-- Le due chiavi qui sotto governano l'attesa. Il CRITERIO della vita non e'
-- nuovo e non e' loro: resta quello della remediation
-- (`service_recovery::await_port_ready` = `probe_port` + `stable_enough`),
-- delegato da `agent_tools::avvio_servizio` (regola L).

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.service.readiness_timeout_s', '20', 'agent',
     'Finestra massima (secondi) in cui run_service attende che il servizio appena lanciato dia prova di vita: la porta assegnata risponde, oppure il processo esce. Ritorna appena uno dei due fatti si presenta, quindi un servizio sano non la paga. Scaduta senza ne'' l''uno ne'' l''altro: il processo e'' vivo ma muto, e l''esito lo dichiara invece di annunciare un avvio riuscito. DISTINTA da agent.playwright.readiness_timeout_seconds (mig 0662): quella e'' l''attesa che un bersaglio gia'' avviato sia CALDO prima di lanciarci una suite, questa e'' l''attesa che un servizio appena lanciato sia SALITO. Default 20.',
     NOW()),
    ('agent.service.morte_precoce_finestra_s', '5', 'agent',
     'Finestra (secondi) entro cui si osserva l''uscita del processo quando nessuna porta e'' attesa (worker, comandi in background): li'' l''unico fatto disponibile e'' se il processo sia sopravvissuto al proprio lancio. Sostituisce l''attesa cieca a tempo fisso che c''era prima, e non e'' piu'' lenta: a parita'' di finestra ritorna appena c''e'' qualcosa da dire. Un''uscita con codice 0 non e'' un fallimento (un comando one-shot ha finito il proprio lavoro); una con codice diverso da zero e'' un lancio fallito, e l''output del processo va all''agente come diagnosi. Default 5.',
     NOW())
ON CONFLICT (key) DO NOTHING;
