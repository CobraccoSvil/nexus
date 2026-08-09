-- 0687 — Il tetto di tempo di una figura rispondeva alla domanda sbagliata.
--
-- ROOT CAUSE (misurata il 09/08/2026 su gestione-corsi). Il Consiglio convoca
-- nove figure: cinque consegnano un parere, QUATTRO muoiono al tetto di tempo
-- del proprio kind (240s e 300s, i valori in esercizio). Le quattro fermate
-- avevano prodotto rispettivamente 4, 5, 17 e 22 passi persistiti, e per tutte
-- e quattro la causa dichiarata da `decisions::timeout_cause` era la stessa:
--
--   "budget esaurito su lavoro in corso (N passi osservati, l'ultimo non e' un
--    fallimento)"
--
-- cioe' `CausaTimeout::NoFailureAtEnd`. Nessuna era ferma: STAVANO LAVORANDO.
-- Il tetto ha trattato identicamente chi aveva prodotto 4 passi e chi ne aveva
-- prodotti 22, perche' il numero che guardava non era nessuno dei due — era
-- l'orologio.
--
-- ALZARE IL NUMERO NON E' IL FIX. La regola H elenca per nome l'aumento di
-- timeout fra le toppe, e qui sarebbe anche inefficace: non esiste un tetto
-- fisso giusto per una figura che puo' ragionevolmente finire in 30s o in 600s
-- a seconda di cosa trova. La domanda "quanto tempo concedo a chiunque?" non ha
-- una risposta buona; quella giusta e' "questa figura sta producendo qualcosa?".
--
-- COSA CAMBIA. Nasce il punto unico
-- `nexus-agent-graph/src/decisions/avanzamento_figura.rs` (regola L): dati i
-- FATTI PERSISTITI del run — i passi di `agent_steps` e le scritture di
-- `file_mutations` — decide se merita ancora tempo. Il criterio e' il PROGRESSO:
--
--   * avanza (una scrittura che cambia il contenuto, oppure un passo su una
--     STRADA NUOVA mai tentata in questo run) -> prosegue, anche molto oltre il
--     vecchio tetto;
--   * non avanza da `progresso_inattivita_max_s` -> si ferma SUBITO, molto prima
--     del vecchio tetto: chi ripete la stessa chiamata muore in un minuto e
--     mezzo invece che in quattro minuti;
--   * non si e' potuto osservare -> prosegue DICHIARANDOLO. Una figura dentro
--     una chiamata al modello non lascia passi, e trattare il suo silenzio come
--     stallo reintrodurrebbe il tetto a tempo sotto un altro nome, piu' corto per
--     giunta (regola Q: l'ignoto e' una variante, mai un degrado).
--
-- Il tempo resta SOLO come backstop: il tetto assoluto, che non e' piu' il
-- timeout della figura ma un suo multiplo.
--
-- PERCHE' UN MOLTIPLICATORE E NON UN SECONDO NUMERO ASSOLUTO. I tetti per kind
-- sono gia' dimensionati (una `verify` da 180s non ha le stesse esigenze di una
-- figura del Consiglio da 300s): un tetto assoluto unico li appiattirebbe tutti
-- e rifarebbe, un piano piu' in alto, esattamente l'errore che si sta
-- correggendo. Il moltiplicatore e' clampato a >= 1 nel codice
-- (`subagent_native::tetto_assoluto_s`, punto unico): la configurazione puo'
-- allargare, mai stringere sotto il timeout della figura, o il difetto
-- rientrerebbe da un'altra porta.
--
-- PERCHE' LE SCRITTURE NON BASTANO COME PROVA. Le quattro figure misurate sono
-- ADVISORY: il loro prodotto e' un parere, non un file, e non scrivono NULLA per
-- costruzione. Un criterio che ammettesse solo `file_mutations` le fermerebbe
-- tutte al primo scrutinio — l'esatto contrario di cio' che serve. Per questo la
-- seconda prova (una strada mai tentata) e' pari dignita' e non un ripiego.
--
-- DIREZIONE DELL'ERRORE, DICHIARATA. Il criterio erra verso il PROSEGUIRE: una
-- strada nuova conta anche se fallisce (scoprire che una strada e' chiusa e'
-- informazione), il taglio delle letture scarta i passi piu' VECCHI (una strada
-- tentata molto tempo fa puo' tornare a sembrare nuova), un guasto di lettura
-- non ferma nessuno. E' voluto: fermare chi lavora e' il difetto MISURATO,
-- mentre lasciar lavorare chi non produce costa tempo che il tetto limita.
--
-- COSA NON E'. Non e' il `progress_controller`, che risponde a "di fronte a uno
-- stallo, qual e' la prossima MOSSA?" (guida, cambia strategia, escala il
-- modello) sulle firme in memoria del turno. Questo decide se si continua a
-- lavorare, sui fatti persistiti dell'intero run. Finche' la seconda domanda
-- aveva come unica risposta un numero di secondi, la prima non poteva salvare
-- nessuna delle quattro figure misurate.

INSERT INTO settings (key, value, description, category)
VALUES (
    'orchestrator.progresso_inattivita_max_s',
    '90',
    'Secondi senza un AVANZAMENTO oltre i quali una figura si ferma, anche col tetto '
    'assoluto lontano. Avanzare significa aver cambiato il contenuto di un file oppure '
    'aver tentato una strada mai tentata prima in quel run: il criterio guarda i fatti '
    'persistiti (agent_steps, file_mutations), mai la prosa del modello. E'' la meta'' '
    'SEVERA del criterio — chi ripete la stessa chiamata muore qui, molto prima del '
    'tetto. Novanta secondi e'' il punto in cui una chiamata al modello lenta ha gia'' '
    'restituito almeno un passo: piu'' corto misurerebbe la latenza invece del progresso. '
    '0 = criterio di progresso SPENTO, governa il solo tetto assoluto (via di ritorno al '
    'comportamento a tempo senza redeploy).',
    'orchestrator'
),
(
    'orchestrator.progresso_tetto_moltiplicatore',
    '4',
    'Per quanto il TETTO ASSOLUTO di una figura eccede il suo timeout. Il timeout del '
    'kind smette di essere la condanna a morte e diventa l''intervallo di lavoro atteso: '
    'chi AVANZA puo'' eccederlo fino a questo multiplo, chi non avanza si ferma molto '
    'prima per la soglia di inattivita''. Quattro e'' il margine che copre una figura del '
    'Consiglio che trova piu'' materiale del previsto senza rendere il backstop teorico. '
    'Clampato a >= 1 nel codice: un valore sotto 1 renderebbe il tetto nuovo piu'' stretto '
    'di quello di oggi, cioe'' farebbe rientrare il difetto che il criterio chiude.',
    'orchestrator'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;
