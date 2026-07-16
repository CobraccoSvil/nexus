# ADR 0040 — Orchestrazione dimensionata dal problema

Data: 2026-07-16
Stato: accettata, attiva (mig 0602-0607)

## Contesto

Nexus aveva tre dei quattro pattern di orchestrazione multi-agente, ma tutti
governati da **cap fissi nel DB**:

| Pattern | Prima | Governato da |
|---|---|---|
| Fan-out di ricognizione | attivo | `subagent_batch_max_tasks` (1-32) |
| Panel di giudici (consiglio) | attivo | `council_max_figures` = 6 |
| Confutatori (review adversariale) | attivo | `review_panel_size` = 2 |
| Tesi contrapposte | **assente** | — |

Tre difetti strutturali:

1. **Il problema non contava.** Un refactor di una riga e una riprogettazione
   dell'architettura convocavano lo stesso numero di figure. Il segnale
   `complexity` del classificatore esisteva dal porting ma era dead-code:
   nessun call site lo consumava per dimensionare.
2. **Il motore misurava il dissenso, non sapeva provocarlo.** `advisory_panel`
   rilevava che le figure divergevano e dava il veto alla minoranza con
   evidenza, ma tutte ricevevano lo stesso task e nessun prompt diceva
   "argomenta contro". Su una decisione architetturale (A vs B), il consenso di
   sei lenti concordi non prova che l'alternativa sia peggiore: nessuno l'ha
   difesa.
3. **Il consiglio bloccava l'avvio.** Pre-step sincrono: fino a ~300+300s prima
   che l'agente leggesse un file. Ma la ricognizione non ha bisogno del parere
   del consiglio — ha bisogno del repo. Solo la scrittura ha bisogno del
   verdetto.

## Decisione

Il **problema** decide, entro un **budget**; i cap storici diventano backstop.

### 1. Resolver di dimensionamento (mig 0602)

`decisions/orchestration_sizing.rs`, puro e replay-stabile (gemello di
`scale_reason`):

```
DOMANDA (profilo per-classe, admin)  vs  OFFERTA (costo e tempo residui)
                     -> vince il piu' stretto, dichiarato in `sized_by`
```

- **Domanda**: `orchestrator.sizing_profile_low|medium|high` (JSON), editabile
  dalla pagina admin Dimensionamento. Non una derivazione hardcoded.
- **Offerta**: doppio vincolo. `affordable_by_cost` = quota del budget residuo
  / costo unitario stimato (modello risolto VIA TIER, prezzo da `nexus-pricing`;
  prezzo ignoto = nessun vincolo, mai prezzo zero). `affordable_by_time` =
  (tempo residuo / durata attesa) x parallelismo.
- **Degrado** a due passi: verso i floor in ordine inverso di priorita', poi
  fit-first. Un panel sotto il proprio floor va a **0, mai monco**: un panel da
  1 con `min_valid=2` e' inconclusivo garantito, cioe' spesa certa e informazione
  nulla (lezione mig 0589).
- `sized_by` e' un segnale strutturato (regola M): il meta-step
  `orchestration_plan` dice QUALE vincolo ha deciso, non lo si deduce.

### 2. Deadline di run (mig 0604)

Terzo asse del budget accanto a token e dollari.
`AgentState.run_started_at_epoch_s` e' **checkpointato**: misura il run intero,
non l'ultimo spezzone dopo un resume. Enforcement gemello del cap di spesa,
reason canonico `time_budget`. Punto unico del residuo:
`run_time_remaining_s`, derivato dal DB — lo usano il clamp del timeout
sub-run, il piano pre-run e la review.

### 3. Tesi contrapposte (mig 0605)

Il gap architetturale colmato. `decisions/debate_panel.rs`:

- **Innesco strutturato**: il consiglio dichiara `contested_decision{topic,
  options[]}` dentro `advisory_verdict` (non il classificatore: ha un contratto
  1:1 congelato e gira su ogni turno, anche di chat pura).
- **Assegnazione**: `plan_debate` round-robin; le opzioni oltre il numero di
  avvocati sono **tagliate**, mai lasciate indifese (un'opzione attaccata da
  tutti e difesa da nessuno perderebbe senza essere stata discussa).
- **Il segnale piu' forte e' la resa**: `stance=oppose` significa "ho provato a
  difendere la mia tesi e non regge". Con evidenza `alta` squalifica l'opzione
  anche in minoranza — il veto della minoranza-con-evidenza, coerente con gli
  altri panel.
- **Attribuzione POSIZIONALE** (regola M): `outcomes[i]` e' il sub-run di
  `assignments[i]`; la posizione difesa e' un fatto strutturale deciso da noi,
  non una stringa che il modello ricopia. Un'eco divergente e' scartata e
  **contata** (`misattributed`), mai persa in silenzio.
