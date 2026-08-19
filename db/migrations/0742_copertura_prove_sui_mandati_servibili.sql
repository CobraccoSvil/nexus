-- ─────────────────────────────────────────────────────────────────────────────
-- 0742 — La richiesta delle PROVE copre le righe SERVIBILI, e lo dichiara
--
-- NUMERO: massimo effettivo sul disco alla stesura = 0741 (prenotazione porta,
-- stesso lotto). Prendo la 0742, la prima libera.
--
-- LA 0737 HA MORSO, e va detto subito perche' la premessa contraria e' quella
-- che ha innescato questo lotto. MISURATO sul META vivo il 19/08/2026:
--   SELECT count(*) FILTER (WHERE content LIKE '%<prove_eseguibili>%')
--   FROM nexus_prompt_templates
--   WHERE key LIKE 'subagent.%' AND is_active AND content LIKE '%advisory_verdict%'
--   -> 8 su 8 (functional_analyst, program_manager, project_manager,
--      provider_analyst, security_engineer, software_architect, sysadmin,
--      ui_ux_designer), tutte con version incrementata.
-- La misura che diceva «0 su 27» usava `ILIKE '%prove eseguibili%'` con uno
-- SPAZIO, contro un marcatore `<prove_eseguibili>` con l'UNDERSCORE — in LIKE
-- `_` e' un jolly, lo spazio no. Le figure le prove le hanno anche EMESSE: 31,
-- ben formate, in sei chiamate ad `advisory_verdict` del run 5de631f9. Il
-- difetto vero era a valle, nei due normalizzatori che le scartavano al confine
-- del tool, e si chiude nel codice (vedi `decisions::tool_dispatch`).
--
-- QUESTA MIGRAZIONE CHIUDE L'ALTRA META', che nessuno aveva coperto: la 0737
-- NON aveva un guard. La sua copertura la verificava un solo test Rust, che
-- gira sullo schema che il MIGRATOR produce — cioe' applica la 0737 e poi
-- controlla che la 0737 sia stata applicata. E' verde per costruzione, e sulla
-- copertura del DB VIVO non dice niente: e' esattamente «un test che gira su
-- una fixture che contiene gia' il frammento cercato» (regola O).
--
-- DUE DIFETTI DEL PERIMETRO DELLA 0737, entrambi silenziosi:
--
-- (a) Decideva PER CONTENUTO, riga per riga: `content LIKE '%advisory_verdict%'`.
--     E' un'euristica su cio' che il testo NOMINA, non sul RUOLO della riga.
--     Una riga di mandato la cui prosa non contenga quel letterale — una
--     traduzione, una riscrittura — sarebbe stata saltata IN SILENZIO, e il
--     conteggio di copertura fatto con lo stesso criterio l'avrebbe pure
--     dichiarata «non pertinente». Un elenco che si assolve da solo.
--
-- (b) Ignorava il PERIMETRO SERVIBILE. Dal 17/08/2026 (mig 0726, A/B sulla
--     lingua) una chiave puo' avere una riga gemella `<chiave>.en`, e quale
--     delle due il runtime serva lo decide il CSV `prompt.english_variants`,
--     che e' un UPDATE senza redeploy. E' lo stesso difetto che la 0739 ha
--     scoperto sui due mandati del gate duale: aggiornata la sola riga
--     italiana, in produzione i giudici leggevano la `.en` intatta, con la
--     migrazione verde. Oggi nessuna figura advisory ha una `.en` (MISURATO:
--     le sole righe `subagent.%.en` vive sono i due giudici del gate), quindi
--     qui non c'e' nulla da riparare — e proprio per questo il rimedio va
--     scritto ORA: quando la variante arrivera', nessuno ripercorrera' questa
--     catena.
--
-- IL PERIMETRO E' DERIVATO, MAI CONTATO A MANO. La forma SQL e' il PREFISSO
-- (`key = K OR key LIKE K || '.%'`), sovrainsieme dichiarato di
-- `nexus_types::chiavi_servibili`: il SQL non puo' leggere il Rust, e la
-- polarita' del sovrainsieme e' quella giusta — una riga di troppo nel
-- denominatore fa fallire il guard rumorosamente, una in meno lo farebbe
-- passare verde su un mandato non aggiornato. Il lato Rust del guard usa il
-- criterio esatto (test `la_copertura_delle_prove_si_misura_sui_servibili`).
--
-- LA LINGUA LA SCEGLIE IL SUFFISSO, non un default: appendere il blocco
-- italiano dentro un prompt inglese e' peggio del blocco mancante, perche' il
-- guard risulterebbe soddisfatto.

UPDATE nexus_prompt_templates t
SET content = content || E'\n<prove_eseguibili>\n'
        || 'Where one of your requirements can be established by a COMMAND, emit it as '
           'an EXECUTABLE PROOF in the `prove` field of advisory_verdict instead of as a '
           'sentence: description + command + expectation. The expectation is ONE of exit '
           'code (exit_code), text present in the output (output_contains), text absent '
           'from the output (output_not_contains): if you need two, declare two proofs.'
        || E'\n'
        || 'The final verification RUNS them for real and judges the outcome mechanically: '
           'you propose the proof, the machine issues the verdict. A requirement in prose '
           'stays admissible and stays useful to whoever reviews the code, but nobody can '
           'execute it: measured, out of 89 requirements emitted only one was verifiable.'
        || E'\n'
        || 'Every proof must be runnable on its own, repeatable and NON-destructive. A '
           'proof classified as irreversible is not executed at all, and every other one '
           'first passes two independent judges: write assertions, not actions.'
        || E'\n</prove_eseguibili>\n',
    version = version + 1,
    updated_at = NOW()
