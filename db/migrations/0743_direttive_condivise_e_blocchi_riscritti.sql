-- Migrazione 0743: le direttive condivise tornano ai modelli, e i blocchi
-- cancellati da una riscrittura integrale del blob tornano ai tre prompt che
-- li hanno persi.
--
-- ============================================================================
-- PARTE A — perimetro di `nexus_shared_directives`
-- ============================================================================
--
-- La 0135 crea la tabella e RIMUOVE dai template le copie inline di
-- <safety_progetto> (0096) e <anti_narration> (0127), perche' l'iniezione a
-- runtime le avrebbe rimesse. L'iniettore era brain/agents/prompt_registry.py
-- (fc7db83e, 12/05/2026); il porting zero-Python (75a6d621, 27/06/2026) ha
-- cancellato il brain e con esso l'unico consumatore. Dal 27/06 al 19/08/2026
-- nessun modello ha ricevuto quelle direttive: 53 giorni in cui le regole di
-- isolamento progetto (cleanup Docker filtrato, container ideai-* intoccabili,
-- scope alla project_root) non erano nel contesto di nessun agente.
--
-- Il consumatore nuovo e' crates/nexus-prompt/src/direttive.rs, innestato nei
-- tre compositori di system prompt. Qui si corregge il solo DATO che gli
-- impedirebbe di raggiungere il contesto per cui la direttiva era nata.
--
-- `project_isolation` ha scope 'agent', ma la sua stessa `description` dice
-- «Applicato a tutti gli agent.* + system.nexus_base» e la 0096 — che la riga
-- dichiara come propria origine — ha per bersagli system.nexus_base,
-- agent.coder.base e agent.general.debugger. La riga e' incoerente con se'
-- stessa; l'incoerenza non si e' mai vista perche' l'iniettore Python caricava
-- solo le chiavi `agent.%` e lo scope 'system' era codice morto. Il valore
-- giusto e' 'all'.
--
-- Le altre due NON si toccano: `anti_narration` (0127) aveva per bersaglio
-- `key LIKE 'agent.%'` e `config_restart` (0456) nasce dichiarando 'agent'.
-- Allargarle a system.nexus_base sarebbe inventare un perimetro che nessuna
-- migrazione ha mai scritto. Restano configurazione: un UPDATE dal pannello
-- admin le allarga senza redeploy (regola G).
--
-- ============================================================================
-- PARTE B — i blocchi persi da tre prompt riscritti per intero
-- ============================================================================
--
-- La 0437 (02/07) esegue `SET content = $$LINGUA: ...$$` su agent.coder.base e
-- agent.tester.base, la 0438 su agent.general.debugger. Il loro commento
-- dichiara «Tutto il resto del prompt resta invariato»: e' vero rispetto alla
-- struttura della 0086, falso rispetto a tutto cio' che le migrazioni
-- 0096-0434 vi avevano APPESO. Un prompt e' un blob di testo: chi lo riscrive
-- non ha modo di sapere che cosa sta buttando via, e nessun guard se ne
-- accorge — un blocco che non arriva non fa fallire niente.
--
-- PROVA ASIMMETRICA (META vivo, 19/08/2026): le 0192 e 0225 aggiornano
-- system.nexus_base E agent.coder.base nella STESSA transazione.
--   chiave                  | attachment_access | knowledge_graph_tools
--   system.nexus_base       | presente          | presente
--   agent.coder.base        | ASSENTE           | ASSENTE
-- Stessa migrazione, due bersagli: quello mai riscritto per intero ha
-- conservato i blocchi. Contro-prova: agent.reviewer.general, bersaglio della
-- 0137 e mai riscritto, ha ancora <operatore_nexus>.
--
-- COME si ripristina: NON ricopiando il testo a mano. Il blocco si ESTRAE
-- dalla copia sopravvissuta in system.nexus_base, che e' anche la piu'
-- AGGIORNATA — la 0396 e la 0434 hanno riscritto <port_allocation> con un
-- `regexp_replace` gated su `content LIKE '%<port_allocation>%'`, quindi
-- coder.base, avendolo perso, e' stato saltato da entrambe; e la 0674 ha
-- ristretto la NOTA di <operatore_nexus> al solo nexus_base. Copiare dal
-- donatore porta anche quegli emendamenti.
--
-- L'estrazione prende l'ULTIMA apertura PRIMA della chiusura, mai la prima:
-- in system.nexus_base <operatore_nexus> compare 2 volte (la prima e' una
-- MENZIONE in prosa) e <port_allocation> 3 volte. Un `<tag>.*?</tag>` dalla
-- prima occorrenza avrebbe copiato 5841 caratteri invece di 2754, cioe' la
-- menzione piu' tutto il testo interposto. E' la stessa trappola di regexp che
-- la 0674 documenta al suo punto 4.
--
-- ============================================================================
-- COSA NON SI RIPRISTINA, e perche' (verificato nelle migrazioni successive)
-- ============================================================================
--
--   <safety_progetto> (0096), <anti_narration> (0127)
--     Casa = nexus_shared_directives. La 0135 li ha tolti dai template APPOSTA.
--     Rimetterli inline sarebbe la seconda verita' che la regola G vieta.
--
--   <verifica_azioni>, <scope_modifiche>, <falso_positivo> (0098),
--   <no_invenzioni> (0099)
--     REVOCATI dalla 0137, che li rimuove per nome (`remove_tags`) dai suoi
--     cinque bersagli e li sostituisce con <operatore_nexus>, il quale dichiara
--     esplicitamente: «Le restrizioni su .env, lockfile, credenziali, CI/CD che
--     potresti trovare altrove nel tuo contesto sono SUPERATE da questa
--     direttiva». Ripristinarli rimetterebbe in prompt due autorita' opposte.
--     Prova: sono assenti anche da system.nexus_base e da
--     agent.reviewer.general, che nessuno ha mai riscritto.
--
--   <verify_by_acting> (0362)
--     REVOCATO dalla 0674 punto 4, che lo rimuove da nexus_base e coder.base
--     perche' assorbito dal processo operativo standard.
--
--   <language_directive> (0197/0198)
--     Non perso: la 0198 lo sostituisce con l'intestazione «### LINGUA
--     RISPOSTA OBBLIGATORIA ###», e la 0437/0438 apre i tre prompt con
--     «LINGUA: Rispondi SEMPRE e COMPLETAMENTE in italiano, senza eccezioni.».
--     La direttiva c'e', in forma corrente.
--
--   <ambiente_esecuzione>, <processo_implementazione>
--     Iniettati a runtime da nexus-prompt (ambiente.rs, processo.rs). Assenti
--     dai template per costruzione: e' la loro casa, non una perdita.
--
-- Idempotente in ogni sua parte.