- **Quorum del confronto**: servono >= 2 opzioni con almeno una voce
  (`MIN_OPTIONS_HEARD`), altrimenti `inconclusive`. Un "dibattito" in cui parla
  solo il difensore di A e' l'ipotesi nulla, non una vittoria di A.

### 4. Overlap con write barrier (mig 0606)

Il run parte subito; i panel deliberano in parallelo. Il primo tool **mutativo**
attende la barriera (`watch::channel`, gate nel `ToolDispatchNode` gemello del
gate HITL, stesso punto unico `hitl::is_mutator_tool_name`):

- `Released` -> si scrive, coi requisiti iniettati come promemoria nello stesso
  turno (l'unico momento in cui il modello puo' tenerne conto).
- `Vetoed` -> stop **prima della prima modifica**, riusando l'edge esistente
  `terminal_panel_veto`: zero routing nuovo.
- timeout / canale chiuso -> il run **prosegue dichiarandolo**. Una barriera che
  attende per sempre sarebbe peggio del problema che risolve; e un'assenza di
  verdetto non e' un verdetto favorevole.

Il timeout e' clampato alla deadline residua: altrimenti un run che aspetta il
consiglio verrebbe chiuso con reason `time_budget`, e la causa vera sparirebbe
dietro un sintomo.

### 5. Figure creabili da admin (fasi 6-7)

`POST /api/admin/orchestrator/figures`: i **4 pezzi** di una figura viva
(prompt, purpose tier-only, definition, whitelist CSV) creati in **una
transazione**. Prima la UI ne creava uno solo e il kind restava muto. La
selezione modello resta **sempre per tier**: `provider=''`/`model_id=''`, il
modello lo sceglie `best_model_for_tier` dal catalog a ogni convocazione.

## Punti unici introdotti (regola L)

| Concern | Modulo |
|---|---|
| Dimensionamento dei panel | `decisions/orchestration_sizing.rs` |
| Tesi contrapposte | `decisions/debate_panel.rs` |
| Vocabolario gravita' (alta/media/bassa) | `decisions/severity.rs` |
| Vocabolario performance-tier | `nexus-types/src/tiers.rs` |
| Whitelist kind CSV | `admin-service/figures.rs::mutate_kinds_whitelist` |
| Residuo della deadline | `subagent_native::run_time_remaining_s` |
| Panel a monte (i 2 rami) | `agent_run::run_upstream_panels` |

Duplicazioni **assorbite** strada facendo: il test "evidenza grave" era scritto
a mano in `advisory_panel` e `adversarial_review` (il debate sarebbe stata la
terza copia); `VALID_FINDING_SEVERITIES` ora deriva da `Severity::as_str`;
la normalizzazione dei `risks` era duplicata fra advisory e debate; il turno di
grazia era legato al solo `advisory_verdict` ed e' ora "canale di ruolo non
ancora usato".

## Conseguenze

- **Positive**: il costo dell'orchestrazione segue il valore del problema; un
  dibattito vero su decisioni vere; il tempo di attesa iniziale crolla (la
  ricognizione parte subito); nuove lenti si creano dalla UI senza migrazione.
- **Da sorvegliare**: il resolver si fida della stima unitaria del sub-run
  (`sizing.est_subrun_tokens/duration_s`, oggi settings). Il raffinamento
  naturale e' la telemetria reale dei sub-run: la firma pura non cambia, cambia
  solo il loader.
- **Rischio noto**: a `advisory_overlap_enabled=true` il modello parte senza il
  blocco del consiglio nel prompt. Se il consiglio veta, il lavoro di
  ricognizione gia' fatto viene buttato — ma nessun file e' stato toccato.

## Verifica

Golden test puri per resolver e debate (inclusi i casi che riproducono la mig
0589 in versione dibattito); 6 test della barriera (read-only non attende, la
scrittura attende e riceve i vincoli, il veto non lascia toccare un file, il
timeout non blocca mai, il sender droppato non appende, flag OFF bit-identico);
18 test dei validatori figure; SQL della whitelist verificato contro Postgres
vivo in transazione + rollback.

**La verifica avversaria della fase 4** (4 lenti indipendenti + 2 confutatori
per finding) ha trovato che il dibattito **non si sarebbe convocato mai**:
`decision_detected` era cablato a `false` perche' il piano si risolve prima del
consiglio, ma quel segnale nasce DAL consiglio. Tutti i test erano verdi col
codice irraggiungibile. Lezione registrata: **compilare e passare i test non
dice che il percorso sia raggiungibile**.
