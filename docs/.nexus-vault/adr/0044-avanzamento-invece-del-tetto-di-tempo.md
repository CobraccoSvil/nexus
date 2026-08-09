---
id: adr-0044-avanzamento-invece-del-tetto-di-tempo
kind: adr
title: "ADR 0044 - Il criterio di terminazione di una figura e' il progresso, non il tempo"
slug: 0044-avanzamento-invece-del-tetto-di-tempo
tags:
  - adr
  - figure
  - consiglio
  - timeout
  - avanzamento
  - regola-H
  - regola-L
  - regola-M
  - regola-O
  - regola-Q
auto_generated: false
created_at: 2026-08-09T00:00:00Z
updated_at: 2026-08-09T00:00:00Z
nexus_meta_version: 1
---

# ADR 0044 - Il criterio di terminazione di una figura e' il progresso, non il tempo

## Stato

Accepted - 2026-08-09. Migrazione META `0687`. Punto unico
`crates/nexus-agent-graph/src/decisions/avanzamento_figura.rs`.

## Contesto

Una figura convocata dal Consiglio veniva fermata da un TETTO DI TEMPO FISSO per
kind (240s e 300s i valori misurati in esercizio). Il tetto risponde alla domanda
sbagliata: chiede *quanto tempo concedo a chiunque?* invece di *questa figura sta
producendo qualcosa?*.

### La misura (09/08/2026, progetto gestione-corsi)

Nove figure convocate. Cinque consegnano un parere, QUATTRO muoiono al tetto. Le
quattro fermate avevano prodotto rispettivamente **4, 5, 17 e 22 passi
persistiti**, e per tutte e quattro la causa dichiarata da
`decisions::timeout_cause` era la stessa:

```
budget esaurito su lavoro in corso (N passi osservati, l'ultimo non e' un fallimento)
```

cioe' `CausaTimeout::NoFailureAtEnd`. Nessuna era ferma: **stavano lavorando**. Il
tetto ha trattato identicamente chi aveva prodotto 4 passi e chi ne aveva
prodotti 22, perche' il numero che guardava non era nessuno dei due — era
l'orologio.

Vale la pena notare che la diagnosi c'era gia' ed era corretta: `timeout_cause`
(ADR 0026, mig 0686) nominava con precisione il fatto che quelle figure stessero
lavorando. Era una MISURA, per costruzione: non allunga budget e non riavvia
nulla. Il referto diceva il vero a chi lo leggeva il giorno dopo, e nel momento in
cui serviva non poteva fare niente.

### Perche' alzare il numero non e' il fix

La regola H elenca per nome l'aumento di timeout fra le toppe. Qui sarebbe anche
inefficace: non esiste un tetto fisso giusto per una figura che puo'
ragionevolmente finire in 30s o in 600s a seconda di cosa trova. La domanda
"quanto tempo concedo a chiunque?" non ha una risposta buona.

## Decisione

Il tempo smette di essere il CRITERIO e resta il BACKSTOP.

Nasce il punto unico `decisions/avanzamento_figura.rs` (regola L): dati i FATTI
PERSISTITI del run decide se merita ancora tempo.

| segnale | verdetto |
|---|---|
| una scrittura che cambia il contenuto di un file | avanza -> prosegue |
| un passo su una STRADA NUOVA (firma mai vista nel run) | avanza -> prosegue |
| nessuna delle due da `progresso_inattivita_max_s`, **e nel frattempo lavoro a vuoto osservato** | `no_progress` -> si ferma SUBITO |
| nessun fatto osservabile, o silenzio dall'ultimo avanzamento | ignoto DICHIARATO -> prosegue |
| eta' oltre il tetto assoluto | `absolute_ceiling` -> si ferma |

Il verdetto e' tipizzato (`Prosecuzione`), con tre varianti e non due: l'ignoto ha
un posto proprio (regola Q).

### Due punti unici RIUSATI, non ricopiati

- **il cambiamento di contenuto** e' `correction_progress::WriteFact::cambia_il_contenuto`
  (`before_sha256 != after_sha256` piu' il caso dei soli fine-riga della mig
  0680). Un secondo confronto degli hash darebbe due idee diverse di "ha scritto
  qualcosa".
- **l'identita' della strada** e' `loop_signatures::build_signature` (nome del
  tool + hash dell'input canonico), lo STESSO che l'executor usa per la
  rilevazione dei loop in memoria.

La granularita' della firma e' quella FINE di proposito. La firma grossolana di
`agent_tools::subagent_timeout` (tool + primo token del comando) risponde a
un'altra domanda — *stava ripetendo la stessa strada quando e' morto?* — e li' e'
giusta, perche' tre gestori di pacchetti diversi non sono una ripetizione. Qui
confonderebbe `npm test` con `npm run build` e direbbe "ripete" a una figura che
sta alternando due comandi diversi.

### Perche' le scritture da sole non bastano

