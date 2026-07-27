-- 0624 - La batteria torna a regime: il valore da campagna rientra
--
-- La mig 0623 aveva alzato il batch a 10 modelli per round, e l'aveva
-- dichiarato per quello che era: un valore DA CAMPAGNA, per coprire il parco
-- dopo il bump a suite 8 in ore invece che in un giorno. La campagna ha fatto
-- il suo lavoro: la copertura ha permesso di certificare il sistema (vedi
-- docs/.nexus-vault/architecture/certificazione-sistema-tier-2026-07-20.md)
-- con la batteria che discrimina su ~33 modelli.
--
-- A regime il batch a 10 e' uno spreco: ~280 chiamate LLM ogni mezz'ora per
-- ri-misurare modelli che scadono col TTL (30 giorni), non per rincorrere un
-- bump. Il ritmo giusto della manutenzione e' 4.
--
-- TIMING, dichiarato: al momento della stesura restavano 78 modelli da coprire
-- a suite 8 (34 fatti). Applicare questa migrazione PRIMA della copertura
-- completa non rompe nulla - la campagna prosegue a passo 4 invece che 10, e
-- finisce in ~10 ore invece di ~4. Se un futuro bump di suite richiedesse una
-- nuova campagna, la leva e' la stessa: UPDATE temporaneo a 10, con la sua
-- migrazione di rientro gia' scritta accanto.

UPDATE settings
   SET value = '4'
 WHERE key = 'agent.model_qualification.max_models_per_round';
