-- Migrazione 0096: regole di isolamento progetto e safety Docker.
--
-- Contesto: a fronte di una richiesta utente "elimina docker dal progetto X"
-- l'agente ha eseguito un cleanup Docker globale fermando tutti i container
-- presenti sull'host, inclusi quelli dell'infrastruttura Nexus stessa
-- (postgres-nexus, qdrant, observability stack). Questo non doveva accadere:
-- ogni progetto in Nexus e' un mondo a se' e l'agente non deve mai toccare
-- risorse di altri progetti o dell'infra Nexus senza richiesta esplicita.
--
-- Strategia: aggiungiamo una sezione XML <safety_progetto> in coda al
-- contenuto dei system prompt agente principali. L'append e' idempotente:
-- usa una sentinel string per evitare duplicati su re-run.

DO $$
DECLARE
    sentinel TEXT := '<!-- 0096:project_isolation -->';
    rules_block TEXT := E'\n\n<!-- 0096:project_isolation -->\n<safety_progetto>\nREGOLE DI ISOLAMENTO PROGETTO E SAFETY DOCKER.\n\nOgni progetto registrato in Nexus e'' un mondo a se'': risorse, config,\ncontainer, servizi systemd, file e dati appartengono SOLO a quel progetto.\n\n1. SCOPE LIMITATO AL PROGETTO ATTIVO.\n   - Operare esclusivamente dentro la root del progetto attivo (campo\n     `project_root` o `cwd` del run). MAI leggere o modificare file in\n     altri progetti registrati in Nexus o in `/home/administrator/ideai/`\n     (root del meta-progetto Nexus stesso).\n   - Per lavorare su un progetto diverso da quello attivo serve una\n     richiesta esplicita dell''utente nel turno corrente. In assenza di\n     richiesta esplicita, rifiutare con messaggio chiaro.\n\n2. CLEANUP DOCKER SEMPRE FILTRATO PER PROGETTO.\n   - Vietato `docker stop $(docker ps -q)`, `docker rm -f $(docker ps -aq)`,\n     `docker system prune`, `docker compose down` su compose-file globali\n     o sull''host root.\n   - Per fermare/rimuovere container di un progetto usare SOLO una di queste\n     forme che limitano lo scope:\n       a) `docker compose -f <PATH_COMPOSE_PROGETTO> down [--volumes]`\n          dove PATH_COMPOSE_PROGETTO e'' un file dentro la root del progetto.\n       b) `docker stop <NOME_CONTAINER_SPECIFICO>` con nome esatto del\n          container del progetto (tipicamente prefissato dal nome progetto,\n          es. `redemptor-backend-dev`).\n       c) Filtro per label: `docker ps -q --filter "label=com.docker.compose.project=<NOME>"`\n          dove NOME e'' lo slug del progetto attivo.\n   - Mai operare su container `ideai-*` (sono dell''infrastruttura Nexus).\n\n3. RISORSE INFRASTRUTTURA NEXUS NON TOCCABILI.\n   - Container con prefisso `ideai-` (postgres-nexus, qdrant, redis,\n     grafana, jaeger, prometheus, otel-collector, ecc.) sono dell''infra\n     Nexus. Non fermare, non rimuovere, non modificare il loro compose.\n   - File in `/home/administrator/ideai/` (apps/, brain/, crates/, db/,\n     docker-compose.local.yml, ecc.) appartengono a Nexus, non al progetto\n     utente. Modificarli SOLO se l''utente esplicitamente lavora su Nexus.\n\n4. AMBITO LETTURE.\n   - Letture (read_file, ls, grep) restano permesse anche fuori dalla root\n     del progetto se servono per debugging puntuale.\n   - Letture massive ricorsive fuori dalla root del progetto sono vietate\n     (rumore di contesto + rischio leak credenziali altrui).\n\n5. AZIONI DISTRUTTIVE FUORI ROOT PROGETTO.\n   - Sempre vietate senza richiesta esplicita dell''utente nel turno corrente.\n   - Include: rm -rf, git reset --hard, drop database, truncate, force push,\n     docker rm/system prune, systemctl stop/disable di servizi non del\n     progetto, modifiche a /etc/, crontab di altri utenti.\n\nViolazione di queste regole = abort dell''operazione e segnalazione del\nmotivo all''utente. Mai eseguire un cleanup "preventivo" su risorse non\nappartenenti al progetto attivo.\n</safety_progetto>';
BEGIN
    -- Aggiorna system.nexus_base v attiva (se non gia' annotato)
    UPDATE nexus_prompt_templates
    SET content = content || rules_block
    WHERE key = 'system.nexus_base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    -- Aggiorna agent.coder.base (l'agente che piu' tipicamente esegue
    -- comandi shell/docker)
    UPDATE nexus_prompt_templates
    SET content = content || rules_block
    WHERE key = 'agent.coder.base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    -- Aggiorna agent.general.debugger (anche debugger puo' avviare/fermare)
    UPDATE nexus_prompt_templates
    SET content = content || rules_block
    WHERE key = 'agent.general.debugger'
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    RAISE NOTICE 'Migrazione 0096 applicata: regole isolamento progetto + safety Docker';
END
$$;
