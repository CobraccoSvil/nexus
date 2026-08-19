-- Migrazione 0744: un prompt e' un BLOB di testo, e chi lo riscrive cancella in
-- silenzio cio' che altri vi hanno appeso. Da qui in avanti non piu': una
-- scrittura che fa sparire un blocco viene RIFIUTATA finche' qualcuno non
-- dichiara di volerlo rimuovere.
--
-- ============================================================================
-- IL DIFETTO, misurato (19/08/2026, META vivo)
-- ============================================================================
--
-- La 0437 (02/07) esegue `SET content = $$LINGUA: ...$$` su agent.coder.base e
-- agent.tester.base, la 0438 la stessa cosa su agent.general.debugger. Il loro
-- commento dichiara «Tutto il resto del prompt resta invariato»: vero rispetto
-- alla struttura della 0086, FALSO rispetto ai 23 blocchi che le migrazioni
-- 0096-0434 vi avevano APPESO. Prova asimmetrica: le 0192 e 0225 aggiornano
-- system.nexus_base E agent.coder.base nella STESSA transazione, e solo il
-- primo — mai riscritto per intero — porta ancora <attachment_access> e
-- <knowledge_graph_tools>.
--
-- Nessuno se n'e' accorto per 48 giorni, e non per distrazione: un blocco che
-- non arriva al modello non fa fallire niente. Non c'e' una query che diventa
-- rossa, non c'e' un test che rosseggia, non c'e' un log. La 0743 ha rimesso i
-- 13 blocchi recuperabili; questa toglie il modo di rifare il danno.
--
-- ============================================================================
-- PERCHE' UN TRIGGER, e non un guard da chiamare
-- ============================================================================
--
-- Un guard che l'autore deve INVOCARE non copre il caso per cui nasce: chi
-- riscrive un blob non sa di stare buttando via qualcosa, quindi non sa nemmeno
-- di dover chiamare il guard. L'unico presidio che copre chi non lo conosce e'
-- quello che si interpone fra la scrittura e la tabella.
--
-- E' lo stesso ragionamento — e lo stesso meccanismo — della 0499
-- (`trg_settings_guard_protected`): il guard vive nel DB come punto unico
-- (regola L), cosi' copre QUALSIASI vettore invece di essere replicato in ogni
-- scrittore, e le modifiche legittime passano da una dichiarazione ESPLICITA.
--
-- PORTATA TEMPORALE: le migrazioni 0001-0743 girano PRIMA che questi oggetti
-- esistano, quindi un DB ricostruito da zero non e' toccato e la storia non va
-- riscritta. Governato e' solo cio' che viene dopo — che e' esattamente il
-- mandato: rendere impossibile la RIPETIZIONE.
--
-- ============================================================================
-- IL CRITERIO: il tag di CHIUSURA, e perche' non e' un confronto fragile
-- ============================================================================
--
-- Un blocco e' `<nome>...</nome>` e si conta sulla CHIUSURA, mai sull'apertura:
-- una MENZIONE in prosa cita `<port_allocation>` senza esserlo (la trappola che
-- la 0674 documenta al suo punto 4, e su cui la 0743 ha dovuto correggere
-- l'estrazione dal donatore).
--
-- MISURATO sul corpus vivo (META, 19/08/2026): 69 tag di chiusura distinti su
-- 174 righe attive, e NESSUNO e' markup accidentale — non c'e' un solo `</div>`, `</p>`, `</li>`.
-- In questa tabella un tag di chiusura significa una cosa sola.
--
-- POLARITA' del riconoscimento: e' deliberatamente PERMISSIVO
-- (`[a-zA-Z][a-zA-Z0-9_-]*`). Un tag in piu' fra quelli sorvegliati produce un
-- rifiuto rumoroso, che si vede e si discute; un tag in meno produce una
-- perdita silenziosa, che e' il difetto stesso. L'errore cade dalla parte in
-- cui si vede.
--
-- ============================================================================
-- LA VIA D'USCITA: dichiarare, non disattivare
-- ============================================================================
--
-- Rimuovere un blocco e' legittimo e succede (la 0137 revoca <verifica_azioni>,
-- <scope_modifiche>, <falso_positivo> e <no_invenzioni> sostituendoli con
-- <operatore_nexus>; la 0674 assorbe <verify_by_acting> nel processo standard).
-- Quelle rimozioni erano DECISIONI, e restano possibili — a condizione di
-- dirlo, nella stessa transazione:
--
--   SET LOCAL nexus.blocchi_rimossi = 'verify_by_acting,falso_positivo';
--
-- Il trigger tollera ESATTAMENTE i tag nominati. Non c'e' un jolly `*` e non e'
-- una svista: un elenco che ASSOLVE per intero e' un interruttore travestito da
-- dichiarazione, e lo si scriverebbe per abitudine. Cio' che si perde va detto
-- per nome, e resta scritto nella migrazione che lo perde.
--
-- COSA NON COPRE, dichiarato:
--   - INSERT: non c'e' un OLD, non c'e' niente da perdere.
--   - un blocco SVUOTATO (tag presenti, corpo sparito): il criterio guarda la
--     presenza del blocco, non la sua consistenza. E' una domanda diversa.
--   - `nexus_shared_directives`: li' il contenuto di una riga E' la direttiva,
--     non un blob a cui si appende. Il difetto che questa migrazione chiude non
--     e' rappresentabile su quella tabella.