WHERE t.is_active
  AND t.key LIKE '%.en'
  AND t.content NOT LIKE '%<prove_eseguibili>%'
  AND EXISTS (
      SELECT 1 FROM nexus_prompt_templates f
       WHERE f.is_active
         AND f.key LIKE 'subagent.%'
         AND f.content LIKE '%advisory_verdict%'
         AND t.key LIKE f.key || '.%');

UPDATE nexus_prompt_templates t
SET content = content || E'\n<prove_eseguibili>\n'
        || 'Dove un tuo requisito e'' accertabile con un COMANDO, emettilo come PROVA '
           'ESEGUIBILE nel campo `prove` di advisory_verdict, invece che come frase: '
           'descrizione + comando + attesa. L''attesa e'' UNA sola fra codice d''uscita '
           '(exit_code), testo presente nell''output (output_contains), testo assente '
           'dall''output (output_not_contains): se te ne servono due, dichiara due prove.'
        || E'\n'
        || 'La verifica finale le ESEGUE davvero e ne giudica l''esito in modo meccanico: '
           'tu proponi la prova, la macchina emette il verdetto. Un requisito in prosa '
           'resta ammesso e resta utile a chi rivede il codice, ma nessuno lo puo'' '
           'eseguire: misurato, su 89 requisiti emessi uno solo era verificabile.'
        || E'\n'
        || 'Ogni prova deve essere eseguibile da sola, ripetibile e NON distruttiva. Una '
           'prova classificata irreversibile non viene eseguita affatto, e ogni altra '
           'passa prima da due giudici indipendenti: scrivi accertamenti, non azioni.'
        || E'\n</prove_eseguibili>\n',
    version = version + 1,
    updated_at = NOW()
WHERE t.is_active
  AND t.key NOT LIKE '%.en'
  AND t.content NOT LIKE '%<prove_eseguibili>%'
  AND EXISTS (
      SELECT 1 FROM nexus_prompt_templates f
       WHERE f.is_active
         AND f.key LIKE 'subagent.%'
         AND f.content LIKE '%advisory_verdict%'
         AND (t.key = f.key OR t.key LIKE f.key || '.%'));

DO $$
DECLARE
    v_totale INT;
    v_figure INT;
    v_mancanti TEXT;
BEGIN
    -- Il perimetro: le righe che il runtime PUO' servire come mandato di una
    -- figura che emette `advisory_verdict`. La riga BASE decide chi e' una
    -- figura advisory (li' il letterale c'e' per costruzione: e' il nome del
    -- tool nel suo protocollo); le SERVIBILI sono quella riga piu' le sue
    -- varianti, ed e' li' che l'invariante deve valere.
    CREATE TEMP TABLE mandati_prove_servibili ON COMMIT DROP AS
    SELECT t.key, t.content
      FROM nexus_prompt_templates t
     WHERE t.is_active
       AND EXISTS (
           SELECT 1 FROM nexus_prompt_templates f
            WHERE f.is_active
              AND f.key LIKE 'subagent.%'
              AND f.content LIKE '%advisory_verdict%'
              AND (t.key = f.key OR t.key LIKE f.key || '.%'));

    SELECT COUNT(*) INTO v_totale FROM mandati_prove_servibili;
    SELECT COUNT(*) INTO v_figure
      FROM nexus_prompt_templates
     WHERE is_active AND key LIKE 'subagent.%' AND content LIKE '%advisory_verdict%';

    -- Non vacuita': un perimetro vuoto renderebbe verde ogni verifica che
    -- segue, perche' un'assenza di righe e' un'assenza di controesempi. E' la
    -- forma in cui questo guard potrebbe mentire senza fallire.
    IF v_figure = 0 THEN
        RAISE EXCEPTION 'mig 0742: nessuna figura advisory trovata (perimetro vuoto): il guard sarebbe vacuo e la copertura non verificata';
    END IF;

    -- COPERTURA: ogni riga servibile chiede le prove, o quella figura
    -- continuera' a produrre prosa che nessuno puo' eseguire. Il tag e' un
    -- identificatore del canale e non e' tradotto.
    SELECT string_agg(key, ', ' ORDER BY key) INTO v_mancanti
      FROM mandati_prove_servibili
     WHERE content NOT LIKE '%<prove_eseguibili>%';
    IF v_mancanti IS NOT NULL THEN
        RAISE EXCEPTION 'mig 0742: % righe servibili su % non chiedono le prove eseguibili [%]: quelle figure emetteranno requisiti in prosa e il piano di verifica restera'' vuoto',
            (SELECT COUNT(*) FROM mandati_prove_servibili WHERE content NOT LIKE '%<prove_eseguibili>%'),
            v_totale, v_mancanti;
    END IF;

    -- E il testo deve dire COSA vale la pena dichiarare, non solo che esiste un
    -- campo: senza il vocabolario delle attese la figura non sa che cosa puo'
    -- chiedere alla macchina di accertare. Marcatori BILINGUI, perche' una
    -- riga `.en` porta la stessa regola tradotta.
    SELECT string_agg(key, ', ' ORDER BY key) INTO v_mancanti
      FROM mandati_prove_servibili
     WHERE NOT (content ILIKE '%exit_code%' AND content ILIKE '%output_contains%'
                AND content ILIKE '%output_not_contains%');
    IF v_mancanti IS NOT NULL THEN
        RAISE EXCEPTION 'mig 0742: il vocabolario delle attese manca in [%]: la figura sa che esiste un campo, non che cosa puo'' chiedere di accertare', v_mancanti;
    END IF;

    RAISE NOTICE 'mig 0742: % righe servibili di % figure advisory chiedono le prove eseguibili', v_totale, v_figure;
END $$;