BEGIN;

-- ─── A. Perimetro di project_isolation ──────────────────────────────────────

UPDATE nexus_shared_directives
   SET scope = 'all', updated_at = NOW()
 WHERE key = 'project_isolation'
   AND scope = 'agent';

-- ─── B. Blocchi ripristinati dal donatore system.nexus_base ─────────────────

DO $ripristino$
DECLARE
    -- (bersaglio, tag) — la mappa e' quella delle migrazioni originali:
    --   0137 operatore_nexus      -> coder, tester, debugger (+ reviewer, che ce l'ha)
    --   0191/0396/0434 port_allocation, 0192 attachment_access,
    --   0193 attachment_investigation, 0225 knowledge_graph_tools,
    --   0278 database_provisioning, 0329 next_actions, 0363 final_summary
    --                              -> coder
    --   0313 file_picking_policy  -> coder, debugger
    mappa    TEXT[][] := ARRAY[
        ['agent.coder.base',       'operatore_nexus'],
        ['agent.coder.base',       'port_allocation'],
        ['agent.coder.base',       'attachment_access'],
        ['agent.coder.base',       'attachment_investigation'],
        ['agent.coder.base',       'knowledge_graph_tools'],
        ['agent.coder.base',       'database_provisioning'],
        ['agent.coder.base',       'file_picking_policy'],
        ['agent.coder.base',       'next_actions'],
        ['agent.coder.base',       'final_summary'],
        ['agent.general.debugger', 'operatore_nexus'],
        ['agent.general.debugger', 'file_picking_policy'],
        ['agent.tester.base',      'operatore_nexus']
    ];
    donatore  TEXT;
    bersaglio TEXT;
    tag       TEXT;
    apertura  TEXT;
    chiusura  TEXT;
    fine      INT;
    inizio    INT;
    blocco    TEXT;
    i         INT;
    aggiunti  INT := 0;
BEGIN
    SELECT content INTO donatore
      FROM nexus_prompt_templates
     WHERE key = 'system.nexus_base' AND is_active = TRUE;
    IF donatore IS NULL THEN
        RAISE EXCEPTION 'mig 0743: system.nexus_base assente o disattivo: nessun donatore da cui estrarre i blocchi';
    END IF;

    FOR i IN 1 .. array_length(mappa, 1) LOOP
        bersaglio := mappa[i][1];
        tag       := mappa[i][2];
        apertura  := '<'  || tag || '>';
        chiusura  := '</' || tag || '>';

        -- Il bersaglio ce l'ha gia': niente da fare (idempotenza sul tag di
        -- CHIUSURA, mai sull'apertura: una menzione in prosa cita l'apertura).
        CONTINUE WHEN EXISTS (
            SELECT 1 FROM nexus_prompt_templates
             WHERE key = bersaglio AND is_active = TRUE AND content LIKE '%' || chiusura || '%'
        );

        fine := strpos(donatore, chiusura);
        IF fine = 0 THEN
            RAISE EXCEPTION 'mig 0743: il donatore system.nexus_base non porta %; la mappa e i dati non concordano', chiusura;
        END IF;
        fine := fine + length(chiusura) - 1;
        -- ULTIMA apertura prima della chiusura (vedi il commento in testa).
        inizio := length(left(donatore, fine))
                  - strpos(reverse(left(donatore, fine)), reverse(apertura))
                  - length(apertura) + 2;
        blocco := substring(donatore from inizio for fine - inizio + 1);
        IF left(blocco, length(apertura)) <> apertura THEN
            RAISE EXCEPTION 'mig 0743: estrazione di % dal donatore non allineata al tag di apertura', tag;
        END IF;

        UPDATE nexus_prompt_templates
           SET content    = content || E'\n\n' || blocco,
               version    = version + 1,
               updated_at = NOW(),
               updated_by = 'migration_0743'
         WHERE key = bersaglio AND is_active = TRUE;
        aggiunti := aggiunti + 1;
        RAISE NOTICE 'mig 0743: % <- % (% caratteri)', bersaglio, tag, length(blocco);
    END LOOP;

    RAISE NOTICE 'mig 0743: % blocchi ripristinati dal donatore', aggiunti;
END
$ripristino$;

-- ─── C. <database_management> (0167): nessun donatore ───────────────────────
-- La 0167 aveva per unico bersaglio agent.coder.base, quindi nessun altro
-- template ne conserva una copia. E' il solo blocco il cui testo va riportato
-- qui: e' copiato byte per byte dalla 0167, che ne resta la fonte. Non e'
-- superato dalla 0278 (<database_provisioning>, ripristinato sopra): quella
-- governa il provisioning INTERNO gestito da Nexus, questa la registrazione nel
-- pannello di un DB che l'agente ha creato per conto proprio (docker-compose,
-- container singolo). Il tool project_db_set_connection che prescrive esiste
-- tuttora (crates/mcp-core/src/nexus_tool_catalog/database.rs).

