-- 0741_prenotazione_porta_dichiarata.sql
--
-- NUMERO: massimo effettivo sul disco alla stesura = 0740
-- (`niente-elenco-che-assolve`). Prendo la 0741, la prima libera: due file con
-- lo stesso numero e sqlx ne applica UNO SOLO, in silenzio.
--
-- `request_port` PROMETTE una porta che il registro non sa TRATTENERE.
--
-- MISURATO il 18/08/2026 in esercizio (biblioteca-18-08, project_id
-- 0ca22a2d-a5ab-4ecb-9b76-8e72970c75c1): l'agente chiede due porte alle
-- 20:49:28 e le ottiene (34184 backend, 34150 frontend, `allocation_mode`
-- 'dynamic'); alle 20:54:16 il log di mcp-core scrive «port_gc: rilasciate 2
-- allocazioni orfane (nessun listener)» e 39 secondi dopo le STESSE due
-- chiamate rispondono di nuovo 'dynamic' con gli stessi numeri — cioe' la riga
-- per quella label non esisteva piu'. Il tool non aveva mentito sulla
-- SCRITTURA (l'audit `port_allocate` c'e', e un INSERT fallito sarebbe uscito
-- come `RegistroNonScritto`): mentiva sulla DURATA.
--
-- LA CAUSA E' CHE IL REGISTRO NON SA RAPPRESENTARE «PRENOTATA». Le uniche
-- prove di vita che il GC (`port_registry::allocazione_da_preservare`) accetta
-- sono OSSERVATE — un listener TCP sulla porta — o DERIVATE DALL'AVVIO — la
-- colonna `service_unit`, che scrive `link_allocation_to_service_unit` quando
-- il servizio parte. Una riga appena creata da `find_or_allocate` non ha ne'
-- l'una ne' l'altra PER COSTRUZIONE, quindi e' indistinguibile dal residuo di
-- un tentativo fallito che quel GC esiste per raccogliere. Fra la richiesta
-- della porta e l'avvio del servizio passano i minuti in cui l'agente scrive
-- il codice, installa le dipendenze e attraversa il gate a due giudici: la
-- grace di 180s (`agent.port_gc.grace_seconds`) e' una stima implicita di quel
-- tempo, e non regge.
--
-- ALZARE LA GRACE SAREBBE LA TOPPA (regola H): nasconde il sintomo dietro un
-- numero che sara' di nuovo sbagliato al primo run piu' lento, e lascia intatto
-- il fatto che una PROMESSA e un RESIDUO restino indistinguibili.
--
-- IL RIMEDIO E' UNA TERZA PROVA, DICHIARATA. La prenotazione dice CHI la tiene:
-- il run che l'ha chiesta. Vive finche' vive quel run, e non un secondo di
-- piu' — chiuso il run (completed, blocked, interrupted) la riga torna
-- raccoglibile senza aspettare nessun timer nuovo. Il proprietario della vita
-- e' il RUN, non l'orologio.
--
-- PERCHE' UNA COLONNA E NON UN SETTING: non e' configurazione, e' un FATTO su
-- una riga specifica (regola G). Un `reserved_until` sarebbe la stessa grace
-- con un altro nome, e mentirebbe con l'aria di un dato.
--
-- NULL = nessuna prenotazione dichiarata. E' il caso delle righe nate fuori da
-- un run (wizard, pannello Servizi, avvio managed) e di tutto lo storico: per
-- loro il criterio resta esattamente quello di prima. Nessun backfill: una
-- prenotazione retroattiva sarebbe inventata, e terrebbe in vita righe che
-- nessuno ha chiesto.
--
-- NIENTE FOREIGN KEY: `agent_runs` vive nel DB del PROGETTO dal cutover della
-- mig 0507, questa tabella nel META. Il riferimento e' fra due database e la
-- sua integrita' non e' esprimibile a schema — la risolve
-- `prenotazione_porta::VitaDelRun`, che interroga il DB del progetto e
-- distingue «run chiuso» da «non ho potuto chiedere».

ALTER TABLE nexus_port_allocations
    ADD COLUMN IF NOT EXISTS prenotata_da_run UUID;

COMMENT ON COLUMN nexus_port_allocations.prenotata_da_run IS
    'Il run agentico che ha PRENOTATO questa porta con request_port, e che la '
    'tiene in vita finche'' e'' attivo (terza prova di '
    'port_registry::allocazione_da_preservare, accanto al listener e a '
    'service_unit). NULL = nessuna prenotazione dichiarata: riga nata fuori da '
    'un run, o antecedente alla mig 0741. Nessuna FK: agent_runs vive nel DB '
    'del progetto (mig 0507), questa tabella nel META.';

-- Il GC scandisce le righe non-`manual` oltre la grace e per ognuna deve
-- sapere se e' prenotata. Indice PARZIALE: le righe con prenotazione sono la
-- minoranza e sono le sole che comportano una domanda al DB del progetto.
CREATE INDEX IF NOT EXISTS idx_port_alloc_prenotata_da_run
    ON nexus_port_allocations (prenotata_da_run)
    WHERE prenotata_da_run IS NOT NULL;

DO $$
DECLARE
    v_tipo TEXT;
    v_nullable TEXT;
BEGIN
    SELECT data_type, is_nullable INTO v_tipo, v_nullable
      FROM information_schema.columns
     WHERE table_name = 'nexus_port_allocations'
       AND column_name = 'prenotata_da_run';

    IF v_tipo IS NULL THEN
        RAISE EXCEPTION 'mig 0741: la colonna prenotata_da_run non esiste: il GC non avrebbe la terza prova e request_port continuerebbe a promettere una riga che evapora';
    END IF;
    IF v_tipo <> 'uuid' THEN
        RAISE EXCEPTION 'mig 0741: prenotata_da_run e'' di tipo % invece che uuid: un run_id non e'' rappresentabile', v_tipo;
    END IF;
    -- NOT NULL sarebbe indistinguibile da «ogni riga e'' prenotata»: l'assenza
    -- di prenotazione DEVE restare rappresentabile (regola Q).
    IF v_nullable <> 'YES' THEN
        RAISE EXCEPTION 'mig 0741: prenotata_da_run e'' NOT NULL: le righe nate fuori da un run non sarebbero piu'' scrivibili';
    END IF;

    RAISE NOTICE 'mig 0741: prenotata_da_run (uuid, nullable) disponibile come terza prova del port_gc';
END $$;
