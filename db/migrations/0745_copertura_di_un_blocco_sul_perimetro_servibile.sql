-- Migrazione 0745: «questo mandato include il blocco X?» smette di essere un
-- grep anche per chi scrive una migrazione.
--
-- ============================================================================
-- IL DIFETTO, misurato
-- ============================================================================
--
-- Il 18/08/2026 la domanda e' stata posta cosi', sul META vivo:
--
--   ILIKE '%prove eseguibili%'      -> 0 su 8   (falso)
--   LIKE  '%<prove_eseguibili>%'    -> 8 su 8   (vero)
--
-- In LIKE l'underscore e' un JOLLY e lo spazio no. Quello zero non era
-- distinguibile da «non c'e'», e su quello zero e' stato costruito - e fatto
-- implementare - un mandato di correzione per un difetto inesistente.
--
-- La 0744 ha dato il criterio strutturale (`nexus_prompt_blocchi`, per tag di
-- CHIUSURA). Restava che nessuno lo usasse per INTERROGARE: sulle migrazioni
-- applicate ci sono 18 righe `content LIKE '%<...'` sparse in 13 file, e ognuna
-- e' un'occasione per riscrivere quel pattern sbagliandolo.
--
-- ============================================================================
-- LA SECONDA META' DEL DIFETTO: il PERIMETRO
-- ============================================================================
--
-- Non basta chiedere bene: bisogna chiederlo alle righe GIUSTE. Il runtime puo'
-- servire, per una chiave, la riga base OPPURE la sua variante `.en` (mig 0726,
-- CSV `prompt.english_variants`) - e quale delle due serva ADESSO lo decide un
-- UPDATE senza redeploy, che puo' cambiare fra due chiamate. Un invariante di
-- contenuto vale quindi su TUTTE le righe servibili o su nessuna.
--
-- E' esattamente il difetto in cui e' caduta la 0739: aggiornava i due mandati
-- ITALIANI, il guard pretendeva «2 su 2» e passava verde, mentre in produzione
-- i giudici leggevano le righe `.en` che nessuno aveva toccato. Il rimedio non
-- e' ricordarsi del `.en`: e' che il perimetro non si scriva a mano.
--
-- `nexus_types::chiavi_servibili` e' il punto unico lato Rust; questa funzione
-- e' il suo gemello SQL, e i due sono confrontati da un test (regola O: due
-- implementazioni perche' SQL e Rust non si chiamano, MAI due criteri).
--
-- ============================================================================
-- POLARITA': una chiave che non esiste NON e' «coperta»
-- ============================================================================
--
-- Una funzione che ritornasse zero righe per una chiave inesistente renderebbe
-- VERDE ogni guard che le si appoggia: un'assenza di righe e' un'assenza di
-- controesempi, ed e' il modo in cui un guard mente senza fallire (la 0742 lo
-- dichiara e lo evita con un controllo di non-vacuita' scritto a mano).
--
-- Qui la non-vacuita' e' STRUTTURALE: la riga BASE e' sempre nel risultato, e
-- quando non esiste (o e' disattiva) l'esito e' `riga_assente` - che non e'
-- `presente`, quindi un guard che pretende «tutte presenti» fallisce
-- rumorosamente invece di passare a vuoto.
--
-- Le VARIANTI, al contrario, compaiono solo se esistono: una `.en` che non e'
-- mai stata scritta non e' un difetto, e pretenderla renderebbe rossa ogni
-- chiave che l'A/B lingua non ha ancora toccato.

BEGIN;

-- ─── 1. Il PERIMETRO: quali righe il runtime puo' servire per questa chiave ──
-- Gemello SQL di `nexus_types::chiavi_servibili`. Il criterio e' il PREFISSO,
-- cioe' un SOVRAINSIEME dell'elenco esatto che il Rust conosce, e la scelta e'
-- di polarita': una riga di troppo nel denominatore fa fallire un guard
-- rumorosamente, una in meno lo farebbe passare verde su un mandato non
-- aggiornato. Il sovrainsieme non e' teorico: `system.scale.assess.sizing` e'
-- un template a se' e non una variante di `system.scale.assess`.

-- IL PREFISSO NON SI CHIEDE A `LIKE`, e non e' un dettaglio di stile: le chiavi
-- di questa tabella contengono UNDERSCORE (`subagent.step_gatekeeper.base`), e
-- in `LIKE` l'underscore e' un JOLLY. La forma `key LIKE p_chiave || '.%'` —
-- quella che i guard scritti a mano usano oggi — accetta percio' anche chiavi
-- che differiscono in quei caratteri: un sovrainsieme piu' largo di quello
-- voluto, prodotto dallo stesso equivoco che ha generato lo «0 su 8» del 18/08.
-- `left(key, n) = prefisso` non ha un linguaggio di pattern e non ha nulla da
-- interpretare.

CREATE OR REPLACE FUNCTION prompt_chiavi_servibili(p_chiave TEXT)
RETURNS TABLE (prompt_key TEXT) AS $$
    SELECT key
      FROM nexus_prompt_templates
     WHERE is_active
       AND (key = p_chiave OR left(key, length(p_chiave) + 1) = p_chiave || '.')
     ORDER BY key;
$$ LANGUAGE sql STABLE;

COMMENT ON FUNCTION prompt_chiavi_servibili(TEXT) IS
  'Le righe ATTIVE che il runtime puo'' servire quando gli si chiede questa '
  'chiave: la base piu'' le sue varianti (mig 0726). Gemello SQL di '
  'nexus_types::chiavi_servibili — due implementazioni perche'' SQL e Rust non '
  'si chiamano, un solo criterio (test il_perimetro_sql_contiene_quello_rust).';

-- ─── 2. La COPERTURA di un blocco su quel perimetro ─────────────────────────
-- Nessun LIKE: uguaglianza su un nome di tag, dal criterio della 0744.
-- L'underscore non e' un jolly, la maiuscola non conta, e la variante `.en`
-- porta lo STESSO block_key col proprio testo tradotto — quindi i guard
-- bilingui su frammenti di prosa non servono piu' per questa domanda.

CREATE OR REPLACE FUNCTION prompt_copertura_blocco(p_blocco TEXT, p_chiave TEXT)
RETURNS TABLE (prompt_key TEXT, esito TEXT) AS $$
    -- La BASE c'e' sempre: se manca, l'esito lo dice invece di sparire.
    SELECT p_chiave,
           COALESCE(
               (SELECT CASE WHEN p_blocco = ANY (nexus_prompt_blocchi(t.content))
                            THEN 'presente' ELSE 'assente' END
                  FROM nexus_prompt_templates t
                 WHERE t.key = p_chiave AND t.is_active),
               'riga_assente')
    UNION ALL
    -- Le VARIANTI solo se esistono: una `.en` mai scritta non e' un difetto.
    SELECT t.key,
           CASE WHEN p_blocco = ANY (nexus_prompt_blocchi(t.content))
                THEN 'presente' ELSE 'assente' END
      FROM nexus_prompt_templates t
     WHERE t.is_active
       AND t.key <> p_chiave
       AND left(t.key, length(p_chiave) + 1) = p_chiave || '.'
     ORDER BY 1;
$$ LANGUAGE sql STABLE;

COMMENT ON FUNCTION prompt_copertura_blocco(TEXT, TEXT) IS
  'Per ogni riga SERVIBILE della chiave: il blocco c''e'' (presente), non c''e'' '
  '(assente), oppure la riga base non esiste o e'' disattiva (riga_assente). '
  'Nessun LIKE sul contenuto: uguaglianza su un nome di tag, criterio della mig '
  '0744. Da usare nei DO $$ delle migrazioni al posto di content LIKE ''%<tag>%''.';

-- ─── 3. La funzione e' ARMATA: provata qui, sullo schema vero ───────────────
-- Un guard che nessuno ha visto fallire non e' un guard. Le tre risposte si
-- esercitano al momento dell'installazione, in una sotto-transazione che non
-- lascia traccia — sul META di produzione come su ogni DB effimero dei test.

DO $prova$
DECLARE
    v TEXT;
    n INT;
BEGIN
    INSERT INTO nexus_prompt_templates (key, category, title, content, is_active)
    VALUES ('_prova_0745',      'system', 'base',     E'testa\n<c>x</c>\ncoda', TRUE),
           ('_prova_0745.en',   'system', 'variante', E'head\nno block here\ntail', TRUE);

    -- (a) la base porta il blocco, la variante no: la copertura le distingue.
    SELECT esito INTO v FROM prompt_copertura_blocco('c', '_prova_0745')
     WHERE prompt_key = '_prova_0745';
    IF v IS DISTINCT FROM 'presente' THEN
        RAISE EXCEPTION 'mig 0745: la base porta <c> ma la copertura dice %', v;
    END IF;
    SELECT esito INTO v FROM prompt_copertura_blocco('c', '_prova_0745')
     WHERE prompt_key = '_prova_0745.en';
    IF v IS DISTINCT FROM 'assente' THEN
        RAISE EXCEPTION 'mig 0745: la variante non porta <c> ma la copertura dice %', v;
    END IF;

    -- (b) LA PROVA DEL 18/08: l'underscore non e' un jolly.
    --     Un tag `a_b` non deve rispondere a un nome che differisce nel solo
    --     carattere che in LIKE sarebbe jolly.
    --     La riscrittura PERDE <c>, e il presidio della 0744 la rifiuterebbe se
    --     non la dichiarassimo: la dichiarazione vale per il resto della
    --     transazione, quindi copre anche la pulizia in fondo.
    PERFORM set_config('nexus.blocchi_rimossi', 'c,a_b', TRUE);
    UPDATE nexus_prompt_templates SET content = E'<a_b>x</a_b>' WHERE key = '_prova_0745';
    SELECT esito INTO v FROM prompt_copertura_blocco('a b', '_prova_0745')
     WHERE prompt_key = '_prova_0745';
    IF v IS DISTINCT FROM 'assente' THEN
        RAISE EXCEPTION 'mig 0745: <a_b> risponde al nome «a b»: il criterio e'' tornato lessicale';
    END IF;
    SELECT esito INTO v FROM prompt_copertura_blocco('a_b', '_prova_0745')
     WHERE prompt_key = '_prova_0745';
    IF v IS DISTINCT FROM 'presente' THEN
        RAISE EXCEPTION 'mig 0745: <a_b> non risponde al proprio nome (esito %)', v;
    END IF;

    -- (c) NON VACUITA': una chiave inesistente non e' «coperta».
    SELECT count(*) INTO n FROM prompt_copertura_blocco('c', '_chiave_che_non_esiste_0745');
    IF n <> 1 THEN
        RAISE EXCEPTION 'mig 0745: chiave inesistente -> % righe (attesa 1, o un guard passerebbe a vuoto)', n;
    END IF;
    SELECT esito INTO v FROM prompt_copertura_blocco('c', '_chiave_che_non_esiste_0745');
    IF v IS DISTINCT FROM 'riga_assente' THEN
        RAISE EXCEPTION 'mig 0745: chiave inesistente -> esito % (atteso riga_assente)', v;
    END IF;

    -- (d) il perimetro comprende la variante, e non si scrive a mano.
    SELECT count(*) INTO n FROM prompt_chiavi_servibili('_prova_0745');
    IF n <> 2 THEN
        RAISE EXCEPTION 'mig 0745: perimetro di _prova_0745 = % righe (attese 2: base + .en)', n;
    END IF;

    -- (e) e l'UNDERSCORE della CHIAVE non e' un jolly. Una riga che differisce
    --     proprio nel carattere che `LIKE` interpreterebbe non entra nel
    --     perimetro di un'altra: e' lo stesso equivoco dello «0 su 8», visto
    --     dal lato delle chiavi invece che da quello dei tag.
    INSERT INTO nexus_prompt_templates (key, category, title, content, is_active)
    VALUES ('Xprova_0745.en', 'system', 'omonima per jolly', 'niente', TRUE);
    SELECT count(*) INTO n
      FROM prompt_chiavi_servibili('_prova_0745')
     WHERE prompt_key = 'Xprova_0745.en';
    IF n <> 0 THEN
        RAISE EXCEPTION 'mig 0745: l''underscore della chiave e'' tornato un jolly: «Xprova_0745.en» e'' nel perimetro di «_prova_0745»';
    END IF;
    DELETE FROM nexus_prompt_templates WHERE key = 'Xprova_0745.en';

    -- Pulizia. La dichiarazione impostata sopra copre anche il DELETE, che per
    -- il presidio della 0744 e' una perdita a tutti gli effetti.
    DELETE FROM nexus_prompt_templates WHERE key IN ('_prova_0745', '_prova_0745.en');
    PERFORM set_config('nexus.blocchi_rimossi', '', TRUE);

    IF EXISTS (SELECT 1 FROM nexus_prompt_templates
                WHERE key IN ('_prova_0745', '_prova_0745.en')) THEN
        RAISE EXCEPTION 'mig 0745: le righe di prova non sono state rimosse';
    END IF;

    RAISE NOTICE 'mig 0745: copertura armata — presente/assente/riga_assente distinti, underscore non jolly, perimetro con variante';
END
$prova$;

COMMIT;
