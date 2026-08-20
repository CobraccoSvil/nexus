-- Migrazione 0747: le prove del piano di verifica hanno un budget di TEMPO,
-- non un tetto sul NUMERO.
--
-- ── PERCHE' LA 0746, DI IERI SERA, E' GIA' SUPERATA ──────────────────────────
--
-- La 0746 (19/08/2026, notte) ha portato `agent.final_gate.piano_max_prove` da
-- 6 a 24, e la sua misura era giusta: un Consiglio emette ~21 prove (misurato
-- su t4-prove-consiglio), quindi col tetto a 6 quindici prove su ventuno non
-- venivano eseguite e il referto dichiarava il piano superato avendone provate
-- meno di un terzo.
--
-- Quella migrazione ha corretto il VALORE. Non poteva correggere il difetto,
-- perche' il difetto era il MECCANISMO: 6 e 24 sono entrambi arbitrari.
--
-- Il tetto sul numero esisteva per UNA sola ragione, e la 0737 la dichiara
-- esplicitamente nella propria descrizione: «`piano_max_prove` da' l'attesa
-- massima di un giro di gate: 6 x 45 = 270s». Cioe' era un modo INDIRETTO di
-- limitare il TEMPO, ottenuto moltiplicando due numeri — e il tempo si puo'
-- misurare direttamente.
--
-- Le conseguenze del surrogato, entrambe visibili nei dati:
--
--   * con 21 prove SANE — su t4 tutte curl verso un servizio locale e grep
--     sull'albero, tutte sotto il secondo — il tetto a 6 ne escludeva quindici
--     senza che nessuna avesse consumato un millisecondo di attesa;
--   * col tetto a 24 UNA sola prova bloccata poteva far durare la verifica 8
--     minuti (24 x 20s), e nessuno dei due numeri diceva quanto si stesse
--     effettivamente aspettando.
--
-- Col budget di tempo il criterio diventa «esegui finche' hai budget»: venti
-- prove veloci girano tutte, una prova che si blocca consuma il budget e ferma
-- le altre. E' il comportamento voluto, ottenuto senza inventare un numero di
-- prove.
--
-- ── IL VALORE, E DA QUALE MISURA VIENE ──────────────────────────────────────
--
--   agent.final_gate.piano_budget_tempo_s = 120
--
-- Le due quantita' misurate il 19/08/2026 su t4-prove-consiglio:
--   * 21 prove emesse da un Consiglio;
--   * ogni prova reale (curl locale, grep sull'albero) sotto il secondo.
--
-- Quindi 120 s coprono le 21 prove sane con circa cinque volte il margine,
-- oppure sei prove COMPLETAMENTE bloccate (6 x 20 s) prima che il gate smetta.
-- Il caso peggiore per invocazione scende da 480 s (24 x 20) a 120 s, e non e'
-- piu' un prodotto da ricalcolare quando uno dei due fattori cambia.
--
-- `agent.final_gate.prova_timeout_s` RESTA a 20 s e non e' ridondante: senza,
-- una singola prova bloccata si prenderebbe l'intero budget prima che il gate
-- possa provare la successiva. E' un bound sull'attesa SINGOLA; il budget e' un
-- bound sul TOTALE. L'attesa applicata e' comunque tagliata sul residuo, cosi'
-- l'ultima prova ammessa non puo' far sforare il budget.
--
-- ── COSA IL TETTO SUL NUMERO COPRIVA, E CHE ORA NON E' PIU' COPERTO ──────────
--
-- NULLA, ed e' verificabile:
--
--   * il TEMPO: e' precisamente cio' che il budget misura, meglio;
--   * la MEMORIA (un piano da migliaia di prove): non era coperto nemmeno
--     prima. `esegui_le_ammesse` accumula un esito per ogni prova DICHIARATA,
--     comprese quelle non eseguite, quindi il tetto non riduceva l'occupazione.
--     Il bound reale e' a monte ed e' strutturale: `tool_dispatch::inserisci_prove`
--     tronca a ADVISORY_LIST_CAP (30) le prove di OGNI parere, e il fan-out del
--     Consiglio e' limitato da `resolve_orchestration_plan` — poche centinaia
--     di prove nel caso estremo, non migliaia;
--   * la taglia della CONVOCAZIONE ai giudici (il prompt cresce col piano:
--     6489 token per 25 prove): non era coperto prima e non lo e' ora. Il tetto
--     si applicava all'ESECUZIONE (passo 5), DOPO che la convocazione era gia'
--     partita — lo dichiara la 0746 stessa: «COSA QUESTA MIGRAZIONE NON FA: non
--     tocca il tetto sulla CONVOCAZIONE». Resta un lotto a se': il rimedio non
--     e' un tetto sul numero di prove giudicate (riproporrebbe lo stesso
--     difetto altrove) ma governare la taglia del batch inviato ai giudici.
--
-- ── ROLLBACK ────────────────────────────────────────────────────────────────
--
-- `agent.final_gate.piano_verifica_enabled = false` spegne il criterio, come
-- prima. Un budget molto alto lo rende di fatto illimitato; un budget a 0 fa
-- dichiarare `time_budget_exhausted` su ogni prova, che e' fail-closed e
-- coerente col resto del criterio.