Le quattro figure misurate sono ADVISORY: il loro prodotto e' un parere, non un
file, e non scrivono NULLA per costruzione. Un criterio che ammettesse solo
`file_mutations` le fermerebbe tutte al primo scrutinio — l'esatto contrario di
cio' che serve. Per questo la seconda prova (una strada mai tentata) e' di pari
dignita' e non un ripiego.

### Il silenzio non ferma nessuno, e non basta il tempo per fermare

Una figura dentro una chiamata al modello non lascia passi. Trattare quel
silenzio come stallo reintrodurrebbe il tetto a tempo sotto un altro nome, e piu'
corto per giunta: sarebbe il difetto misurato, peggiorato. `NonOsservabile` e'
percio' una variante dichiarata e prosegue; a coprirlo resta il solo tetto
assoluto, che si controlla PRIMA di tutto il resto proprio per questo.

La forma insidiosa dello stesso errore non e' il run senza alcun fatto — quello
si vede subito — ma il run che ha avanzato e **poi** tace. Li' la sottrazione fra
"ora" e "ultimo avanzamento" cresce, e un criterio scritto sulla sola soglia
temporale ucciderebbe la figura che sta aspettando il proprio turno di parlare col
fornitore: la sola attesa in coda vale 90 secondi
(`routing.inflight_queue_wait_max_s`, mig 0686), cioe' la soglia intera.

Per questo l'arresto richiede DUE condizioni congiunte: soglia superata **e**
lavoro a vuoto osservato da quando ha avanzato l'ultima volta. La sottrazione fra
due istanti si puo' sempre fare, il fatto no — ed e' l'errore piu' facile da
commettere scrivendo questo modulo. Senza il secondo termine il referto direbbe
`no_progress` con `passi_a_vuoto: 0`, cioe' dichiarerebbe di aver fermato una
figura per assenza di progresso senza avere un solo fatto da opporle.

Il conteggio del lavoro a vuoto parte dall'ULTIMO avanzamento e non
dall'inizio: una ripetizione gia' superata non e' una prova valida contro il
presente.

Il silenzio non resta comunque impunito. Lo copre il tetto assoluto; e il turno di
sola PROSA — l'altro modo di girare a vuoto senza lasciare passi, perche' un
blocco senza tool non entra fra le strade — ha gia' il suo presidio in
`gate_streak_solo_testo`, che non si duplica qui (regola L).

### Direzione dell'errore, dichiarata

Il criterio erra verso il PROSEGUIRE, e lo fa di proposito:

- una strada nuova conta anche se FALLISCE (scoprire che una strada e' chiusa e'
  informazione; e' la stessa asimmetria che `timeout_cause` gia' dichiara:
  "tentare alternative diverse non e' ripetere la stessa strada");
- il taglio delle letture scarta i passi piu' VECCHI, quindi una strada tentata
  molto tempo fa puo' tornare a sembrare nuova;
- un guasto della porta non ferma nessuno.

Fermare chi lavora e' il difetto MISURATO; lasciar lavorare chi non produce costa
tempo che il tetto limita comunque.

### Il tetto e' un MOLTIPLICATORE, non un secondo numero assoluto

I tetti per kind sono gia' dimensionati (una `verify` da 180s non ha le stesse
esigenze di una figura del Consiglio da 300s): un tetto assoluto unico li
appiattirebbe tutti e rifarebbe, un piano piu' in alto, l'errore che si sta
correggendo. Il moltiplicatore e' clampato a `>= 1` nel punto unico
`subagent_native::tetto_assoluto_s`: la configurazione puo' allargare, mai
stringere sotto il timeout della figura, o il difetto rientrerebbe da un'altra
porta.

## Architettura

```
agent_steps (DB progetto)  ─┐
                            ├─> AvanzamentoAdapter ─> AvanzamentoPort ─┐
file_mutations (DB META)   ─┘   (mcp-core, solo I/O)                   │
                                                                       v
                                       decisions::avanzamento_figura::decidi_prosecuzione
                                                    (puro, nessun DB)
                                                                       │
                                                                       v
                                       ExecutorNode::gate_deadline_run
                                        - Prosegue -> sollecito a tempo invariato
                                        - Ferma    -> sollecito one-shot, poi close_runaway
```

### Dove si innesta, e perche' li'

Nel gate di deadline dell'executor, cioe' **dentro** il run e non nel
`tokio::time::timeout` esterno che lo avvolge. Un supervisore esterno puo' solo
CANCELLARE un future: chiuderebbe la figura senza che nessuno le abbia mai
chiesto di dichiarare quello che ha trovato. Il gate interno chiude PULITO, e
soprattutto passa dal turno di grazia.

Il `tokio::time::timeout` esterno resta armato e sale al tetto assoluto: copre
l'unico caso che il gate non puo' vedere — un run wedged DENTRO una singola
chiamata al modello, che non raggiunge mai un confine di iterazione e quindi non
viene mai interrogato.