BEGIN;

-- ─── 1. Punto unico: quali blocchi DICHIARA questo contenuto ────────────────
-- La domanda si pone in tre posti (il trigger, i test, qualunque diagnostica) e
-- ha una sola risposta. Una seconda regexp scritta altrove sarebbe una seconda
-- idea di "blocco", e le due divergerebbero al primo tag con un trattino.

CREATE OR REPLACE FUNCTION nexus_prompt_blocchi(contenuto TEXT)
RETURNS TEXT[] AS $$
    SELECT COALESCE(array_agg(DISTINCT m[1] ORDER BY m[1]), ARRAY[]::TEXT[])
      FROM regexp_matches(COALESCE(contenuto, ''), '</([a-zA-Z][a-zA-Z0-9_-]*)>', 'g') AS m;
$$ LANGUAGE sql IMMUTABLE;

COMMENT ON FUNCTION nexus_prompt_blocchi(TEXT) IS
  'I blocchi che un contenuto di prompt dichiara, per tag di CHIUSURA (una '
  'menzione in prosa cita l''apertura e non e'' un blocco). Punto unico del '
  'criterio: vi delegano il trigger trg_prompt_blocchi_* e i test.';

-- ─── 2. Punto unico: che cosa questa scrittura farebbe SPARIRE ──────────────

CREATE OR REPLACE FUNCTION nexus_prompt_blocchi_persi(vecchio TEXT, nuovo TEXT)
RETURNS TEXT[] AS $$
    SELECT COALESCE(array_agg(b ORDER BY b), ARRAY[]::TEXT[])
      FROM unnest(nexus_prompt_blocchi(vecchio)) AS b
     WHERE b <> ALL (nexus_prompt_blocchi(nuovo));
$$ LANGUAGE sql IMMUTABLE;

COMMENT ON FUNCTION nexus_prompt_blocchi_persi(TEXT, TEXT) IS
  'I blocchi presenti nel vecchio contenuto e assenti dal nuovo. Direzionale: '
  'aggiungerne non e'' perderne.';

-- ─── 3. Il presidio ─────────────────────────────────────────────────────────

CREATE OR REPLACE FUNCTION nexus_prompt_templates_guard_blocchi()
RETURNS trigger AS $guard$
DECLARE
    contenuto_nuovo TEXT;
    persi           TEXT[];
    dichiarati      TEXT[];
    non_dichiarati  TEXT[];
BEGIN
    -- Un DELETE porta via tutto: il "nuovo contenuto" e' il vuoto. Chiude la
    -- via del delete+insert, che riscriverebbe un template scavalcando
    -- l'UPDATE. Costo su cio' che si fa gia': nessuno — nessuna delle 743
    -- migrazioni precedenti ha mai cancellato un template.
    IF TG_OP = 'DELETE' THEN
        contenuto_nuovo := '';
    ELSE
        contenuto_nuovo := NEW.content;
    END IF;

    persi := nexus_prompt_blocchi_persi(OLD.content, contenuto_nuovo);

    IF array_length(persi, 1) IS NOT NULL THEN
        dichiarati := ARRAY(
            SELECT btrim(v)
              FROM unnest(string_to_array(
                       COALESCE(current_setting('nexus.blocchi_rimossi', true), ''), ',')) AS v
             WHERE btrim(v) <> ''
        );
        non_dichiarati := ARRAY(SELECT p FROM unnest(persi) AS p WHERE p <> ALL (dichiarati));

        IF array_length(non_dichiarati, 1) IS NOT NULL THEN
            RAISE EXCEPTION
                'prompt "%": questa scrittura (%) farebbe sparire % blocchi che nessuno ha dichiarato di voler rimuovere: %. Un prompt e'' un blob: chi lo riscrive per intero cancella cio'' che altre migrazioni vi hanno appeso (le 0437/0438 hanno perso 23 blocchi cosi'', e per 48 giorni non se n''e'' accorto nessuno). Per APPENDERE usa `content = content || ...`. Se la rimozione e'' VOLUTA, dichiarala per nome nella stessa transazione: SET LOCAL nexus.blocchi_rimossi = ''%'';',
                OLD.key,
                TG_OP,
                array_length(non_dichiarati, 1),
                array_to_string(ARRAY(SELECT '<' || p || '>' FROM unnest(non_dichiarati) AS p), ', '),
                array_to_string(non_dichiarati, ',')
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END IF;

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END
$guard$ LANGUAGE plpgsql;

