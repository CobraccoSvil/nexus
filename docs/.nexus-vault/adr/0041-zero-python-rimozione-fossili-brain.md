# ADR 0041 — Zero-Python: rimozione dei fossili del brain

Data: 2026-07-17
Stato: accettata, applicata (mig 0609)

## Contesto

Il brain — servizio Python/LangGraph (REST su `:8001`, gRPC su `:50051`) — era
stato eliminato da tempo: servizio fermato (mig 0462), settings orfane droppate
(0463), cutover del motore versionato (0532), nessun file `.py` nel repo. Il
porting a Rust era considerato finito.

Non lo era. Un censimento a 6 lenti con verifica avversaria (519 finding grezzi,
35 confermati vivi, 5 refutati) ha trovato che il brain era ancora **chiamato a
runtime da codice raggiungibile dal flusso utente normale**.

La causa non era una svista isolata, ma un pattern: **i rami di ripiego verso il
brain erano sopravvissuti al brain**. Un fallback verso un servizio che non
esiste non e' una rete di sicurezza — e' un buco in cui cadere, e maschera il
difetto vero dietro un degrado silenzioso.

### Il caso esemplare: una race da 1,4 secondi

`mcp-core` sondava il gateway UNA volta all'avvio (`is_healthy()`) e congelava
l'esito per la vita del processo:

```
21:06:25.614  mcp-core: "Nexus Gateway non raggiungibile -> uso PATH B"
21:06:26.980  nexus-gateway: "connesso a PostgreSQL"
```

Il gateway aveva finito di nascere **1,4s dopo la probe**. Esito: `nexus_gateway
= None` per sempre, col gateway che rispondeva `200` da ore. A valle: il ramo
Rust del classificatore non partiva nemmeno col setting `classifier_engine='rust'`
-> ripiego sul ramo Python -> brain assente -> `classifier_resolved=false` -> il
dimensionamento dell'orchestrazione (ADR 0040) restava al piano legacy **senza
dire perche'**. Due sessioni ci hanno perso una diagnosi sbagliata a testa.

## Decisione

### 1. La disponibilita' di un servizio si osserva, non si deduce (regola M)

`Orchestrator.nexus_gateway` non e' piu' `Option`: e' un parametro obbligatorio
del costruttore. Senza gateway non esiste modo di chiamare un LLM, quindi
"orchestrator senza gateway" non era uno stato valido — era solo **indicibile**.
Ora il TIPO lo rende impossibile. Se il gateway e' giu', lo dice la singola
chiamata che fallisce, e al tentativo dopo puo' essere di nuovo su.

### 2. Un'alternativa che non puo' funzionare va rimossa, non tenuta "per sicurezza"

Rimossi perche' morti, non perche' scomodi:

| Cosa | Righe | Perche' |
|---|---|---|
| loop di retry Python in `spawn_agent_run` | 668 | il failover cross-provider vive nel motore nativo (`nodes/executor.rs`), osservato su un run reale |
| `execute_via_neural` ("PATH B: Brain gRPC") | 185 | il brain non c'e'; il gateway ora e' obbligatorio |
| `run_via_brain` + resume SSE + `brain_rest_url` | 501 | nessun backend |
| blocco SHADOW | 89 | confrontava l'ombra Rust col primario **Python** |
| `classify_intent_async_with_threshold` + gemella | 252 | POST a `{BRAIN_REST_URL}/classify-intent-agentic` |
| enum `Engine`, `select_engine`, `ClassifierEngine` | — | scelte con una sola opzione viva |

### 3. I consumatori legittimi si portano al gateway, non si buttano

Due funzioni **rotte ma legittime** chiamavano ancora il brain e fallivano in
silenzio: il tool `nexus_visual_compare` (dispatchabile da qualunque agente) e
`call_prompt_revise` (il `PromptOptimizerWorker` gira ogni 30 minuti, attivo di
default, e scartava ogni variante). Rimuoverle sarebbe stato piu' rapido, ma
avrebbe tolto due funzioni che il prodotto dichiara di avere: sono state portate
al gateway Rust (regola H — la causa, non il sintomo).

Punto unico introdotto: `nexus_types::gateway_client::gateway_text_complete`
(regola L). La completion testuale via gateway per i crate fuori da mcp-core
esisteva gia' in admin-service: una seconda copia sarebbe divergente per
costruzione. Ora admin-service vi delega.

