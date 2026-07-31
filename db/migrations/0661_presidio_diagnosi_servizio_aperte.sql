-- 0661_presidio_diagnosi_servizio_aperte.sql
-- Presidio delle diagnosi di servizio aperte: quando un rimedio automatico
-- viene ritentato, quante volte il ripiego deterministico interviene, e a che
-- distanza l'uno dall'altro.
--
-- Il difetto che accompagna. Il contratto di successo di una riparazione
-- esisteva gia' (mig 0654, mcp-core::project_workspace::service_recovery), ma
-- nessuno lo interrogava se non DOPO un run dell'AI riuscito: l'unica strada
-- verso un rimedio partiva dallo spawn del Debugger, interrogato UNA sola
-- volta al momento della rilevazione e mai piu' riconsiderato finche' il
-- servizio non ripartiva da solo. Ogni guardia che diceva "non ora"
-- (boot-grace, run gia' attivo sulla sessione, cap orario, nessuna sessione
-- chat) consumava quel colpo per sempre: un rinvio diventava una rinuncia.
--
-- MISURATO il 30-31/07/2026 sul progetto bacheca-attivita: tre diagnosi crash
-- aperte alle 20:51:53 e ancora aperte sette ore dopo, con le anomalie gemelle
-- riscritte 1806 volte (una ogni 15 secondi), triggered_run_id NULL su tutte e
-- zero tentativi di riparazione. Il log del processo riavviato alle 04:17:40
-- mostra la meccanica intera: cinque righe "boot-grace attivo, skip auto-debug"
-- tutte alle 04:18:08, e poi mai piu' una sola interrogazione del trigger. Non
-- e' un caso sfortunato: il primo ciclo dell'observer e' a +25 secondi
-- dall'avvio e la boot-grace dura 90, quindi per qualunque servizio gia' giu'
-- quando mcp-core riparte l'unico colpo cade dentro la finestra in cui il
-- trigger e' per costruzione inerte.
--
-- Da qui la presa in carico e' guidata dallo STATO DELLA RIGA (status,
-- triggered_run_id, ts, remediation_attempts, cooldown_until), riletto a ogni
-- ciclo dell'observer. Il presidio NON sostituisce l'AI: la RITENTA (stesso
-- trigger, stessi gate, nessuna logica duplicata — regola L). Solo una
-- diagnosi rimasta bloccata a lungo senza che l'AI sia mai partita — o con un
-- run AI interrotto da un riavvio di mcp-core — ricade su un riavvio
-- deterministico di ripiego, verificato sul contratto (regola M) prima di
-- toccare qualunque cosa: se il servizio risponde gia', si chiude senza
-- riavviare nulla.
--
-- Consumatore (regola G): service_recovery::repair_policy. I default nel
-- codice valgono solo a chiave assente e sono identici ai valori qui sotto.
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.remediation.auto_restart_enabled', 'true', 'agent',
    'Se il presidio delle diagnosi di servizio aperte puo'' ricadere sulla riparazione deterministica (riavvio + verifica del contratto: stato in esecuzione e almeno una porta allocata a quella unit che risponde, per una durata e dopo un ulteriore riavvio) per le diagnosi rimaste bloccate senza che il trigger AI sia mai partito. A false quelle diagnosi restano visibili nel pannello Problemi, ritentando solo il trigger AI.'
),
(
    'agent.remediation.max_restart_attempts', '3', 'agent',
    'Quanti tentativi di riparazione deterministica (il ripiego, non l''AI) si fanno su una singola diagnosi prima di dichiararla failed_remediation (stato terminale: il problema resta visibile con l''evidenza di cosa non risponde). Un tentativo solo rimetterebbe in circolo la decisione presa una volta: un intoppo transitorio chiuderebbe la riga per sempre.'
),
(
    'agent.remediation.retry_cooldown_seconds', '600', 'agent',
    'Distanza minima fra due tentativi di riparazione deterministica sulla stessa diagnosi, scritta in cooldown_until al momento della presa in carico. Spazia i tentativi; NON serve a far tacere una rilevazione ripetuta, che si chiude riparando o verificando, mai aspettando.'
),
(
    'agent.remediation.ai_trigger_stuck_after_seconds', '1800', 'agent',
    'Quanto una diagnosi di crash resta affidata SOLO ai ritentativi del trigger AI (maybe_trigger_debugger, invariato) prima di diventare ammissibile al ripiego deterministico. Deve superare con margine qualunque gate transitorio del trigger (boot-grace, cap orario): sotto questa soglia il presidio non riavvia nulla da solo, ritenta soltanto lo stesso trigger che avrebbe dovuto scattare alla rilevazione.'
)
ON CONFLICT (key) DO NOTHING;
