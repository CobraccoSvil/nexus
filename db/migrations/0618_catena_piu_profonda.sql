-- 0618 - La catena torna a discriminare: piu' turni, bersaglio piu' alto, pass piu' severo
--
-- Misurato il 2026-07-20 sui 156 tentativi di `agentic_chain` a suite 5:
--
--   links=0   9 tentativi        links=3    5
--   links=1   1                  links=4   20
--   links=2   1                  links=5  120   <- il 77%, esattamente il soffitto
--
-- Il profilo passava al 100% dei modelli e la componente di score era satura:
-- 5 anelli davano il punteggio pieno e 120 tentativi su 156 li toccavano. Quel 5
-- non e' la bravura dei modelli: e' `max_turns: 6`. Con sei turni non se ne
-- concatenano di piu', e il mondo NON c'entra — `pianta_prossimo` genera l'anello
-- successivo all'infinito. Stavamo misurando il nostro tetto, non i modelli:
-- e' la stessa forma dell'errore che porto' `agentic_longctx` a chiedere 100k
-- caratteri e a non decidere mai niente.
--
-- Conseguenza a valle: chi supera i profili facili ha gia' la maggioranza dei
-- punti, e il vertice si affolla (frontier al 43% del parco prima della mig
-- 0617). Il taglio della 0617 ha reso il vertice usabile subito; questa
-- migrazione toglie la causa.
--
-- LE TRE LEVE, coerenti fra loro (una sola non basterebbe):
--   max_turns 6 -> 8      alza il soffitto (8 e' il massimo che TURNI_MAX
--                         concede: il clamp esiste per contenere il costo e non
--                         lo tocchiamo). Rende raggiungibili ~7 anelli.
--   LINKS_TARGET 5 -> 7   (codice, non setting: cambia il significato degli score
--                         persistiti, per questo va con un bump di suite). Chi si
--                         ferma a 5 prende il 71% invece del 100%.
--   min_chained_calls 3 -> 5   il PASS smette di accontentarsi di 3 anelli quando
--                         quasi tutti ne fanno 5. Chi ne fa 3-4 non passa piu'.
--
-- COSTO DICHIARATO: +33% di turni sui profili multi-step (6 -> 8), e il bump di
-- suite fa rigirare la batteria su tutto il parco eleggibile. E' il prezzo di una
-- misura che discrimina invece di promuovere tutti.
--
-- SUITE 6, obbligatoria: cambia il MATERIALE del test (turni, bersaglio, soglia),
-- quindi i punteggi vecchi non sono confrontabili coi nuovi — e il leader delle
-- bande measured si calcola solo fra righe della suite corrente. E' il pattern
-- dichiarato da tau2-bench: cambio materiale = versione nuova, non retrocompatibile.

-- (1) Piu' turni: senza, il bersaglio a 7 sarebbe irraggiungibile e avremmo
--     costruito il difetto opposto (una banda vuota per design).
UPDATE ai_model_probe_profile
   SET payload = payload || '{"max_turns": 8}'::jsonb
 WHERE profile_key IN ('agentic_chain', 'agentic_recovery');

-- (2) Il pass non si accontenta piu' di 3 anelli.
UPDATE ai_model_probe_profile
   SET pass_predicate = pass_predicate || '{"min_chained_calls": 5}'::jsonb
 WHERE profile_key = 'agentic_chain';

-- (3) Il bump che rende i nuovi punteggi comparabili solo fra loro.
UPDATE ai_model_probe_profile
   SET suite_version = 6
 WHERE enabled;
