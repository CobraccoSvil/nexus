-- Migrazione 0746: il tetto delle prove del piano e' tarato su quante prove
-- un Consiglio ne emette davvero.
--
-- MISURATO il 19/08/2026 sul progetto t4-prove-consiglio (primo run in cui le
-- figure hanno emesso prove eseguibili invece di sola prosa):
--
--   SELECT sum(json_array_length(verdict->'advisory'->'prove'))
--     FROM nexus_subagent_runs;
--   -> 21 prove da 6 figure
--
--   agent.final_gate.piano_max_prove = 6
--
-- Quindici prove su ventuno non sarebbero state eseguite, e il referto avrebbe
-- dichiarato il piano superato avendone provate meno di un terzo. Il valore 6
-- fu scelto quando NESSUNA figura emetteva prove (mig 0737, quando il canale
-- era appena nato e il volume reale era zero): non era una misura, era una
-- stima prudente su un fenomeno non ancora osservato.
--
-- IL PRODOTTO E' IL NUMERO CHE CONTA, e va dichiarato:
--   24 prove x 20 s = 480 s (8 minuti) di tetto teorico per invocazione.
-- Prima era 6 x 45 = 270 s. Il tetto cresce da 4,5 a 8 minuti nel caso
-- PEGGIORE, che e' quello in cui ogni prova va in timeout.
--
-- Il timeout per prova scende da 45 a 20 secondi perche' le prove reali sono
-- curl verso un servizio locale e grep sull'albero: MISURATE sul run di T4,
-- tutte sotto il secondo. Quarantacinque secondi non servivano a nessuna prova
-- sana — servivano solo a far pagare di piu' quelle bloccate.
--
-- COSA QUESTA MIGRAZIONE NON FA: non tocca il tetto sulla CONVOCAZIONE. Il
-- batch inviato ai giudici resta senza cap (limite dichiarato dalla review del
-- 20/08: il prompt cresce col piano, 6489 token per 25 prove), ed e' un lotto
-- a se'.

BEGIN;

UPDATE settings
   SET value = '24',
       description = 'Massimo di prove del piano di verifica eseguite in una invocazione del final gate. Tarato sul volume reale: un Consiglio ne emette ~21 (misurato il 19/08/2026 su t4-prove-consiglio). Col tetto precedente (6) quindici prove su ventuno restavano non eseguite e il referto dichiarava il piano superato avendone provate meno di un terzo. Il PRODOTTO con agent.final_gate.prova_timeout_s e'' il tetto di tempo per invocazione: 24 x 20 = 480 s.',
       updated_at = NOW()
 WHERE key = 'agent.final_gate.piano_max_prove';

UPDATE settings
   SET value = '20',
       description = 'Attesa massima del gate su UNA prova del piano. Le prove reali sono curl verso un servizio locale e grep sull''albero, tutte sotto il secondo (misurato su t4-prove-consiglio, 19/08/2026): i 45 s precedenti non servivano a nessuna prova sana, servivano solo a far pagare di piu'' quelle bloccate. E'' un bound sull''ATTESA DEL GATE, non sul processo: il cap del processo lo applica il tool runner.',
       updated_at = NOW()
 WHERE key = 'agent.final_gate.prova_timeout_s';

DO $guard$
DECLARE
    v_max INT;
    v_timeout INT;
BEGIN
    SELECT value::INT INTO v_max FROM settings WHERE key = 'agent.final_gate.piano_max_prove';
    SELECT value::INT INTO v_timeout FROM settings WHERE key = 'agent.final_gate.prova_timeout_s';

    IF v_max IS NULL OR v_timeout IS NULL THEN
        RAISE EXCEPTION 'mig 0746: una delle due chiavi non esiste (max=%, timeout=%). '
                        'La 0737 le crea entrambe: se manca, il set non e'' allineato.',
                        v_max, v_timeout;
    END IF;

    -- Il tetto deve coprire il volume MISURATO (21) con margine, o la
    -- migrazione non ha fatto cio' che dichiara.
    IF v_max < 21 THEN
        RAISE EXCEPTION 'mig 0746: il tetto (%) non copre le 21 prove misurate su un '
                        'Consiglio reale: il referto dichiarerebbe superato un piano '
                        'provato in parte.', v_max;
    END IF;

    -- Il PRODOTTO e' il numero operativamente rilevante e non deve superare i
    -- 10 minuti: oltre, un gate bloccato costa piu' del run che verifica.
    IF v_max * v_timeout > 600 THEN
        RAISE EXCEPTION 'mig 0746: il tetto di tempo per invocazione e'' % s (% prove x % s), '
                        'oltre i 600 s ammessi.', v_max * v_timeout, v_max, v_timeout;
    END IF;

    RAISE NOTICE 'mig 0746: tetto prove % (>= 21 misurate), attesa % s, prodotto % s',
                 v_max, v_timeout, v_max * v_timeout;
END
$guard$;

COMMIT;