### 4. La configurazione che sopravvive al servizio e' un innesco (mig 0609)

`settings.brain_rest_url` era seedata a `http://127.0.0.1:8001` e **non vuota**:
faceva superare a ogni call site il proprio guard `filter(|v| !v.is_empty())` e
sparare HTTP reale a un servizio morto. Con lei: `routing.classifier_engine` (il
valore `'python'` restava accettato — un UPDATE di "rollback" dall'aria innocua
spegneva la classificazione), `neural_core_url`, `brain_log_level` (zero
lettori), i sudo purpose `brain-restart`/`brain-stop`/`brain-disable` (ancora
`enabled=true`, `systemctl` su un servizio inesistente) e la tabella
`nexus_orchestrator_engine`.

### 5. Una firma che mente si porta dietro lavoro inutile

`NeuralCoreClient::connect(url)` accettava un URL, lo **ignorava** e non falliva
mai. Attorno a quella firma erano cresciuti un setting, una env var, tre lettori
e un **retry-loop da 60 tentativi (~9 minuti dichiarati) che ritentava una
funzione infallibile**. Diventata `NeuralCoreClient::new()`, tutto quel lavoro e'
sparito da solo.

## Cosa NON e' stato toccato

- **Il supporto ai progetti Python DEGLI UTENTI** (`resolver_python.rs`,
  `mcp-ast`, `detector.rs`, `wizard.rs`): e' una funzione viva del prodotto.
  Confonderla col brain sarebbe stato il danno peggiore di questa pulizia.
- **`neural_compat.rs`**: non e' un fossile, e' la reimplementazione Rust degli
  endpoint che il brain esponeva.
- **`agent_runs.engine`**: la colonna resta e serve al recovery; i run storici
  conservano il valore che avevano davvero.
- **I commenti storici** che spiegano una parita' col vecchio motore ("parita'
  col path Python" in `event_sink.rs`): hanno valore documentale. Sono stati
  riscritti solo i commenti che descrivevano il PRESENTE in modo **ora falso**
  (es. `core.rs`: "path vivo INVARIATO" del ramo python; `neural_client.rs`:
  "restano sul gRPC al brain gli RPC batch/model-sync").

## Conseguenze

- Il classificatore risolve (`engine="rust"`), quindi il dimensionamento
  dell'orchestrazione (ADR 0040) non e' piu' inerte.
- La race di avvio non puo' ripresentarsi: non c'e' piu' una probe da vincere.
- `-4.000` righe circa di codice irraggiungibile; baseline quality scesa di
  ~100 finding.
- Zero-Python anche nel tooling: nessuno script del repo richiede piu' un
  interprete python3 (portati a node), e sei script non sondano piu' `:8001`
  chiamandola "brain" — quella porta oggi e' di presidio-stub/vllm, e
  identificare un servizio dalla porta viola la regola M.

## Lezioni

1. **Un fallback verso un servizio rimosso non e' inerte.** Sembra codice morto,
   ma e' raggiungibile: cattura i casi che il ramo vivo non copre e li fa fallire
   in silenzio. `handlers.rs` chiamava `run_via_brain` senza gate: ogni
   "riprendi" scritto in chat finiva li'.
2. **La configurazione e' il carburante.** Finche' `brain_rest_url` esisteva e
   non era vuota, i guard dei call site passavano. Rimuovere il codice senza
   rimuovere la config lascia il difetto pronto a ripartire.
3. **Il nome di un modulo puo' mentire per anni.** `brain_agent_client.rs` era
   vivo al 90% e non parlava col brain: costruiva i tool del turno. Rinominato
   `agent_turn_setup.rs`.
4. **Un modulo di test puo' contenere piu' di quanto dichiara.** Rimuovendo
   `mod tests_select_engine` sono stati buttati con lui 11 test `native_mapping_*`
   validi che ci abitavano dentro; il conteggio (1235 -> 1220 invece di 1231) lo
   ha smascherato. Ripristinati.

## Verifica

1231 test passati, 0 falliti. Migrazione 0609 provata in transazione sul DB vivo
con ROLLBACK (4 settings + 3 sudo purpose + 1 tabella rimossi, poi DB intatto).
Gate quality al ribasso a ogni commit. Script `.sh` eseguiti davvero dopo il
port a node (equivalenza verificata su caso nominale, JSON malformato e chiave
assente); `tsc --noEmit` e `pnpm lint` puliti sul frontend.