-- Due trigger e una sola funzione: la clausola WHEN esiste solo per l'UPDATE, e
-- serve — senza, ogni `SET is_active = FALSE` pagherebbe due scansioni regexp su
-- un testo da decine di KB per una domanda la cui risposta e' gia' nota.
DROP TRIGGER IF EXISTS trg_prompt_blocchi_update ON nexus_prompt_templates;
CREATE TRIGGER trg_prompt_blocchi_update
  BEFORE UPDATE ON nexus_prompt_templates
  FOR EACH ROW
  WHEN (OLD.content IS DISTINCT FROM NEW.content)
  EXECUTE FUNCTION nexus_prompt_templates_guard_blocchi();

DROP TRIGGER IF EXISTS trg_prompt_blocchi_delete ON nexus_prompt_templates;
CREATE TRIGGER trg_prompt_blocchi_delete
  BEFORE DELETE ON nexus_prompt_templates
  FOR EACH ROW
  EXECUTE FUNCTION nexus_prompt_templates_guard_blocchi();

-- ─── 4. Il presidio e' ARMATO: provato qui, sullo schema vero ───────────────
-- Un guard che nessuno ha visto fallire non e' un guard: e' una speranza. Qui
-- lo si esercita al momento dell'installazione, sul DB reale che sta per
-- riceverlo, in una sotto-transazione che non lascia traccia. Vale per il META
-- di produzione come per ogni DB effimero dei test.

DO $prova$
DECLARE
    ha_rifiutato BOOLEAN := FALSE;
    conserva     BOOLEAN := FALSE;
BEGIN
    INSERT INTO nexus_prompt_templates (key, category, title, content, is_active)
    VALUES ('_prova_0744', 'system', 'prova del presidio',
            E'testa\n<prova_del_presidio>corpo</prova_del_presidio>\ncoda', FALSE);

    -- (a) la riscrittura integrale che perde il blocco DEVE essere rifiutata.
    BEGIN
        UPDATE nexus_prompt_templates
           SET content = 'LINGUA: testo nuovo, tutto il resto invariato.'
         WHERE key = '_prova_0744';
    EXCEPTION WHEN integrity_constraint_violation THEN
        ha_rifiutato := TRUE;
    END;
    IF NOT ha_rifiutato THEN
        RAISE EXCEPTION 'mig 0744: il presidio non e'' armato — una riscrittura che perde <prova_del_presidio> e'' passata';
    END IF;

    -- (b) l'append non e' una perdita: deve passare, e conservare il blocco.
    UPDATE nexus_prompt_templates
       SET content = content || E'\n<altro>x</altro>'
     WHERE key = '_prova_0744';
    SELECT 'prova_del_presidio' = ANY(nexus_prompt_blocchi(content))
      INTO conserva
      FROM nexus_prompt_templates WHERE key = '_prova_0744';
    IF NOT conserva THEN
        RAISE EXCEPTION 'mig 0744: un append ha perso il blocco preesistente';
    END IF;

    -- (c) la rimozione DICHIARATA passa: la via d'uscita esiste davvero.
    PERFORM set_config('nexus.blocchi_rimossi', 'prova_del_presidio,altro', TRUE);
    DELETE FROM nexus_prompt_templates WHERE key = '_prova_0744';
    PERFORM set_config('nexus.blocchi_rimossi', '', TRUE);

    IF EXISTS (SELECT 1 FROM nexus_prompt_templates WHERE key = '_prova_0744') THEN
        RAISE EXCEPTION 'mig 0744: la riga di prova non e'' stata rimossa';
    END IF;

    RAISE NOTICE 'mig 0744: presidio armato — riscrittura lossy rifiutata, append ammesso, rimozione dichiarata ammessa';
END
$prova$;

COMMIT;