BEGIN;

INSERT INTO settings (key, value, description, updated_at)
VALUES (
    'agent.final_gate.piano_budget_tempo_s',
    '120',
    'Budget di TEMPO (secondi) di UNA invocazione del criterio piano_di_verifica: si eseguono prove finche'' ce n''e'', e la prima che trova il budget esaurito resta dichiarata con causa time_budget_exhausted. Sostituisce agent.final_gate.piano_max_prove (rimosso dalla mig 0747): il tetto sul NUMERO esisteva solo per limitare il tempo, e il tempo si misura direttamente. Tarato su due misure del 19/08/2026 (t4-prove-consiglio): 21 prove emesse da un Consiglio, ognuna sotto il secondo (curl locale, grep sull''albero). 120 s coprono le 21 prove sane con ampio margine, oppure sei prove completamente bloccate. Va letto INSIEME a agent.final_gate.prova_timeout_s, che limita l''attesa su UNA prova.',
    NOW()
)
ON CONFLICT (key) DO UPDATE
   SET value = EXCLUDED.value,
       description = EXCLUDED.description,
       updated_at = NOW();

-- Il tetto sul numero non ha piu' un lettore: lasciarlo sarebbe una seconda
-- verita' su «quanto puo' durare la verifica», da allineare a mano e destinata
-- a divergere (regola G).
DELETE FROM settings WHERE key = 'agent.final_gate.piano_max_prove';

DO $guard$
DECLARE
    v_budget NUMERIC;
    v_timeout NUMERIC;
    v_residuo INT;
BEGIN
    SELECT value::NUMERIC INTO v_budget
      FROM settings WHERE key = 'agent.final_gate.piano_budget_tempo_s';
    SELECT value::NUMERIC INTO v_timeout
      FROM settings WHERE key = 'agent.final_gate.prova_timeout_s';

    IF v_budget IS NULL THEN
        RAISE EXCEPTION 'mig 0747: la chiave del budget di tempo non risulta scritta';
    END IF;
    IF v_timeout IS NULL THEN
        RAISE EXCEPTION 'mig 0747: agent.final_gate.prova_timeout_s non esiste. '
                        'La crea la 0737: se manca, il set non e'' allineato.';
    END IF;

    -- Il budget deve valere ALMENO qualche prova bloccata, o la prima prova che
    -- non risponde spegnerebbe la verifica per tutte le altre.
    IF v_budget < v_timeout * 3 THEN
        RAISE EXCEPTION 'mig 0747: budget % s troppo stretto rispetto all''attesa per prova '
                        '(% s): non basterebbe a tre prove bloccate.', v_budget, v_timeout;
    END IF;

    SELECT count(*) INTO v_residuo
      FROM settings WHERE key = 'agent.final_gate.piano_max_prove';
    IF v_residuo <> 0 THEN
        RAISE EXCEPTION 'mig 0747: il tetto sul numero di prove e'' ancora presente';
    END IF;
END
$guard$;

COMMIT;
