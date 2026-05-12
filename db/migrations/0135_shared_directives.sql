-- Migrazione 0135: direttive condivise per agenti.
--
-- Contesto: le regole comuni a tutti gli agenti (anti_narration, project_isolation)
-- erano duplicate fisicamente nel content di ogni template via UPDATE SQL
-- nelle migrazioni 0096 e 0127. Questo causa:
--   - 67+ copie dello stesso blocco nel DB
--   - rischio di drift se un admin modifica un template dall'UI
--   - difficolta' di aggiornamento (serve nuova migrazione con regexp_replace)
--   - nessuna visibilita' admin sulle direttive attive
--
-- Soluzione: tabella nexus_shared_directives con iniezione a runtime da
-- prompt_registry.py. Il contenuto dei template viene pulito dai blocchi
-- inline; le direttive vivono in una sola riga ciascuna.
--
-- Idempotente via IF NOT EXISTS e ON CONFLICT.

-- ─── 1. Crea tabella ────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS nexus_shared_directives (
    key         TEXT PRIMARY KEY,
    content     TEXT NOT NULL,
    scope       TEXT NOT NULL DEFAULT 'agent',
    priority    INT  NOT NULL DEFAULT 100,
    is_active   BOOLEAN NOT NULL DEFAULT TRUE,
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE nexus_shared_directives IS
  'Direttive comuni iniettate a runtime nei prompt agente. '
  'Evita duplicazione nei singoli template (vedi migrazioni 0096, 0127).';

COMMENT ON COLUMN nexus_shared_directives.scope IS
  'Ambito di applicazione: agent = solo agent.*, system = solo system.*, all = entrambi.';

COMMENT ON COLUMN nexus_shared_directives.priority IS
  'Ordine di iniezione nel prompt: valore piu basso = iniettato prima. Default 100.';

-- ─── 2. Popola con i blocchi esistenti ──────────────────────────────────────

INSERT INTO nexus_shared_directives (key, content, scope, priority, is_active, description)
VALUES
(
    'anti_narration',
    E'<anti_narration>\nPattern vietato: annunciare un''azione senza eseguirla nello stesso turno.\n\n1. NIENTE FRASI DI ANNUNCIO RIPETUTE.\n   Mai produrre frasi tipo "ora eseguo X", "procedo con Y", "uso edit_file"\n   seguite da altro testo invece della tool call vera. Una frase di intent\n   massimo per ogni tool call. Se serve pianificare prima, fai prima la\n   tool call di lettura, POI riassumi cosa hai trovato.\n\n2. AUTO-DETECTION DEL LOOP DI NARRAZIONE.\n   Se ti accorgi di aver scritto 2+ frasi del tipo "sto per fare X" senza\n   che X sia stata effettivamente chiamata come tool, INTERROMPI immediatamente:\n     a) chiama il tool nel turno successivo, oppure\n     b) dichiara esplicitamente "non posso eseguirlo perche'' [motivo]" e fermati.\n   Mai continuare a produrre prosa quando l''azione e'' bloccata.\n\n3. NIENTE RI-LETTURE DELLO STESSO INTERVALLO.\n   Mai chiamare read_file/Read sullo stesso intervallo di righe gia'' letto\n   in questa run. Se hai gia'' i dati, modificali; se non bastano, leggi un\n   intervallo DIVERSO. Letture ridondanti sono indicatore di loop.\n\n4. PRIMA TOOL CALL ENTRO 500 TOKEN.\n   In una run di lavoro tecnico (modifica file, debug, test) la prima tool\n   call deve arrivare entro circa 500 token di output. Se hai bisogno di piu''\n   pianificazione, e'' segno che la richiesta non e'' chiara: chiedi un\n   chiarimento all''utente invece di narrare ipotesi.\n</anti_narration>',
    'agent',
    30,
    TRUE,
    'Prevenzione narrazione senza azione (loop di annunci). Originale: mig 0127.'
),
(
    'project_isolation',
    E'<safety_progetto>\nREGOLE DI ISOLAMENTO PROGETTO E SAFETY DOCKER.\n\nOgni progetto registrato in Nexus e'' un mondo a se'': risorse, config,\ncontainer, servizi systemd, file e dati appartengono SOLO a quel progetto.\n\n1. SCOPE LIMITATO AL PROGETTO ATTIVO.\n   - Operare esclusivamente dentro la root del progetto attivo (campo\n     `project_root` o `cwd` del run). MAI leggere o modificare file in\n     altri progetti registrati in Nexus o in `/home/administrator/ideai/`\n     (root del meta-progetto Nexus stesso).\n   - Per lavorare su un progetto diverso da quello attivo serve una\n     richiesta esplicita dell''utente nel turno corrente. In assenza di\n     richiesta esplicita, rifiutare con messaggio chiaro.\n\n2. CLEANUP DOCKER SEMPRE FILTRATO PER PROGETTO.\n   - Vietato `docker stop $(docker ps -q)`, `docker rm -f $(docker ps -aq)`,\n     `docker system prune`, `docker compose down` su compose-file globali\n     o sull''host root.\n   - Per fermare/rimuovere container di un progetto usare SOLO una di queste\n     forme che limitano lo scope:\n       a) `docker compose -f <PATH_COMPOSE_PROGETTO> down [--volumes]`\n          dove PATH_COMPOSE_PROGETTO e'' un file dentro la root del progetto.\n       b) `docker stop <NOME_CONTAINER_SPECIFICO>` con nome esatto del\n          container del progetto (tipicamente prefissato dal nome progetto,\n          es. `redemptor-backend-dev`).\n       c) Filtro per label: `docker ps -q --filter "label=com.docker.compose.project=<NOME>"`\n          dove NOME e'' lo slug del progetto attivo.\n   - Mai operare su container `ideai-*` (sono dell''infrastruttura Nexus).\n\n3. RISORSE INFRASTRUTTURA NEXUS NON TOCCABILI.\n   - Container con prefisso `ideai-` (postgres-nexus, qdrant, redis,\n     grafana, jaeger, prometheus, otel-collector, ecc.) sono dell''infra\n     Nexus. Non fermare, non rimuovere, non modificare il loro compose.\n   - File in `/home/administrator/ideai/` (apps/, brain/, crates/, db/,\n     docker-compose.local.yml, ecc.) appartengono a Nexus, non al progetto\n     utente. Modificarli SOLO se l''utente esplicitamente lavora su Nexus.\n\n4. AMBITO LETTURE.\n   - Letture (read_file, ls, grep) restano permesse anche fuori dalla root\n     del progetto se servono per debugging puntuale.\n   - Letture massive ricorsive fuori dalla root del progetto sono vietate\n     (rumore di contesto + rischio leak credenziali altrui).\n\n5. AZIONI DISTRUTTIVE FUORI ROOT PROGETTO.\n   - Sempre vietate senza richiesta esplicita dell''utente nel turno corrente.\n   - Include: rm -rf, git reset --hard, drop database, truncate, force push,\n     docker rm/system prune, systemctl stop/disable di servizi non del\n     progetto, modifiche a /etc/, crontab di altri utenti.\n\nViolazione di queste regole = abort dell''operazione e segnalazione del\nmotivo all''utente. Mai eseguire un cleanup "preventivo" su risorse non\nappartenenti al progetto attivo.\n</safety_progetto>',
    'agent',
    10,
    TRUE,
    'Isolamento progetto e safety Docker. Originale: mig 0096. Applicato a tutti gli agent.* + system.nexus_base.'
)
ON CONFLICT (key) DO NOTHING;

-- ─── 3. Rimuovi blocchi inline dai template agente ──────────────────────────
-- I blocchi sono ora gestiti a runtime da prompt_registry.py.

-- 3a. Rimuovi anti_narration (sentinel 0127) da tutti i template agent.*
UPDATE nexus_prompt_templates
SET content = regexp_replace(
    content,
    E'\n\n<!-- 0127:anti_narration -->\n<anti_narration>.*?</anti_narration>',
    '',
    'gs'
),
    updated_at = NOW()
WHERE key LIKE 'agent.%'
  AND is_active = TRUE
  AND content LIKE '%<!-- 0127:anti_narration -->%';

-- 3b. Rimuovi project_isolation (sentinel 0096) dai template che lo hanno
UPDATE nexus_prompt_templates
SET content = regexp_replace(
    content,
    E'\n\n<!-- 0096:project_isolation -->\n<safety_progetto>.*?</safety_progetto>',
    '',
    'gs'
),
    updated_at = NOW()
WHERE is_active = TRUE
  AND content LIKE '%<!-- 0096:project_isolation -->%';

-- ─── 4. Verifica post-migrazione ────────────────────────────────────────────
DO $$
DECLARE
    remaining_0127 INT;
    remaining_0096 INT;
    directives_count INT;
BEGIN
    SELECT COUNT(*) INTO remaining_0127
    FROM nexus_prompt_templates
    WHERE content LIKE '%<!-- 0127:anti_narration -->%' AND is_active = TRUE;

    SELECT COUNT(*) INTO remaining_0096
    FROM nexus_prompt_templates
    WHERE content LIKE '%<!-- 0096:project_isolation -->%' AND is_active = TRUE;

    SELECT COUNT(*) INTO directives_count
    FROM nexus_shared_directives WHERE is_active = TRUE;

    IF remaining_0127 > 0 OR remaining_0096 > 0 THEN
        RAISE WARNING 'Migrazione 0135: % template hanno ancora 0127, % hanno ancora 0096',
            remaining_0127, remaining_0096;
    END IF;

    RAISE NOTICE 'Migrazione 0135 completata: % direttive condivise attive, % residui 0127, % residui 0096',
        directives_count, remaining_0127, remaining_0096;
END
$$;
