-- 0623 - La batteria copre il parco in ore, non in un giorno
--
-- PERCHE' ORA. Il ridisegno della catena (mig 0622, suite 8) ha rimesso in gioco
-- 118 modelli, ma il giro ne prende 4 per volta ogni 30 minuti: circa 15 ore per
-- coprire il parco. Nel frattempo il verdetto sul ridisegno resta appeso a 4
-- modelli - e su 4 modelli il 62% al tetto non si distingue dal rumore, perche'
-- basta un modello in piu' a spostarlo di 15 punti. Non e' un problema di
-- pazienza: e' che una misura arrivata troppo tardi non serve a decidere, e la
-- decisione che aspetta (risaturato? pavimento? funziona?) governa il prossimo
-- passo del test.
--
-- IL VALORE 10, non di piu'. Ogni modello costa 7 profili x 4 tentativi = 28
-- chiamate LLM, e i due profili multi-step arrivano a 8 turni ciascuno: un round
-- da 10 modelli e' ~280 chiamate ogni 30 minuti. Con 12 il carico cresce senza
-- cambiare l'ordine di grandezza del tempo di copertura (~4h contro ~5h), con 4
-- resta un giorno. Dieci copre il parco in circa 5 ore mantenendo il round entro
-- una finestra che il worker chiude comodamente.
--
-- COSA NON TOCCO, e perche'. `model_health_probe_interval_s` resta 1800: l'altra
-- leva sarebbe stringere l'intervallo, ma i round piu' fitti sovrappongono le
-- chiamate ai provider e avvicinano il rate limit - il collo di bottiglia si
-- sposterebbe su `provider:transient`, cioe' su giri inconclusivi che non
-- misurano nulla. Meglio piu' modelli per round che piu' round.
--
-- REVERSIBILE E TEMPORANEO: questo e' un valore da campagna, non un default per
-- sempre. Coperto il parco a suite 8, riportarlo a 4 e' un UPDATE di una riga -
-- e va fatto, perche' a regime la batteria deve ri-misurare per scadenza (TTL),
-- non rincorrere un bump.

UPDATE settings
   SET value = '10'
 WHERE key = 'agent.model_qualification.max_models_per_round';
