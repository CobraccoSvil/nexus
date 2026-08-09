-- 0692: il tetto dei rimandi del gate duale FERMA il run, invece di contare a vuoto
--
-- ROOT CAUSE
-- La sera del 09/08/2026, prima serata con il gate in `enforce` (mig 0689), un
-- run di riparazione ha ripetuto NOVE volte la stessa sequenza:
--   1. write_file di uno script di correzione  -> passa (mutatore ordinario)
--   2. run_command che lo esegue               -> `unconfined`, pavimento
--      Critical (mig 0688), il gate convoca
--   3. il gate non autorizza, il batch torna al modello col motivo
--   4. il modello scrive un ALTRO script
-- Nomi prodotti, nell'ordine: apply_fixes.js, final_fix.js, complete_fix.js,
-- batch_fix.js, final_batch_fix.js, ... In autonomia nessun umano sblocca: il
-- ciclo finiva col budget, non con un esito.
--
-- I fornitori NON mancavano (openai/perplexity/anthropic esclusi per credito;
-- deepseek, google, groq, mistral, openrouter, kimi disponibili) e la selezione
-- NON ha fallito: il testo del rimando conteneva la coda «nessun validatore
-- indipendente disponibile», che e' il DEFAULT di `report.degraded` — quindi il
-- degrado era NULL, cioe' il ramo di `seleziona_convocati` che dichiara DUE
-- provider distinti trovati. Due giudici erano stati convocati; uno ha risposto
-- e l'altro no. La condizione di uscita della tier-chain era gia' stata
-- corretta la mattina stessa (commit c83c7f27: il veto sull'esecutore entra
-- nell'ELEGGIBILITA').
--
-- Il difetto era la CONSEGUENZA, ed erano due meta' dello stesso buco:
--   (a) l'impossibilita' di ottenere un giudizio veniva trattata come un
--       rifiuto del singolo passo. Ma un quorum mancante non e' una proprieta'
--       del passo: e' una condizione dell'ambiente, e riproporre lo stesso
--       lavoro con un altro nome di file non la cambia. Il modello non aveva
--       modo di saperlo, perche' nessun campo glielo diceva.
--   (b) il tetto `critical_step_max_rejections` esisteva e NON agiva: si
--       calcolava sui soli `Rejected` (qui i verdetti erano approve+astensione,
--       quindi non si calcolava mai) e, quando scattava, degradava la decisione
--       a `NeedsHuman` — che in autonomia torna a essere lo STESSO rimando.
--       Contava fino a due e poi non produceva alcuna conseguenza diversa.
--
-- COSA CAMBIA NEL CODICE (nessun valore da modificare qui)
--   - `decisions::step_gate::GateBlock` (nuovo punto unico) risponde «di che
--     natura e' il blocco, e ripetere puo' cambiarlo»:
--       step_rejected     = un validatore ha espresso un verdetto contrario;
--                           rimando al modello, che puo' cambiare strada
--       not_judgeable     = solo astensioni: il gate non ha giudicato; rimando
--                           DICHIARATO come condizione d'ambiente
--       retries_exhausted = tetto raggiunto; il run si ferma
--   - il tetto vale ora per OGNI rimando del gate, qualunque ne sia la causa;
--   - al tetto, dove un umano c'e' (Conferma/Studio) si sospende come sempre;
--     in autonomia il run si CHIUDE dichiarando outcome `blocked` + blocker
--     `safety` (regola D: non una domanda, una dichiarazione terminale).
--
-- IL FAIL-CLOSED NON SI ALLENTA: il passo critico non veniva eseguito prima e
-- non viene eseguito ora, in nessuna delle tre nature. Cambia solo CHI riceve
-- la conseguenza, il passo o il run.
--
-- OSSERVABILITA': il payload del meta_step `step_validation` porta il campo
-- `block` con la natura (NULL quando il batch passa), accanto a `cap_reached`,
-- che mantiene il nome storico per le query di taratura gia' scritte.
--
-- ROLLBACK / TARATURA (a caldo, cache settings TTL 60s): alzare il valore
-- concede piu' rimandi prima della chiusura; NON esiste un valore che
-- ripristini il ciclo infinito, ed e' deliberato.
--   UPDATE settings SET value = '3' WHERE key = 'orchestrator.critical_step_max_rejections';

UPDATE settings
   SET description = 'Rimandi massimi del gate duale in UN run, contati su OGNI '
                     'blocco del gate qualunque ne sia la causa (rifiuto giudicato '
                     'o quorum mancante). Raggiunto il tetto il gate non rimanda '
                     'piu'': dove un umano c''e'' (Conferma/Studio) sospende coi '
                     'verdetti allegati, in autonomia CHIUDE il run dichiarando '
                     'outcome blocked + blocker safety. Prima si calcolava sui soli '
                     'Rejected e degradava a NeedsHuman, che in autonomia tornava a '
                     'essere lo stesso rimando: contava e non fermava nulla (nove '
                     'ripetizioni misurate il 09/08/2026). Punto unico: '
                     'decisions::step_gate::classify_block -> GateBlock (mig 0692).',
       updated_at = NOW()
 WHERE key = 'orchestrator.critical_step_max_rejections';

DO $$
DECLARE
  v_tetto TEXT;
BEGIN
  SELECT value INTO v_tetto
    FROM settings
   WHERE key = 'orchestrator.critical_step_max_rejections';

  IF v_tetto IS NULL THEN
    RAISE NOTICE '0692: chiave critical_step_max_rejections ASSENTE: il gate usa il default di codice. Verificare la mig 0677 che la introduce.';
  ELSE
    RAISE NOTICE '0692: tetto dei rimandi = % (invariato: cambia la conseguenza, non il numero).', v_tetto;
  END IF;
END $$;
