-- 0609_rimozione_fossili_brain_python.sql
--
-- Chiude nel DB la rimozione del brain Python, gia' fatta nel codice.
--
-- Il brain (servizio Python/LangGraph, REST su :8001 e gRPC su :50051) e' stato
-- eliminato dalle mig 0462 (servizio fermato e disabilitato) e 0532 (cutover del
-- motore). Le sue tracce nel DB pero' sono rimaste, e non erano innocue: erano
-- il carburante di rami di codice che continuavano a chiamarlo.
--
-- Le mig 0002 / 0190 / 0416 / 0451 / 0458 / 0460 / 0462 sono IMMUTABILI (sqlx ne
-- verifica il checksum): non si toccano, si correggono qui.
--
-- ---------------------------------------------------------------------------
-- 1. Settings che puntavano al brain
-- ---------------------------------------------------------------------------
-- `brain_rest_url` (mig 0190) e' il caso piu' istruttivo: era seedata a
-- 'http://127.0.0.1:8001' e NON vuota, quindi ogni call site che la leggeva
-- superava il proprio guard `filter(|v| !v.is_empty())` e sparava HTTP reale a
-- un servizio morto. Una configurazione che sopravvive al suo servizio non e'
-- un residuo passivo: e' un innesco.
--
-- `routing.classifier_engine` (0458, promossa a 'rust' dalla 0460) sceglieva fra
-- motore 'rust' e 'python'. Il valore 'python' restava accettato: un UPDATE di
-- "rollback" dall'aria innocua spegneva la classificazione. Il codice che la
-- leggeva (`select_classifier_engine`) e' stato rimosso.
--
-- `neural_core_url` (0002) veniva letta e passata a `NeuralCoreClient::connect`,
-- che IGNORAVA l'URL: attorno a una firma che mentiva erano cresciuti tre
-- lettori e un retry-loop da 60 tentativi su una funzione infallibile.
--
-- `brain_log_level` (ancora presente dopo la 0463) non aveva piu' nemmeno un
-- lettore nel repo.
DELETE FROM settings
 WHERE key IN (
        'brain_rest_url',
        'brain_log_level',
        'routing.classifier_engine',
        'neural_core_url'
       );

-- ---------------------------------------------------------------------------
-- 2. Sudo purpose che agivano su un servizio inesistente
-- ---------------------------------------------------------------------------
-- `brain-restart` (mig 0416, requires_confirm=false), `brain-stop` e
-- `brain-disable` (mig 0462) erano ancora enabled=true: comandi `systemctl`
-- verso `nexus-brain.service`, eseguibili da un agente senza conferma.
DELETE FROM nexus_sudo_purposes
 WHERE name IN ('brain-restart', 'brain-stop', 'brain-disable');

-- ---------------------------------------------------------------------------
-- 3. Tabella di selezione del motore
-- ---------------------------------------------------------------------------
-- `nexus_orchestrator_engine` (mig 0451) governava la strangler-fig: DEFAULT
-- 'python' e CHECK (engine IN ('python','rust','shadow')). Il codice che la
-- leggeva (`select_engine`) e' stato rimosso: il motore e' uno solo, quello
-- nativo. Dei tre valori ammessi solo 'rust' era vivo — 'python' chiamava il
-- brain e 'shadow' aveva come PRIMARIO proprio il path Python.
--
-- La tabella contiene solo configurazione di routing (nessun dato storico): al
-- momento della scrittura una riga, `('*', 'rust', 'global')`. Resta comunque
-- ricreabile dalla 0451 in caso di rollback del codice.
DROP TABLE IF EXISTS nexus_orchestrator_engine;

-- ---------------------------------------------------------------------------
-- NON si tocca `agent_runs.engine`: la colonna resta e viene valorizzata a
-- 'rust' su ogni run. Serve al recovery, che deve sapere con che motore girava
-- un run interrotto, e i run storici conservano il valore che avevano davvero.
--
-- Idempotente: DELETE per chiave e DROP IF EXISTS, ri-eseguirla non cambia nulla.