### Il sollecito precede la chiusura in ENTRAMBI i rami

Il turno di grazia al canale di ruolo era agganciato al solo tempo
(`time_grace_pct` del budget). Col criterio nuovo una figura puo' fermarsi molto
PRIMA di quella soglia — nel caso misurato il 70% del tetto e' 672s, e l'arresto
per ripetizione arriva a 160s — e senza il secondo aggancio morirebbe muta
proprio nel caso che il criterio esiste per cogliere. Un arresto passa percio'
comunque per `maybe_advisory_grace_delta`, che e' one-shot: al giro successivo il
gate chiude davvero e non si cicla.

## Cosa NON e'

Non e' il `progress_controller`, che risponde a *di fronte a uno stallo, qual e'
la prossima MOSSA?* (guida, cambia strategia, escala il modello) sulle firme in
memoria del turno. Questo decide **se si continua a lavorare**, sui fatti
persistiti dell'intero run — sub-run delegati compresi, perche' le scritture sono
filtrate per sessione come in `mutation_progress`. Finche' la seconda domanda
aveva come unica risposta un numero di secondi, la prima non poteva salvare
nessuna delle quattro figure misurate.

## Configurazione (mig 0687, regola G)

| chiave | default | significato |
|---|---|---|
| `orchestrator.progresso_inattivita_max_s` | `90` | secondi senza avanzamento oltre cui si ferma. `0` = criterio spento, governa il solo tetto (via di ritorno senza redeploy) |
| `orchestrator.progresso_tetto_moltiplicatore` | `4` | per quanto il tetto assoluto eccede il timeout della figura. Clampato a `>= 1` |

## Conseguenze

- Una figura che avanza puo' vivere fino a 4x il tetto di prima. Costa piu'
  tempo per run, e produce un parere invece di niente: nel caso misurato il
  costo del tetto vecchio era il 100% di quattro figure su nove.
- Una figura che ripete muore in ~90s invece che in 240-300s: il criterio e' piu'
  severo del tetto, non piu' permissivo.
- Il `reason` di chiusura cambia vocabolario: `time_budget` ->
  `no_progress` | `absolute_ceiling`. Il nome cambia perche' e' cambiato cio' che
  dichiara.
- Il run PRIMARIO non e' toccato: `agent.run_time_budget_s` vale 0 per policy
  (mig 0604/0607), quindi il gate non e' mai raggiunto. Il criterio governa le
  sole figure, che sono l'oggetto del difetto.

## Verifica (regola O)

Quattro mutazioni provate, tutte rosse col valore del difetto:

0. **arresto sulla sola soglia temporale** (togliere `&& ha_lavorato_a_vuoto`, la
   forma "naturale" del criterio) -> `il_silenzio_dopo_un_avanzamento_non_ferma_nessuno`
   e `il_lavoro_a_vuoto_si_conta_dopo_l_ultimo_avanzamento` rosse, con
   `NonAvanza { passi_a_vuoto: 0, riscritture: 0 }` — la firma esatta di un
   arresto senza prove.

1. **tetto fisso di 240s come criterio** (il difetto originale) ->
   `una_figura_che_avanza_non_si_ferma_al_vecchio_tetto` rossa.
2. **ogni passo conta come strada nuova** (novita' ignorata) ->
   `chi_ripete_la_stessa_strada_si_ferma_molto_prima_del_tetto` rossa.
3. **l'executor smette di interrogare la porta** ->
   `una_figura_che_avanza_supera_il_tempo_che_la_uccideva` e
   `una_figura_che_ripete_si_ferma_prima_del_tetto` rosse.

I test attraversano i produttori: la firma dei passi nasce da `build_signature`
(mai una stringa scritta a mano), le righe di `agent_steps` da
`PgAgentStepStore::persist_step` sullo schema reale della migrazione, non da una
INSERT del test.

Una divergenza e' stata trovata dal test durante la scrittura, non dopo:
`#[serde(rename_all = "snake_case")]` sui nomi italiani delle varianti produceva
`non_avanza` sul wire mentre `key()` diceva `no_progress` — due nomi per la stessa
cosa (regola N), su un campo che e' anche il `reason` di chiusura. I `rename` sono
ora espliciti.

## Riferimenti

- `crates/nexus-agent-graph/src/decisions/avanzamento_figura.rs` - il criterio
- `crates/nexus-agent-graph/src/runtime/ports.rs` - `AvanzamentoPort`
- `crates/mcp-core/src/agent_graph_adapter/avanzamento.rs` - l'I/O
- `crates/nexus-agent-graph/src/nodes/executor.rs` - `gate_deadline_run`
- `crates/mcp-core/src/agent_tools/subagent_native.rs` - `tetto_assoluto_s`
- `db/migrations/0687_avanzamento_invece_del_tetto.sql`
- ADR 0026 (catalogo punti unici), mig 0686 (`timeout_cause`, il referto)
