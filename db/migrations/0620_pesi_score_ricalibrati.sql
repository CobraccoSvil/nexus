-- 0620 - I pesi seguono cio' che DISCRIMINA, e il peso morto sparisce
--
-- Due difetti misurati il 2026-07-20 sui dati di suite 6.
--
-- (A) LA CATENA E' DIVENTATA COMMODITY. Alzato il soffitto (mig 0618:
--     max_turns 6->8), 11 modelli su 18 sono saliti a 7,0 anelli di media -
--     inclusi ministral-8b-2512 e mistral-small-latest, cioe' i piccoli. Il
--     79% dei tentativi tocca il nuovo tetto esattamente come il 77% toccava
--     il vecchio. Il fix ha PROVATO la diagnosi (era il nostro tetto, non la
--     bravura: appena alzato sono saliti tutti) ma la componente e' satura di
--     nuovo. Seguire riferimenti concatenati non e' piu' una capacita' rara.
--     Continuare ad allungare la catena sarebbe una corsa senza fine che
--     misura la RESISTENZA, non la capacita': meglio ammettere che il test e'
--     diventato commodity e pesarlo per quello che dice ancora.
--
-- (B) UN PESO MORTO DA 15 PUNTI. `agentic_longctx` e' disabilitato dalla mig
--     0610 (40 evidenze su 40 inconclusive: non ha mai dato un verdetto), ma
--     `w_longctx = 15` e' rimasto nella formula. Quindici punti su cento erano
--     IRRAGGIUNGIBILI per costruzione, ed e' il motivo per cui il leader misura
--     esattamente 85,0 e non 100: la scala era compressa di un sesto da un
--     profilo che non gira. Non e' un dettaglio estetico - comprime le distanze
--     fra i modelli proprio nella zona alta, dove servono.
--
-- I PESI NUOVI seguono il potere discriminante osservato, non l'intuizione:
--   chain    25 -> 12   satura: quasi tutti al tetto (11/18 a 7,0 anelli)
--   recovery 30 -> 45   il solo che apre davvero il ventaglio: 0%-100% fra i
--                       modelli, ed e' cio' che separa deepseek-v4-pro (4/4)
--                       da grok-4.5 (0/4)
--   latent   15 -> 25   discrimina ancora (73% pass, non 100)
--   real     15 -> 18   quasi saturo (87%) ma resta il gradino di base
--   longctx  15 ->  0   profilo spento: un peso che nessuno puo' prendere non
--                       e' un criterio, e' una compressione
-- Totale 100, e ora il 100 e' RAGGIUNGIBILE.
--
-- EFFETTO ATTESO (da verificare sui dati, non da assumere): il ventaglio si
-- allarga in alto perche' i 15 punti morti tornano disponibili, e il recupero
-- pesa quasi meta' del punteggio. Un modello che passa tutti i test facili ma
-- non recupera si ferma a 55/100, dove prima faceva 55/85 = il 65% del leader.
--
-- SUITE 7, obbligatoria: cambiano i PESI, quindi il significato di ogni score
-- gia' persistito. Il leader delle bande si calcola solo fra righe della suite
-- corrente, e mescolare punteggi pesati in modo diverso falserebbe il confronto
-- in silenzio. Stesso principio del bump della 0618 (li' cambiava il test, qui
-- la formula): materiale diverso = versione nuova.

UPDATE settings SET value = '12' WHERE key = 'catalog.measured_score.w_chain';
UPDATE settings SET value = '45' WHERE key = 'catalog.measured_score.w_recovery';
UPDATE settings SET value = '25' WHERE key = 'catalog.measured_score.w_latent';
UPDATE settings SET value = '18' WHERE key = 'catalog.measured_score.w_real';
UPDATE settings SET value = '0'  WHERE key = 'catalog.measured_score.w_longctx';

UPDATE ai_model_probe_profile SET suite_version = 7 WHERE enabled;