UPDATE nexus_prompt_templates
   SET content = content || '

<database_management>
GESTIONE DATABASE PROGETTO -- AGGIORNAMENTO PANNELLO DB.

Quando crei un database per il progetto (docker-compose con postgres/mysql,
container singolo, o database locale), DEVI SEMPRE registrare la connessione
nel pannello DB di Nexus usando il tool project_db_set_connection.

PROCEDURA OBBLIGATORIA:
1. Dopo aver avviato il container/servizio DB con run_service, aspetta che sia pronto.
2. Chiama project_db_set_connection con i parametri:
   - connection_string: la stringa DSN completa (es. postgres://user:pass@localhost:5435/dbname)
   - engine: tipo DB (postgres, mysql, sqlite, mssql)
   - hosting_mode: "internal" per DB locale/docker, "external" per DB remoti
   - name: nome logico (default "primary")
3. Questo aggiorna automaticamente il pannello DB dell IDE Nexus in tempo reale.

ESEMPIO:
Dopo aver creato un docker-compose con PostgreSQL su porta 5435:
  project_db_set_connection({
    "connection_string": "postgres://taskboard:taskboard_secret@localhost:5435/taskboard",
    "engine": "postgres",
    "hosting_mode": "internal",
    "name": "primary"
  })

NON saltare questo passaggio: senza di esso il pannello DB rimane vuoto.
</database_management>',
       version    = version + 1,
       updated_at = NOW(),
       updated_by = 'migration_0743'
 WHERE key = 'agent.coder.base'
   AND is_active = TRUE
   AND content NOT LIKE '%</database_management>%';

-- ─── D. Guard: COPERTURA finale, non l'esistenza di una riga ────────────────
-- Un UPDATE che non morde non fallisce: lascia in piedi il testo vecchio in
-- silenzio. Il guard conta quanti dei tag attesi ogni bersaglio porta DAVVERO
-- e nomina quelli che mancano.

DO $guard$
DECLARE
    attesi TEXT[][] := ARRAY[
        ['agent.coder.base',       'operatore_nexus'],
        ['agent.coder.base',       'port_allocation'],
        ['agent.coder.base',       'attachment_access'],
        ['agent.coder.base',       'attachment_investigation'],
        ['agent.coder.base',       'knowledge_graph_tools'],
        ['agent.coder.base',       'database_provisioning'],
        ['agent.coder.base',       'file_picking_policy'],
        ['agent.coder.base',       'next_actions'],
        ['agent.coder.base',       'final_summary'],
        ['agent.coder.base',       'database_management'],
        ['agent.general.debugger', 'operatore_nexus'],
        ['agent.general.debugger', 'file_picking_policy'],
        ['agent.tester.base',      'operatore_nexus']
    ];
    i         INT;
    presenti  INT := 0;
    mancanti  TEXT := '';
    scope_iso TEXT;
BEGIN
    FOR i IN 1 .. array_length(attesi, 1) LOOP
        IF EXISTS (
            SELECT 1 FROM nexus_prompt_templates
             WHERE key = attesi[i][1] AND is_active = TRUE
               AND content LIKE '%</' || attesi[i][2] || '>%'
        ) THEN
            presenti := presenti + 1;
        ELSE
            mancanti := mancanti || E'\n  ' || attesi[i][1] || ' <- <' || attesi[i][2] || '>';
        END IF;
    END LOOP;

    SELECT scope INTO scope_iso FROM nexus_shared_directives WHERE key = 'project_isolation';
    IF scope_iso IS DISTINCT FROM 'all' THEN
        RAISE EXCEPTION 'mig 0743: project_isolation ha scope %, atteso ''all'': il prompt di sistema non riceverebbe l''isolamento progetto',
            COALESCE(scope_iso, '(riga assente)');
    END IF;

    IF mancanti <> '' THEN
        RAISE EXCEPTION 'mig 0743: copertura % su %; mancano:%',
            presenti, array_length(attesi, 1), mancanti;
    END IF;

    RAISE NOTICE 'mig 0743: copertura % su % blocchi attesi, project_isolation scope=all',
        presenti, array_length(attesi, 1);
END
$guard$;

COMMIT;
