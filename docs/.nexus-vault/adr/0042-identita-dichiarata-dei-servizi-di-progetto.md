# ADR 0042 — Identita' dichiarata dei servizi di progetto

Data: 2026-08-02
Stato: accettata, non ancora implementata. Il passo 0 e' bloccante e puo' ancora
cambiare la forma della decisione (vedi "Il perno da misurare prima di spendere").

Sottotitolo operativo: **il manifesto dichiara, il kernel prova, l'osservatore
registra**. Tre domande — chi e' questo servizio, questo processo e' suo, questa
porta e' sua — che oggi hanno cinque risposte a testa e domani ne hanno una.

## Contesto — nove identita' per due servizi

Misurato il 02/08/2026 sul progetto `bacheca-attivita`
(`66f4bf72-3975-4bb0-bc38-5e1107bf1d94`), DB meta `localhost:5433`, tabella
`nexus_port_allocations`. L'app ha **due** servizi reali: un frontend Vite e un
backend Express. Il registro ne conteneva **nove**, tutte
`allocation_mode='adopted'`, nate in sei momenti diversi nell'arco di tre giorni:

| porta | label | service_unit | nata |
|---|---|---|---|
| 24804 | `frontend dev` | `bacheca-attivita-frontend dev.service` | 31/07 10:15 |
| 24826 | `Service` | `bacheca-attivita-backend.service` | 31/07 10:15 |
| 24806 | `frontend-preview` | `bacheca-attivita-frontend.service` | 31/07 19:50 |
| 24802 | `service-66f4bf72` | `bacheca-attivita-service-66f4bf72.service` | 31/07 19:56 |
| 24827 | `bacheca-attivita-backend` | `bacheca-attivita-backend.service` | 01/08 13:02 |
| 24805 | `frontend` | `bacheca-attivita-frontend.service` | 01/08 18:41 |
| 24843 | `bacheca-attivita-frontend dev.service` | `bacheca-attivita-bacheca-attivita-frontend dev.service.service` | 01/08 20:57 |
| 24807 | `bacheca-attivita-frontend` | `bacheca-attivita-bacheca-attivita-frontend.service` | 01/08 23:09 |
| 24828 | `backend` | `bacheca-attivita-backend.service` | 02/08 07:10 |

In ascolto davvero, al momento della misura: **tre** (24802, 24804, 24828). Sei
righe su nove descrivono servizi che non esistono, e non spariranno mai da sole.

Il bucket del progetto e' 24800-24849 — cinquanta porte, calcolate da
`nexus-tool-kit/src/ports.rs:62` (`project_bucket_start`, hash dei primi 8 byte
dell'UUID modulo 400 bucket; per questo progetto `idx=96`, quindi `20000 + 96*50`).
Nove slot su cinquanta consumati in tre giorni da una app con due servizi: a quel
ritmo il bucket si esaurisce in poco piu' di due settimane, e l'esaurimento non
sarebbe un caso limite ma il funzionamento normale del sistema.

### Cinque fabbriche di identita', nessuna delle quali sa delle altre

Ogni riga della tabella ha un produttore diverso, e tutti sono raggiungibili dallo
stesso gesto dell'utente ("avvia il frontend"):

1. **Label esplicita presa verbatim.** `resolve_service_label`
   (`crates/mcp-core/src/agent_tools/service.rs:629`) accetta `input["label"]`
   dopo tre soli vagli: trim, non vuota, non generica. Non applica
   `normalizza_label`, che vive ottanta righe piu' sotto ed e' usata solo dal ramo
   dedotto dal percorso. Cosi' passano uno spazio (`frontend dev`), un punto, e
   perfino il nome intero di una unit.
2. **Deduzione dal comando.** `derive_kind_hint` e' una catena di
   `cmd.contains("vite")`, `contains("express")`, `contains("server.js")` che
   ritorna `Option<&'static str>` con due soli valori possibili: `frontend` e
   `backend`. E' una lista di varianti a codice — il pattern che la regola H vieta
   altrove — e ha un tetto strutturale: due frontend nello stesso progetto non
   possono ricevere due identita' distinte da questa via.
3. **Deduzione dal percorso.** `identita_dal_percorso` prende il nome della
   cartella sotto la radice.
4. **Ancoraggio dall'UUID.** `ServiceIdentity::SoloAncoraggio` produce
   `service-{uuid[..8]}` quando nessun segnale dice il ruolo ma il comando avvia
   un server. E' l'unico ramo onesto del gruppo — dichiara l'assenza di scopo
   invece di inventarlo — ed e' la riga 24802.
5. **Identita' generica.** `LABEL_NON_SERVIZIO = "Service"` (service.rs:555),
   emessa da `ServiceIdentity::NonServizio.classifica()`. Il `CLAUDE.md` la
   dichiara eliminata, e lo e' davvero per `agent_processes.kind` — un task
   declassato non ha unit ne' allocazione. Ma **non** per
   `nexus_port_allocations`: la riga 24826 dimostra che quella stringa entra nel
   registro delle porte lo stesso.

Nessuno di questi cinque produttori consulta gli altri quattro. Il ramo di riuso
di `find_or_allocate` (`allocate_port.rs:101`) cerca
`WHERE project_id = $1 AND label = $2`: uguaglianza esatta di stringa. Le cinque
chiavi del frontend (`frontend`, `frontend dev`, `frontend-preview`,
`bacheca-attivita-frontend`, `bacheca-attivita-frontend dev.service`) non possono
incontrarsi, per costruzione.

Il vocabolario che saprebbe riconciliarle esiste ed e' corretto:
`similar_service_labels("frontend", "bacheca-attivita-frontend")` e' vero, perche'
condividono una parola significativa. Ma viene applicato **solo a processi vivi**,
mai alle righe del registro: il ramo che lo usa
(`allocate_port.rs:371`) parte da `services.iter().filter(|s| s.listening)`. Il caso
normale in cui la label cambia e' il riavvio di un servizio **fermo**: nessun
listener, nessun candidato, allocazione nuova. Il sistema sa che sono lo stesso
servizio e non usa quel sapere nel punto in cui deciderebbe.

### Il furto di identita' (le due righe incoerenti)

Due righe hanno la coppia label/unit incoerente: 24826 (`Service` su
`bacheca-attivita-backend.service`) e 24806 (`frontend-preview` su
`bacheca-attivita-frontend.service`). L'unit si costruisce **dalla** label, quindi
quelle due unit sono state scritte quando la label era `backend` e `frontend`.
Qualcuno le ha riscritte dopo. Quel qualcuno e' `register_detected_port`
(`agent_tools/service.rs:975`):

```sql
INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
VALUES ($1, $2, $3, 'auto')
ON CONFLICT (port) DO UPDATE SET project_id = $1, label = $3, updated_at = NOW()
```

Il conflitto e' sulla **porta**, e la `DO UPDATE` riscrive la **label** senza
toccare `service_unit`. Chiunque stampi un numero di porta nel proprio stdout puo'
rinominare l'allocazione di un altro servizio. Dopo la riscrittura non esiste piu'
nessuna riga con label `backend`, quindi la `find_or_allocate("backend")`
successiva ne crea una nuova: e' 24828, nata il giorno dopo.

Questa e' la causa radice misurata della moltiplicazione del backend, e non e' un
difetto di logica: e' un difetto di **modello**. La label — cioe' l'identita' del
servizio — e' un attributo della riga *porta*, quindi la porta puo' cambiare
identita' al servizio.

### L'accumulo (le unit con lo slug doppio)

Le due unit `bacheca-attivita-bacheca-attivita-*` hanno lo slug ripetuto e
`.service` doppio. La firma e' inequivocabile: l'identita' nuova si e' costruita
sopra quella vecchia, che gia' la conteneva.

La funzione **diretta** (label -> unit) ha un punto unico:
`services.rs::service_unit_name` produce `{slug}-{label}.service`. L'**inversa**
(unit -> label) non ne ha nessuno, e la sua forma ricorrente e':

```rust
service.strip_prefix(&format!("{slug}-")).unwrap_or(&service)
       .strip_suffix(".service").unwrap_or(&service)
```

I due `unwrap_or` ricadono entrambi sulla stringa **intera**: se il prefisso viene
tolto ma il suffisso non c'e', il fallback del secondo passo rimette il prefisso
appena tolto. Con `service = "bacheca-attivita-frontend"` e
`slug = "bacheca-attivita"` l'esito e' `bacheca-attivita-frontend`, che diventa la
label del processo, poi la label dell'allocazione, poi l'unit
`bacheca-attivita-bacheca-attivita-frontend.service`. E' la riga 24807, alla
lettera. E' auto-amplificante: ogni giro aggiunge uno slug.

Le tre diagnosi indipendenti parlavano di "sei copie". Sono **sei file e dodici
occorrenze**, contate il 02/08/2026 con `strip_suffix(".service")` su `crates/`:
`project_workspace/services.rs` (204, 262, 695, 854, 1158, 1715),
`project_workspace/logs.rs` (1063, 1438), `project_workspace/wizard.rs` (1831,
2275), `project_workspace/service_recovery.rs` (662),
`nexus_builtin/services.rs` (99). Il numero esatto e' irrilevante, ed e' questo il
punto: finche' la domanda esiste, il conteggio delle sue risposte e' una cosa da
tenere aggiornata.

Nota sulla piattaforma, che cambia la lettura del difetto: l'ambiente in cui la
misura e' stata fatta e' **Windows nativo** (ambiente canonico dichiarato nel
`CLAUDE.md`), dove le unit systemd non esistono come oggetti del sistema
operativo. La stringa `bacheca-attivita-bacheca-attivita-frontend dev.service.service`
non ha mai nominato nulla: e' **solo un dato**, la chiave con cui l'allocazione si
lega al servizio. L'accumulo non e' un problema di systemd. E' un problema di una
chiave derivata che qualcuno ha il permesso di scrivere a mano.

### Perche' il registro non poteva accorgersene

Tutte e nove le righe sono `allocation_mode='adopted'`. Il ramo che lo timbra
(`allocate_port.rs:320`) scatta quando la porta della label non ascolta e nessun
processo del bucket e' provatamente suo: `UPDATE ... SET allocation_mode='adopted'`
e ritorna la stessa porta. Preso da solo e' ragionevole. Ma l'unica pulizia che
ragiona per label, `cleanup_dead_process_ports`, legge
`WHERE allocation_mode='dynamic'`: una riga adottata non la vede piu' nessuno, per
sempre. L'audit conta centinaia di `port_adopt` con `reason=stale_no_listener` —
la sola 24805 e' stata adottata circa quaranta volte fra le 23:48 e le 00:04 del
01/08. **Ogni riavvio a porta spenta ri-timbra la riga come adottata**, cioe' la
riconferma invece di metterla in discussione.

Il registro non e' una fotografia della realta' che si aggiorna: e' un diario di
eventi in cui ogni evento aggiunge e nessuno toglie. E' la definizione di sistema
edge-triggered, e un evento perso lo rende falso in modo permanente.

### La forma del difetto, in una frase

`nexus_port_allocations` mescola **spec** e **status** nella stessa riga: `label`
e' cio' che si desidera (l'identita' del servizio), `allocation_mode='adopted'` e
`service_unit` sono cio' che si e' osservato. Da questa mescolanza discende tutto
il resto — uno status ha potuto timbrarsi sopra un'identita', e un'osservazione ha
potuto crearne una nuova.

## Cosa dicono i supervisori maturi

Ricerca su systemd, launchd, s6/runit, supervisord, Docker Compose, Foreman/Heroku,
Kubernetes, Nomad, Testcontainers. Quattro principi, tutti applicabili qui.

1. **L'identita' e' dichiarata, mai derivata dal processo.** Nessun supervisore
   indovina il ruolo dal nome del programma, dal percorso o dalla command line. La
   chiave nasce dalla dichiarazione: nome dell'unit (systemd), `Label` reverse-DNS
   (launchd), sezione `[program:x]` (supervisord), nome della **directory** di
   servizio (s6: identificato dalla directory che e' stabile, non dal PID che non
   lo e'), chiave del servizio nel compose, process type del Procfile.
2. **Il nome del servizio e l'id dell'esecuzione sono due cose diverse e
   coesistono.** systemd le separa con l'`InvocationID`, ID a 128 bit rigenerato a
   ogni passaggio inactive -> activating. Il razionale della PR di Poettering e'
   esattamente il nostro problema: prima si usava il nome piu' gli istanti di
   start/stop, "fortemente soggetto a race". Il nome risponde a "quale servizio
   e'", l'InvocationID a "quale giro di vita e'".
3. **L'appartenenza si prova per contenimento, non per somiglianza.** systemd non
   si fida della parentela (il doppio fork la rompe) e usa il **cgroup**: un PID
   letto da un pidfile non fidato viene rifiutato se non e' gia' dentro il cgroup
   della unit. supervisord vieta l'auto-backgrounding per lo stesso motivo.
4. **Level-triggered, non edge-triggered.** Le API conventions di Kubernetes:
   il comportamento e' "level-based rather than edge-based", e chi scrive
   controller non puo' contare di aver visto la transizione, solo lo stato
   attuale. La formula di Hockin: "state is more useful than events". Applicato
   qui: la domanda giusta non e' "e' arrivato l'evento di crash?" ma "chi e' in
   ascolto su questa porta, adesso?".

Un quinto fatto vincola tutto: **solo `bind()` alloca davvero**. Qualunque registro
e' una previsione, e fra la verifica e il bind c'e' una finestra grande quanto il
tempo di avvio del processo. Nessun disegno la chiude su Windows senza la
cooperazione del processo figlio; si puo' solo renderla visibile.

## Le opzioni considerate

### A. Riparare i produttori (scartata)

Normalizzare la label esplicita, consolidare l'inversa unit->label in un punto
unico, aggiungere `similar_service_labels` al ramo di riuso, far leggere al GC
anche le righe `adopted`.

Scartata perche' e' la toppa che la regola H vieta: cura i cinque sintomi
misurati e lascia in piedi il meccanismo che li genera. Con la label libera come
chiave, il sesto produttore nascera' al prossimo intervento — e' gia' successo
tre volte (`orphan_placeholder_label`, il ripiego `unwrap_or("Service")`,
l'ancoraggio da UUID sono tutti tentativi precedenti di curare la stessa cosa). E
l'inversa consolidata resterebbe un'inversa: la domanda sbagliata, con una sola
risposta invece di dodici.

### B. Manifesto dichiarativo + ciclo di riconciliazione (scartata, con innesti)

Il disegno piu' pulito dei tre valutati: `project_service_manifest` (spec),
`project_service_status` (status), `service_invocations` (giro di vita),
riconciliatore level-triggered con decisore puro
`decide(desired, observed) -> ReconcileAction`, backoff persistito, capacita' nel
tipo (il riconciliatore riceve `ManifestReader` + `StatusWriter`, **mai** il
`ManifestStore`: non ha, nella propria firma, la facolta' di creare un'identita').

Scartata per la sequenza, non per il modello. La proposta si autoesclude con una
frase propria: "il manifesto senza il ciclo di riconciliazione sarebbe una quarta
fonte di verita' inerte accanto alle tre attuali; o si fanno entrambi, o non si
comincia". Il valore arriva tutto alla fine, e il suo cutover della scrittura e'
dichiarato "in un solo commit" — rimozione di `find_or_allocate` e
`register_detected_port`, sostituzione di `request_port`, `REVOKE` sul ruolo
applicativo — con **50 file** sotto `crates/` e `apps/` che nominano
`nexus_port_allocations` (contati il 02/08/2026 con `grep -rl`). In questo repo il
big bang e' la forma di intervento che ha gia' prodotto lavoro perso: sette
sessioni su tredici si sono fermate col lavoro nel solo worktree (regola P).

Due difetti puntuali del suo schema, entrambi corretti nella decisione: `UNIQUE
(port)` **globale sulla macchina**, che lega la dichiarazione di un progetto allo
stato di un altro (viola la regola E: un progetto archiviato che ha pinnato una
porta la sequestra per sempre); e `pg_advisory_lock(project_id)`, che non e'
chiamabile cosi' — l'advisory lock vuole un `i64`, `project_id` e' un UUID, quindi
serve un hash e con l'hash arrivano collisioni fra progetti che si serializzano a
vicenda senza saperlo.

**Innestato dalla decisione**: la capacita' nel tipo, il decisore puro, il backoff
persistito con soglia di arresto, la ragione tipizzata del riavvio, il file di
lock in `state_dir` (fuori dalla project root), la riga di invocazione inserita
**prima** dello spawn, il fallimento rumoroso come enforcement, la misura
dell'attrito nuovo per l'agente.

### C. Porta calcolata, senza registro (scartata, con innesti)

L'identita' e' un `Role` canonico validato al costruttore; da `(project_id, role)`
discendono per funzione pura chiave, unit e porta. Tesi: "allocare non e' piu' una
decisione che qualcuno prende e registra, e' un calcolo che chiunque rifa' uguale".

Scartata per tre ragioni misurate.

**Il meccanismo distintivo esiste gia' in produzione e non ha impedito nulla.**
`deterministic_project_port_for_key` (`project_workspace/services.rs:2539`) fa hash
della chiave piu' linear probing dentro il bucket, ed e' chiamata da sei punti
(`allocate_port.rs:188` e `:454`, `wizard.rs:484` e `:978`, `run_configs.rs:939`,
`service_discovery.rs:210`). La derivazione della porta e' viva da tempo e le nove
identita' sono nate lo stesso, perche' il difetto non era il calcolo: era la
**chiave libera**. La proposta presenta come rimedio decisivo cio' che il repo gia'
esegue.

**La purezza si autodistrugge.** La proposta stessa calcola circa il 28% di
probabilita' di collisione dello slot naturale con sei servizi su cinquanta slot
(paradosso del compleanno), introduce il probing, e a quel punto deve
**persistere** lo slot come sigillo e inventare l'esito `SlotDrift` per la
divergenza fra sigillo e derivazione. Il registro non sparisce: degrada da verita'
a sigillo verificabile — cioe' due fonti per la stessa domanda, la regola L
violata dalla cura. In piu' l'assegnazione dipende dall'ordine di dichiarazione
sull'intero manifesto: cancellare un servizio fa derivare agli altri uno slot
diverso da quello sigillato, e l'operazione piu' normale del sistema produce
allarme perpetuo su servizi sani.

**`UNIQUE(project_id, slot)` non impedisce due servizi sulla stessa porta.** I
bucket sono 400 (`(39999-20000+1)/50`, verificato in `ports.rs`) e sono derivati da
un hash dell'UUID: a una trentina di progetti registrati la probabilita' che due
condividano il bucket supera il 60%. Due progetti col bucket colliso possono avere
due servizi legittimi sulla stessa porta senza violare alcun vincolo. La proposta
che promette il calcolo al posto del registro perde proprio l'invariante che il
calcolo doveva garantire.

**Innestato dalla decisione**: la sequenza deterministica di slot come **politica**
di scelta dentro la prenotazione (rende l'assegnazione riproducibile e
diagnosticabile — "perche' proprio 24804?" — senza pretendere di sostituire il
registro); il vincolo **zero riavvii** come criterio di accettazione dell'import;
l'ordine granulare del cutover dei produttori; la rinomina come operazione
transazionale di prima classe; `port_exceptions` con `granted_by` ed `expires_at`;
la terna persistita `(pid, pid_start_time, image_path)` come prova di ripiego; la
disinstallazione verificata unit per unit; la rimisura a 7 e 30 giorni.

### D. La decisione (di seguito)

## Decisione

Tre domande, una risposta ciascuna, ognuna con la propria sede.

| Domanda | Risposta unica | Sede |
|---|---|---|
| Chi e' questo servizio? | `project_services.service_key`, dichiarata | manifesto (spec) |
| Questo processo e' suo? | contenimento nel Job Object, con ripiego dichiarato | kernel |
| Questa porta e' sua? | `service_port_reservations`, 1:1 col servizio | prenotazione |
| In che stato e', adesso? | vista `service_status`, join fra desiderato e osservato | osservatore (status) |

Un servizio **esiste solo se una scrittura esplicita lo ha dichiarato**. La sua
identita' e' una chiave canonica validata dallo schema, che nessun percorso di
esecuzione puo' coniare. Tutto il resto — porta, unit, pid, stato — e' o **derivato**
da quella chiave o **osservato** dal sistema operativo e riscritto a ogni giro.

Ne discende la proprieta' che chiude il difetto misurato: il numero delle identita'
di un progetto puo' cambiare **solo quando qualcuno lo chiede**, mai come effetto
collaterale di un avvio, di un riavvio, di un cambio comando o della lettura dello
stdout di un processo.

### Condizione di non-inizio

Assunta dall'opzione B come vincolo esplicito del piano: **un manifesto senza il
ciclo di riconciliazione e' una quarta fonte di verita' inerte accanto alle tre
attuali**. Se non si arriva almeno al passo 5 (cutover della scrittura), non si
comincia. I passi 0-4 restano committabili e utili singolarmente (regola P), ma il
piano non si apre senza l'impegno ad arrivare al 5.

## Modello dati

Tutto in DB **meta**, migrazioni versionate a partire da **0671**: la 0670 e'
occupata da `0670_ambiente_esecuzione_dichiarato.sql`, gia' in main. Il numero
libero va RIVERIFICATO al momento di scrivere la prima migrazione, non assunto da
questo documento: fra la stesura e l'implementazione altre sessioni ne aggiungono. Le invarianti stanno
nello schema o nei tipi; nessuna vive in un commento.

### 1. `project_services` — la SPEC

Unica sede in cui nasce un'identita'. Scritta da chi dichiara (umano, agente,
importatore), **mai** dall'osservatore.

```
id                UUID PRIMARY KEY
project_id        UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE
project_slug      TEXT NOT NULL
  FOREIGN KEY (project_id, project_slug)
    REFERENCES projects(id, slug) ON UPDATE CASCADE     -- richiede UNIQUE(id,slug) su projects
service_key       TEXT NOT NULL
  CHECK (service_key ~ '^[a-z][a-z0-9]*(-[a-z0-9]+)*$' AND length(service_key) BETWEEN 2 AND 32)
  CHECK (service_key <> project_slug)
kind              TEXT NOT NULL CHECK (kind IN ('web','worker'))
command           TEXT NOT NULL
working_dir       TEXT NOT NULL DEFAULT '.'
  -- Il separatore NON e' solo `/`: l'ambiente canonico e' Windows nativo, dove
  -- il separatore e' `\` e un percorso puo' essere `C:\...`, `\\server\share`
  -- (UNC) o `..\..\altrove`. Le due regex della prima stesura conoscevano il
  -- solo slash, quindi `..\..\..\Users\CBRAC\.claude` le attraversava
  -- indisturbato e con esso saltava l'isolamento di progetto (regola E).
  -- Si vieta percio' su ENTRAMBI i separatori, e si vieta anche il prefisso UNC.
  CHECK (working_dir !~ '(^|[/\\])\.\.([/\\]|$)')
  CHECK (working_dir !~ '^([A-Za-z]:|[/\\])')
  -- I byte che un percorso non puo' contenere: NUL e i caratteri che Windows
  -- rifiuta. Non e' zelo, e' la stessa famiglia del difetto: `frontend dev` e'
  -- entrato come label perche' nessuno aveva detto quali byte fossero ammessi.
  CHECK (working_dir !~ '[\x00-\x1f<>:"|?*]')
env               JSONB NOT NULL DEFAULT '{}'
health_spec       JSONB NULL
desired_state     TEXT NOT NULL DEFAULT 'stopped' CHECK (desired_state IN ('running','stopped'))
generation        BIGINT NOT NULL DEFAULT 1            -- trigger +1 su command/working_dir/env/kind
created_by        TEXT NOT NULL CHECK (created_by IN ('human','agent','import'))
unit_name         TEXT GENERATED ALWAYS AS (
                    project_slug || '_' || substr(project_id::text, 1, 8) || '_'
                    || service_key || '.service') STORED
UNIQUE (project_id, service_key)
UNIQUE (project_id, id)     -- per le FK composite
UNIQUE (id, kind)           -- per la FK che vieta le prenotazioni ai worker
UNIQUE (unit_name)
```

Cosa rende impossibile, **per schema**:

- `frontend dev` (spazio), `Service` (maiuscola), `bacheca-attivita-frontend dev.service`
  (punto e spazio): rifiutate dal CHECK regex, da qualunque produttore, incluso un
  `INSERT` a mano da psql.
- `bacheca-attivita` come chiave nel progetto `bacheca-attivita`: rifiutata dal
  secondo CHECK. E' la forma `{slug}-{slug}.service`, che dice due volte il
  progetto e non dice mai il ruolo.
- `unit_name` e' **GENERATED**: nessuno la scrive, quindi nessuno la puo'
  accumulare. Con essa spariscono le dodici occorrenze dell'inversa e la loro
  classe di bug — non si toglie lo stesso difetto dodici volte, si toglie
  l'**oggetto** su cui lavorava.

Il separatore `_` non e' un dettaglio estetico: ne' `slug` (prodotto da
`projects::slugify`, che emette solo `[a-z0-9-]`) ne' `service_key` (stessa
grammatica) possono contenerlo, quindi la composizione e' **iniettiva**. Chi ha in
mano una unit letta dal sistema operativo la riconosce con un `WHERE unit_name = $1`
— una **lookup**, non una scomposizione della stringa. Il discriminatore
`substr(project_id::text,1,8)` esiste perche' `projects` ha `UNIQUE(team_id, slug)`,
non `UNIQUE(slug)`: due team possono avere lo stesso slug, e senza discriminatore
la dichiarazione di un progetto potrebbe essere bloccata dalla dichiarazione di un
altro (regola E). Con il discriminatore, `UNIQUE(unit_name)` e' una **trappola che
non puo' scattare** fra due dichiarazioni legittime: se scatta, e' la funzione
generatrice a essere sbagliata, ed e' esattamente la notizia che vogliamo forte.

Nota deliberata: **non** si vieta a `service_key` di *iniziare* con lo slug del
progetto. In un progetto `api` la chiave `api-gateway` e' legittima; un CHECK sul
prefisso pagherebbe un costo permanente su nomi validi per chiudere una seconda
volta un difetto che la colonna GENERATED ha gia' chiuso alla radice.

I **task one-shot** (`npm install`, un lint, `playwright test`) restano fuori dal
manifesto, in `agent_processes`: niente riga, quindi niente unit e niente
prenotazione. E' il vocabolario `kind IN ('web','worker')` a dirlo, non un
commento. La confusione fra servizi e one-shot e' l'origine diretta della riga
`Service`.

### 2. `service_port_reservations` — la PREVISIONE

```
service_id   UUID NOT NULL
service_kind TEXT NOT NULL CHECK (service_kind = 'web')
  FOREIGN KEY (service_id, service_kind) REFERENCES project_services(id, kind) ON DELETE CASCADE
project_id   UUID NOT NULL
  FOREIGN KEY (project_id, service_id) REFERENCES project_services(project_id, id) ON DELETE CASCADE
purpose      TEXT NOT NULL CHECK (purpose IN ('primary','secondary','debug','metrics'))
port         INT  NOT NULL CHECK (port BETWEEN 1 AND 65535)
origin       TEXT NOT NULL CHECK (origin IN ('bucket','deroga'))
bucket_start INT  NOT NULL
  FOREIGN KEY (project_id, bucket_start) REFERENCES projects(id, port_bucket_start)
-- Fuori dal bucket si sta SOLO con una deroga che esiste come RIGA, non come
-- valore scritto nella stessa riga che dovrebbe autorizzare. La FK e' verso
-- port_exceptions e ha la porta nella chiave, quindi la deroga non e' un
-- attributo di se stessa: e' un fatto separato, firmato e datato, che qualcun
-- altro ha dovuto scrivere.
exception_id UUID NULL REFERENCES port_exceptions(id) ON DELETE RESTRICT
CHECK (
  (origin = 'bucket' AND exception_id IS NULL
     AND port >= bucket_start AND port <= bucket_start + 49)
  OR
  (origin = 'deroga' AND exception_id IS NOT NULL)
)
FOREIGN KEY (exception_id, port) REFERENCES port_exceptions(id, port)  -- la deroga vale per QUELLA porta
PRIMARY KEY (service_id, purpose)
UNIQUE (port)
```

**Perche' `origin='manual'` non esiste piu'.** Nella prima stesura il vincolo era
`CHECK (origin = 'manual' OR port nel bucket)`: una disgiunzione la cui seconda
alternativa era una **stringa scritta nella stessa riga**. Chiunque potesse
inserire la riga poteva scrivervi `'manual'` e assolversi da solo, senza motivo,
senza firma e senza scadenza — cioe' esattamente il difetto di
`allocation_mode='manual'` che questo ADR dichiara di correggere, e che la regola
L del CLAUDE.md descrive con «uno sbaglio non puo' rendersi lecito da se'».
Riprodurlo nel sistema nuovo, in forma piu' elegante, sarebbe stato il modo
peggiore di fallire. La deroga ora **costa una riga in un'altra tabella**, con la
porta nella chiave: `INSERT` in `service_port_reservations` con `origin='deroga'`
e `exception_id` inventato viola la FK; con una `exception_id` valida ma di
un'altra porta, viola la FK composita.

- Al piu' **una** porta `primary` per servizio: e' la PK.
- Una porta appartiene a **un solo** servizio: `UNIQUE(port)`. A differenza
  dell'opzione B, la porta non e' una colonna della spec ma una riga con FK e
  `ON DELETE CASCADE`, quindi la cancellazione del servizio la libera davvero e
  nessun progetto archiviato puo' sequestrare una porta.
- Nessuna riga puo' esistere senza un servizio dichiarato. La riga orfana — meta'
  del problema di oggi — non e' **rappresentabile**, e con essa sparisce l'intera
  famiglia del garbage collection delle porte.
- Un `worker` non puo' avere una prenotazione: lo impedisce la FK composita su
  `(id, kind)`. Non serve un controllo in Rust che ogni chiamante puo' saltare.
- `projects.port_bucket_start` viene materializzato al provisioning con lo stesso
  valore che `project_bucket_start()` produce oggi: nessun servizio cambia porta, e
  il bucket smette di essere una funzione ricalcolata a ogni domanda per diventare
  un dato referenziabile da un vincolo.

`allocation_mode` scende da cinque valori a due. `auto`, `dynamic`, `existing`,
`adopted` erano quattro modi di raccontare la **nascita** di una decisione che ora
non si prende piu'; `adopted` in particolare era uno status timbrato sopra una
spec. Restano le sole due risposte che cambiano una decisione: l'ha scelta il
bucket, o l'ha scelta un umano.

### 3. `port_exceptions` — la deroga umana, datata e firmata

```
project_id UUID, port INT, PRIMARY KEY (project_id, port)
reason TEXT NOT NULL, granted_by TEXT NOT NULL, expires_at TIMESTAMPTZ NULL, created_at
```

Erede unico e onesto di `allocation_mode='manual'`. Una deroga senza autore e senza
scadenza e' la riga che fra un anno nessuno sapra' spiegare.

Lo schema porta anche `id UUID NOT NULL` con `UNIQUE (id, project_id, port)`: e'
il bersaglio della FK composita di `service_port_reservations`. Senza quella
`UNIQUE` la FK non e' dichiarabile, e la deroga tornerebbe a essere una stringa
che assolve se stessa.

`expires_at` non e' decorativo: una deroga **scaduta** non autorizza piu' nulla, e
il ciclo di riconciliazione la tratta come assente. Va detto qui perche' un campo
che nessun percorso legge e' un campo che mente — e' la stessa famiglia dei
contatori che questo ADR corregge poco sotto.

### 3-bis. Il freno: dove stanno i contatori

Il freno anti-tempesta descritto nei flussi ha bisogno di colonne, altrimenti e'
una promessa in prosa. Stanno su `project_services`, perche' appartengono al
SERVIZIO dichiarato e devono sopravvivere alla morte dell'istanza:

```
restart_count    INT NOT NULL DEFAULT 0
next_attempt_at  TIMESTAMPTZ NULL
last_failure     TEXT NULL
desired_state    TEXT NOT NULL DEFAULT 'stopped'
  CHECK (desired_state IN ('running','stopped','failed'))
```

`failed` e' nel CHECK di `desired_state`, non fra gli stati dell'istanza: e' la
DICHIARAZIONE che quel servizio non va piu' riavviato finche' un umano non
interviene, e sopravvive per costruzione al riavvio di mcp-core e alla sparizione
di ogni riga di istanza. `ReconcileAction::MarkFailed{cause}` scrive qui: senza
questa colonna era un esito tipizzato correttamente (regola Q) e senza
destinazione, cioe' un tipo che il DB non sa ricevere.

### 4. `service_instances` — il GIRO DI VITA

L'`InvocationID` di systemd, tenuto distinto dal nome.

```
id                    UUID PRIMARY KEY          -- "quale esecuzione"
service_id, project_id (FK composita a project_services)
containment_name      TEXT NOT NULL UNIQUE      -- nome del Job Object
generation_at_start   BIGINT NOT NULL           -- observedGeneration
launcher_pid          INT,  launcher_start_time BIGINT,  image_path TEXT
state                 TEXT NOT NULL CHECK (state IN
                        ('starting','running','running_not_listening','adopted','exited','lost'))
members_seen          INT,  last_seen_at, started_at, stopped_at, exit_code INT
stop_reason           TEXT CHECK (stop_reason IN
                        ('requested','crashed','generation_changed','renamed',
                         'port_conflict','superseded','supervisor_shutdown','unknown'))
CREATE UNIQUE INDEX ... ON service_instances (service_id) WHERE state IN ('starting','running','adopted')
```

L'indice **parziale** rende il doppio avvio una violazione di vincolo invece che
una seconda porta. La riga si inserisce **prima** dello spawn (innesto da B): un
crash di mcp-core lascia comunque la traccia. `stop_reason` include `renamed`
perche' dal registro si deve leggere **perche'** un servizio e' ripartito, non solo
che e' ripartito.

Contropartita dell'indice parziale, e va gestita esplicitamente (vedi flussi):
mcp-core ucciso mentre l'istanza girava lascia una riga viva che nessuno chiude, e
il servizio diventa non avviabile. Il rimedio e' `force_close_instance`, un
percorso dichiarato e auditato, non un `UPDATE` a mano sul DB.

### 5. `observed_listeners` — la MISURA GREZZA

```
port INT PRIMARY KEY, snapshot_id UUID NOT NULL, observed_at TIMESTAMPTZ NOT NULL
pid INT, program TEXT
instance_id UUID NULL REFERENCES service_instances(id)   -- solo se PROVATO
membership TEXT NOT NULL CHECK (membership IN ('in_job','marker_file','brokered','foreign','unknown'))
broker TEXT NULL   -- valorizzato sse membership='brokered'
```

**Nessuna colonna label.** L'`ON CONFLICT (port) DO UPDATE SET label` non e'
vietato: e' privo di bersaglio. L'identita' ha smesso di essere un attributo della
porta.

La tabella contiene **solo lo snapshot corrente**, sostituito in transazione a ogni
giro. La domanda della retention sparisce per costruzione: cio' che vale la pena
conservare non e' uno stato ripetuto migliaia di volte, e' una **transizione**, e
le transizioni stanno in `service_instances` e nell'audit.

### 6. Vista `service_status` — nessuno la scrive, tutti la leggono

`service_key, unit_name, kind, desired_state, generation, observed_generation,
instance_id, instance_state, reserved_port, listening, listening_pid, membership,
conformita'`.

Risponde in un punto solo al pannello Servizi, al pannello Porte, al blocco
RISORSE PROGETTO del prompt, al port_enforcer e al gate di readiness. Il "verde"
non e' un campo scritto da chi avvia: e' un **join** fra cio' che si e' chiesto e
cio' che si e' visto.

### Tipi Rust (regola Q: l'esito sta in un campo, e l'ignoto e' una variante)

Crate nuovo `nexus-service-identity`, punto unico dell'identita' (regola L).

- `ServiceKey` — newtype con **unico** costruttore
  `parse(&str) -> Result<ServiceKey, ServiceKeyError{Vuota|CaratteriIllegali|UgualeAlloSlug|TroppoLunga}>`.
  Nessun `From<&str>`, nessun `Deref<Target=str>`. Nessuna firma del sottosistema
  accetta piu' `&str` per l'identita': oggi la label e' `&str` da capo a fondo, ed
  e' per questo che ogni produttore ne puo' fabbricare una qualunque.
- `UnitName` — tipo **opaco**, ottenuto solo da una lookup o dalla colonna
  generata. Nessun `FromStr`, **nessuna funzione inversa**, in nessun modulo.
- `Membership` — `InJob{pid} | MarkerFile{pid, path} | Brokered{broker} |
  Foreign{pid, program} | Unknown{motivo}`.
- `PortTruth` — `OwnListening{instance, pid} | ForeignListening{pid, program} |
  Brokered{broker} | Silent | NotObservable{motivo}`.
- `Reconciliation` — `InSync | NeedsStart | NeedsStop | StaleGeneration{da,a} |
  PortMismatch{prenotata, osservata} | PortHeldByForeign{pid} | Unobservable{motivo}`.
  Non e' mai un `bool`: "non ho potuto guardare" non degrada ne' a "va bene" ne' a
  "e' rotto".
- `ServiceOutcome` — `Declared{port} | AlreadyRunning{..} | Started{..} |
  Restarted{..} | Renamed{..} | Stopped{exit_code} |
  RefusedUndeclaredService{chiavi_dichiarate, chiamata_da_fare} |
  PortHeldByForeign{..} | PortMismatch{..} | Unobservable{motivo}`. Nessun ramo
  ritorna `String`; il testo per l'umano e' un campo, composto **dai** campi.

### Il decisore puro e la capacita' nel tipo

```rust
pub fn decide(desired: &DesiredState, observed: &ObservedState) -> ReconcileAction
```

Funzione **pura e totale**, senza I/O, con `ReconcileAction` enum chiuso
(`Noop | Start | Stop{reason} | Restart{reason} | Adopt{instance} |
MarkUnhealthy{cause} | MarkFailed{cause} | Hold{reason}`). E' la parte piu'
pericolosa del sistema — chi decide di riavviare — e senza questa estrazione
sarebbe testabile solo attraversando il sistema operativo, quindi senza batteria e
senza test di mutazione (regola O).

Il riconciliatore riceve un `ManifestReader` (sola lettura) e uno `StatusWriter`.
**Non riceve mai il `ManifestStore`**: non ha, nella propria firma, la facolta' di
creare un'identita'. Senza questo vincolo, "lo status lo scrive solo l'osservatore"
resta un commento, e i commenti non sono invarianti. E' anche l'unica risposta
strutturale a un nome come `service-66f4bf72`, che nessuna grammatica esclude: se
nessun tipo raggiungibile dall'osservazione puo' fare `INSERT` su
`project_services`, quel nome non e' coniabile qualunque cosa scriva un call site
futuro.

### Le invarianti, e cosa le impone

| # | Invariante | Imposta da |
|---|---|---|
| I1 | Nessuna porta senza servizio dichiarato | FK + `ON DELETE CASCADE` |
| I2 | Chiave canonica | CHECK regex + `ServiceKey::parse` |
| I3 | Una porta, un servizio | `UNIQUE(port)` su reservations |
| I4 | Una `primary` per servizio | PK `(service_id, purpose)` |
| I5 | Porta nel bucket o deroga umana | CHECK + FK su `projects.port_bucket_start` |
| I6 | Un worker non ha porta | FK composita su `(id, kind)` |
| I7 | Una sola esecuzione viva per servizio | indice UNIQUE parziale |
| I8 | La unit e' derivata e mai riletta | colonna GENERATED + assenza di inversa |
| I9 | L'osservatore non puo' dichiarare | capacita' nel tipo |
| I10 | L'identita' non e' attributo della porta | assenza della colonna `label` in `observed_listeners` |

## Flussi

### Dichiarazione (il flusso che oggi non esiste)

`declare_service(project, service_key, kind, command, working_dir, env, health)`.
`ServiceKey::parse` rifiuta cio' che non e' canonico; l'UPSERT su
`(project_id, service_key)` e' idempotente per chiave, non per fortuna; la
prenotazione viene assegnata alla prima dichiarazione e non cambia piu'.

Il caso normale **non passa dall'agente**: alla registrazione del progetto un
importatore legge il manifesto reale (script di `package.json`, servizi del
compose, Procfile) e propone le dichiarazioni; l'utente conferma. Dichiarare non
avvia nulla.

### Avvio

E' una riconciliazione, non un evento.

1. Chiave assente -> `RefusedUndeclaredService{chiavi_dichiarate, chiamata_da_fare}`,
   con la `declare_service` **gia' compilata** (stesso schema del
   `next_action_recommended` della pipeline allegati, ADR 0012). Il rifiuto senza
   l'azione pronta lo paga l'agente in iterazioni.
2. Porta dalla prenotazione. Se manca, si sceglie con la sequenza deterministica
   di slot (innesto da C) la prima porta insieme non prenotata (DB) e non in
   ascolto (osservazione), e la si scrive.
3. Bindabilita': `port_recovery::classifica_bind` gia' esistente, esito tipizzato
   `PortBind{Libera|Occupata|NonInterrogabile}` — "occupata da un processo" e "il
   sistema non ha piu' porte effimere" restano due cose diverse.
4. Istanza viva il cui pid ascolta sulla porta attesa -> `AlreadyRunning`, si esce.
   E' il ramo che oggi non esiste e che da solo elimina la moltiplicazione.
5. Porta occupata da un estraneo -> **non ci si sposta su un'altra porta**:
   `PortHeldByForeign` con pid e programma. Il ripiego automatico e' esattamente
   cio' che sposta i servizi fuori bucket.
6. `INSERT` dell'istanza (stato `starting`), poi spawn dentro un Job Object
   intestato all'istanza, con `PORT`/`HOST`/`NEXUS_SERVICE_KEY`/`NEXUS_INSTANCE_ID`
   iniettati e un file marcatore in **`state_dir`** (mai dentro la project root:
   li' `git clean -fdx`, `npm ci`, la ricreazione di un worktree e l'autocommit di
   sessione lo cancellerebbero o lo committerebbero) contenente
   `(instance_id, pid, pid_start_time, image_path)`.
7. Readiness: non e' chi avvia a dichiarare `running`. L'osservatore attende che un
   pid **membro del job** compaia in ascolto sulla **porta prenotata**. Se compare
   altrove -> `PortMismatch{prenotata, osservata}`.

### `PortMismatch` non produce mai un `Restart`

Regola esplicita del decisore. Un processo vivo che ascolta altrove (Vite o Next
senza `strictPort`, che ripiega in silenzio) e' un **problema da mostrare** nel
pannello Problemi, non un servizio da uccidere. Senza questa regola il ciclo
diventa un uccisore periodico: il servizio non risulta mai `running`, quindi si
riavvia, quindi ripiega di nuovo. Oggi il difetto e' un registro sporco col
servizio **su**; sarebbe una regressione consegnare un registro pulito col servizio
**giu'**.

La mitigazione alla causa — `strictPort` imposto nella configurazione generata — e'
un passo del piano (P6), non una nota, con la verifica che l'agente non la
sovrascriva al primo turno successivo.

### Adozione

Si adotta per **contenimento**, mai per somiglianza, e l'adozione puo' solo
**legare** un processo a un'identita' che esiste gia': non puo' crearne una.

- Processo dentro il Job Object dell'istanza -> `Membership::InJob`, adottato.
- Job non riapribile ma processo sulla porta prenotata col marcatore valido —
  `instance_id` **e** `pid_start_time` **e** `image_path` coincidenti ->
  `Membership::MarkerFile`, adottato con l'ammissione esplicita che il contenimento
  non c'e'. Lo start time del SO chiude il riuso di PID: il pid da solo autentica il
  processo sbagliato dopo un riavvio della macchina.
- Listener **intermediato** (Docker Desktop `com.docker.backend`/vpnkit, relay WSL)
  sulla porta prenotata di un servizio dichiarato di quel progetto ->
  `Membership::Brokered{broker}`, che e' uno stato **legittimo**, non un conflitto.
  Senza questo ramo ogni progetto in compose diventerebbe rosso permanente: il repo
  genera gia' `docker-compose.nexus.yml` mappando porte del bucket
  (`project_workspace/compose_ports.rs:22`, `parse_service_ports` a `:83`).
- Nessuna prova -> **non si adotta**. `PortHeldByForeign`, con l'occupante
  dichiarato e due azioni umane possibili. Una porta libera in piu' non costa
  nulla; una porta condivisa fra due servizi e' l'incidente.

Il vocabolario `similar_service_labels` / `is_generic_service_label` non partecipa
a nessuno di questi rami. La somiglianza fra nomi cessa di essere una prova.

### Osservazione non conclusiva: si sospende, non si agisce

`Unobservable`/`Unknown` **inibisce** `Start`, `Stop` e `Restart` — non e' solo un
esito da dichiarare. Senza questa regola, al rientro del DB meta dopo un pool
stale (incidente gia' noto in questo repo), ogni servizio con
`desired_state='running'` e nessuna istanza registrata ripartirebbe simultaneamente
su tutto il parco progetti. Un ciclo level-triggered senza freno sull'ignoto e' una
tempesta di riavvii in attesa dell'occasione.

### Morte del processo, e freno

A ogni giro, per ogni istanza viva: si riapre il job per nome e si chiede al kernel
la lista dei pid. Lista vuota o oggetto assente -> `exited`. Membri vivi ma nessuno
in ascolto sulla porta prenotata -> `running_not_listening`, che e' un fatto
**diverso** da `exited` e va detto diverso.

La prenotazione **non** viene rilasciata: e' del servizio dichiarato, non
dell'esecuzione.

Freno obbligatorio (innesto da B): `restart_count` e `next_attempt_at`
**persistiti**, quindi sopravvivono al riavvio di mcp-core, e una soglia oltre la
quale il servizio va in `failed` e **smette**, chiedendo intervento. Concentrare la
decisione in un solo ciclo significa concentrare anche il danno: oggi e' distribuito
e lento (nove righe in tre giorni), domani sarebbe concentrato e rapido (un riavvio
per tick).

### Riavvio di mcp-core

Nessuno stato in RAM e' autoritativo e nessun registro viene ricostruito a
indovinare. Per ogni istanza che risultava viva: `OpenJobObject` sul
`containment_name` persistito; se si riapre, i pid arrivano dal kernel e l'istanza
prosegue senza interruzione; altrimenti il marcatore con la terna; altrimenti
`lost`, che e' **dichiarato**, non dedotto.

La spec non viene toccata all'avvio. Sparisce la classe di incidenti in cui lo
`startup_recovery` svuotava o riscriveva il registro delle porte.

Prova decisiva del disegno: cancellando `service_instances` e `observed_listeners`
per intero, il sistema riparte identico — le porte sono le stesse perche' stanno
nella spec, e lo status e' ricostruibile per osservazione. Il registro dello stato
puo' essere perso senza perdere l'identita'.

### Cambio di comando

`UPDATE` della spec, `generation` +1 dal trigger. L'istanza in corso ha
`generation_at_start < generation`, quindi la vista dice `StaleGeneration{da,a}` e
il ciclo agisce secondo `desired_state`, chiudendo la precedente con
`stop_reason='generation_changed'`. **Identita', porta e unit non cambiano.**

E' precisamente il caso che oggi produce un nome nuovo (`frontend` ->
`frontend-preview` -> `frontend dev`) e con esso una porta nuova: il comando torna
a essere un **attributo** del servizio invece che la sua identita'.

### Rinomina del servizio (obbligatoria, non facoltativa)

`service_rename(service_id, nuova_key)`: operazione **transazionale** che conserva
`id`, prenotazione e storia, e cambia la sola chiave. Chiude l'istanza con
`stop_reason='renamed'`, disinstalla la vecchia unit **verificandone la
scomparsa**, installa la nuova, riavvia.

Senza questo flusso, con `service_key` dentro la chiave unica e `unit_name`
derivato, rinominare sarebbe `DELETE` + `INSERT`: cioe' esattamente il meccanismo
che questo ADR dichiara di estinguere, ripresentato come operazione ordinaria. Il
primo utente che vuole chiamare un servizio diversamente rifarebbe il difetto.

### Rinomina del progetto

Lo slug e' denormalizzato in `project_services` per poter esprimere il CHECK in DB,
quindi la FK composita porta `ON UPDATE CASCADE`. Cambia `unit_name` per **tutti** i
servizi del progetto in un colpo solo: la rinomina di un progetto e' percio' un
flusso che disinstalla e reinstalla le unit una per una, con verifica, e non un
semplice `UPDATE` sul nome.

### Cancellazione

`DELETE` sulla riga di manifesto: `CASCADE` porta via prenotazione e istanze, dopo
la terminazione per contenimento. E' **l'unico** modo in cui una porta torna
disponibile — un solo punto, esplicito e voluto, invece di sei percorsi di pulizia
che si contraddicono.

### Chiusura forzata di un'istanza

`force_close_instance(instance_id, motivo)`: percorso esplicito e auditato per la
riga rimasta `running` dopo un `kill -9` di mcp-core. Con il solo indice parziale e
senza questo percorso, quel caso — che nel prompt originario e' descritto come un
fastidio — diventerebbe uno **stallo** che richiede di toccare il DB a mano.

## Piano di migrazione

Ogni passo e' committabile e utile da solo (regola P). Nessun passo prima del 5 e'
irreversibile.

### P0 — Misurare il perno e lo stato, prima di spendere una riga

Due misure indipendenti, entrambe bloccanti.

**(a) Il Job Object.** Verificato il 02/08/2026: nel repo ci sono **zero**
occorrenze di `JobObject`/`CreateJobObject`/`AssignProcessToJobObject` in `crates/`
e `apps/`. E' una capacita' **assunta**, non usata. Esperimento da poche decine di
righe, prima di qualunque schema: venti avvii reali di `npm`, `pnpm`, `npx`,
`node`, `cargo`, `python`, con la percentuale di `Membership::InJob`.
`AssignProcessToJobObject` fallisce se il processo e' gia' in un job che non
consente il nesting, e i wrapper npm creano job propri. Nello stesso esperimento,
la seconda incognita: mcp-core gira come servizio WinSW (sessione 0) ma anche a
mano da `dev-start.ps1` (sessione interattiva); un nome `Local\...` vive nel
namespace della sessione, quindi se launcher e osservatore finiscono in sessioni
diverse `OpenJobObject` non risolve e **ogni** istanza diventa `lost` dopo un
riavvio di mcp-core — cioe' il meccanismo degrada proprio nel momento per cui
esiste. `Global\...` richiede `SeCreateGlobalPrivilege`, che il servizio ha e un
avvio manuale no: le due configurazioni vanno misurate entrambe.

Se la percentuale non regge, il perno reale e' il marcatore con la terna, e questo
va saputo **prima**, non dopo aver disegnato meta' sistema intorno alla prova
forte. E' il caso, gia' visto in questo repo con `orphan_placeholder_label`, della
catena corretta nel codice e irraggiungibile nei dati.

**(b) Il censimento.** Su tutti i progetti: righe di `nexus_port_allocations`,
quante di quelle porte ascoltano adesso, quante label sono generiche o contengono
spazi o slug duplicato, quante coppie label/`service_unit` sono incoerenti (prova
diretta della riscrittura da parte di `register_detected_port`). Il censimento
interroga il sistema operativo con le **stesse** funzioni della produzione, mai un
`netstat` parsato a parte (regola O). Su `bacheca-attivita` deve dire: 9 righe, 3
in ascolto, 2 servizi reali, 4 coppie incoerenti. E' il metro del prima-dopo.

### P1 — Schema additivo, nessun consumatore

Migrazioni 0671+ (primo numero libero verificato al momento, vedi sopra): `projects.port_bucket_start` materializzato,
`UNIQUE(projects.id, slug)`, le cinque tabelle, la vista. Nessuna scrittura sulle
tabelle vecchie.

Verificabile: `pnpm verify` verde; `#[sqlx::test]` sul migrator reale
(`nexus-migrations-embedded`, mai `CREATE TABLE` ricopiati — regola O), con un test
per **ogni** invariante che deve rifiutare: `frontend dev`, `Service`,
`bacheca-attivita` come chiave nel progetto omonimo, due `primary` sullo stesso
servizio, due istanze vive, prenotazione su un `worker`, porta fuori bucket con
`origin='bucket'`. Ogni test deve rosseggiare se si toglie il suo CHECK, col valore
del difetto reale.

Un test in piu', che attraversa la produzione: `projects.port_bucket_start` in
colonna deve coincidere con il valore prodotto da `project_bucket_start()` per ogni
progetto esistente — chiamando la funzione vera, non ricopiandone la formula.

### P2 — Il crate dell'identita' e il decisore puro

`nexus-service-identity` (`ServiceKey`, `UnitName` opaco, sequenza di slot,
`decide`). Nessun call site cambia. Guard testuale nuovo in
`scripts/check-single-source.sh` (`identita-servizio-dichiarata`): vietata qualunque
funzione da `&str` a `ServiceKey`, e vietato `strip_suffix(".service")` accanto a
uno `strip_prefix` dello slug in qualunque file.

Verificabile: batteria del decisore su start, stop, adopt, foreign, brokered,
spec-changed, port-mismatch, unobservable, backoff, crash-loop; per ognuno il test
di mutazione.

### P3 — Importatore in dry-run, con approvazione umana

`xtask services-import --project X` raggruppa le righe esistenti (qui, e **solo**
qui, si riusa `similar_service_labels`) e propone le chiavi, attribuendo a ciascun
gruppo la porta **che ascolta adesso**, non la piu' recente. Output tabellare, righe
non attribuibili dichiarate, archivio in `nexus_port_allocations_archivio` (nessun
`DROP` distruttivo: precedente mig 0563).

Criterio di accettazione, non effetto sperato (innesto da C): **numero di riavvii
provocati dall'import = 0**, misurato. Su `bacheca-attivita`: 2 dichiarazioni,
porte 24804 e 24828, una decisione umana esplicita su 24802 (in ascolto ma senza
scopo dichiarato: o e' un terzo servizio e va dichiarato, o va fermato), 6 righe
archiviate. E' l'unico punto dell'intero piano in cui una decisione umana e'
**obbligatoria**, e va tenuta obbligatoria.

Il criterio di successo sull'intero parco progetti va fissato **prima** di
cominciare, non solo sul progetto che ha prodotto la diagnosi: in particolare il
dry-run deve dichiarare esplicitamente le porte che non ha saputo attribuire a un
`purpose`.

### P4 — Osservatore in ombra

L'osservatore nuovo scrive **solo** `observed_listeners` e la vista; non tocca nulla
che il sistema attuale legga. Vincolo assoluto della finestra: il nuovo osserva e
non scrive nulla di condiviso.

Il gate non e' "zero divergenze inspiegate" — sarebbe un giudizio, e un gate cosi'
passa sempre. Si dichiara **in anticipo** l'elenco chiuso delle classi di
divergenza attese (latenza dello snapshot, listener del bucket senza prenotazione,
processo morto fra le due letture, listener intermediato) e una soglia numerica sul
residuo. Tutto cio' che non cade in una classe nota **blocca** l'avanzamento.

### P5 — Contenimento e avvio nuovo, dietro flag

`services.launcher_v2` in `settings` (regola G, niente env var), su un progetto
pilota. Il flag va accompagnato da un'indicazione **visibile per progetto** nel
pannello: con il flag acceso per un progetto e spento per un altro, i pannelli
mostrerebbero due modelli di verita' nella stessa interfaccia e l'utente non avrebbe
modo di sapere quale sta guardando.

### P6 — Cutover dei produttori, cinque commit distinti

Ordine dichiarato (innesto da C), ognuno con `pnpm verify` e prova di mutazione:

1. `register_detected_port` + `registra_o_audita_porta_rilevata` **rimosse** —
   nessuno puo' piu' riscrivere l'identita' di una riga partendo da un numero di
   porta;
2. `resolve_service_label` sostituita da `ServiceKey::parse` + `declare_service`;
   il tool `run_service` senza dichiarazione ritorna `RefusedUndeclaredService`;
3. `find_or_allocate` sostituita da `reserve_for_service`;
4. le dodici occorrenze dell'inversa rimosse insieme (non hanno piu' oggetto);
5. il GC (`cleanup_orphaned_ports`, `cleanup_dead_process_ports`,
   `release_stale_port`, `cleanup_duplicate_dev_servers`) rimosso, perche' non
   esiste piu' la riga che raccoglieva.

Nello stesso passo: `strictPort` imposto nella configurazione generata dei
framework che ripiegano, e riscrittura della direttiva `<port_allocation>`
"riusa-prima" della mig 0434 — era un prompt che chiedeva al modello di eseguire a
mano, a ogni turno, il controllo che ora e' un vincolo di schema. Un'istruzione nel
prompt e' una speranza; `UNIQUE(project_id, service_key)` e' un fatto.

Enforcement strutturale, non promesso (innesto da B): `REVOKE INSERT, UPDATE ON
nexus_port_allocations` al ruolo applicativo. Un percorso residuo dimenticato deve
presentarsi da solo fallendo rumorosamente, invece di aggiungere la decima riga.

**E lo stesso REVOKE va dove l'invariante I9 vive davvero.** La prima stesura lo
prescriveva sulla tabella VECCHIA e non su `project_services`, che e' quella che
I9 protegge: cosi' «l'osservatore non puo' dichiarare» restava imposto dalla FORMA
della firma (`ManifestReader` separato da `StatusWriter`) e non dalla capacita'
effettiva. Il riconciliatore, per scrivere lo status, possiede comunque una
connessione al DB meta: con quel pool, `sqlx::query("INSERT INTO project_services
...")` compila, passa clippy, e nessun tipo lo vede. Una convenzione vestita da
tipo.

Percio' l'osservatore gira con un **ruolo DB proprio** — `nexus_observer` — a cui
si fa `REVOKE INSERT, UPDATE, DELETE ON project_services, service_port_reservations,
port_exceptions` lasciando il solo `SELECT`, piu' `INSERT/UPDATE` su
`observed_listeners` e `service_instances`. Il tentativo di coniare un'identita'
dall'osservazione non fallisce in revisione: fallisce in esecuzione, con un errore
di permessi che nomina la tabella.

Verificabile, e va verificato: un test che apre una connessione col ruolo
dell'osservatore e pretende che l'`INSERT` su `project_services` sia RIFIUTATO.
Senza quel test il REVOKE e' una riga di migrazione che nessuno esercita.

### P7 — Cutover dei lettori

`nexus_port_allocations` diventa una **vista di sola lettura** sulle prenotazioni,
cosi' i lettori residui continuano a funzionare mentre si migrano uno per uno. La
vista deve **tradurre** nel vocabolario che si sta ritirando: `resource_linter`
legge ancora `allocation_mode` e `nexus-tool-kit::ports` confronta con
`ALLOCATION_MODE_MANUAL`. La mappatura (`origin='deroga'` -> `'manual'`,
`origin='bucket'` -> valore accettato dai lettori) va **esplicita e coperta da
test**: senza, durante la finestra il linter comincerebbe a segnalare come abusive
porte perfettamente legittime, su un pannello che l'utente ha gia' imparato a non
fidarsi.

Poi si migrano pannello Porte, pannello Servizi, `port_enforcer`,
`resource_linter`, sandbox, gate di readiness e blocco RISORSE PROGETTO alla vista
`service_status`, e si rimuove la vista di compatibilita'. Nello stesso passo si
dichiara **chi legge** gli esiti `PortMismatch`, `PortHeldByForeign`,
`StaleGeneration`, `Unobservable` e con quale conseguenza: senza consumatore, il
risultato netto sarebbe un sistema che sbaglia meno e si lamenta di piu'.

### P8 — Rimozione, ratchet, rimisura

Rimozione del flag e del codice morto; `similar_service_labels` cancellato **nello
stesso commit** che chiude l'importazione (senza una data, una funzione deprecata
che indovina un'identita' e' una funzione che qualcuno richiamera'); baseline jscpd
e `markers-ratchet` riallineate al ribasso; voce nel catalogo dei punti unici
(ADR 0026).

Rimisura del censimento P0 a **7 e 30 giorni** su tutto il parco progetti, con una
sola metrica: **righe di identita' per servizio reale**. Se non e' 1, il disegno ha
una falla e la si cerca, non la si spiega. Seconda metrica, sull'attrito:
**avvii rifiutati per dichiarazione mancante** nei primi giorni; se sale invece di
scendere, la cura sta producendo una regressione di esperienza e va saputo in tempo.

## Rischi accettati

**Il perno e' una capacita' della piattaforma che oggi non usiamo.** Zero
occorrenze di Job Object nel repo, verificato. Windows non ha il cgroup: il Job
Object da' contenimento vero solo per i processi che avviamo noi, e non sopravvive
a noi nel modo in cui un cgroup sopravvive a systemd. Il ripiego e' il marcatore
con la terna, che e' una prova piu' debole; un launcher che rilancia il server vero
come nipote la rende inconcludente. Conseguenza onesta e accettata: dopo certi
riavvii un servizio vivo verra' dichiarato non provato e servira' una conferma
umana. La variante `Unknown` sara' piu' frequente di quanto si vorrebbe, e il
disegno deve renderla **innocua** (non adottare, dichiarare) invece di ridurla con
euristiche — che e' esattamente come si e' arrivati alle nove identita'.

**Il TOCTOU della porta non si chiude.** Prenotare non e' bindare, e la finestra
dura quanto l'avvio del processo. Il trucco `TIME_WAIT` + `SO_REUSEADDR` non e'
portabile su Windows, e la port reservation di Winsock richiede la cooperazione del
processo, che Vite non dara'. Restano bucket per progetto, `strictPort` nella
configurazione generata e la verifica post-avvio con `PortMismatch` dichiarato. La
finestra non si chiude: si rende visibile.

**Attrito nuovo per l'agente.** Oggi lancia un comando e il sistema indovina;
domani deve dichiarare. Il rifiuto porta la chiamata gia' compilata e un
`run_command` che apre un listener del bucket produce un `unclaimed` **visibile**,
non un avvio rifiutato ne' un processo ucciso — ma il rischio di una regressione di
esperienza nelle prime settimane e' reale e va **misurato**, non assunto.

**Il disegno impedisce che nascano identita' non chieste, non impedisce a chi puo'
chiedere di chiedere male.** Un agente puo' dichiarare `frontend` e poi `frontend2`.
L'unico anticorpo ammissibile e' il confronto su `(command, working_dir)`
normalizzati al momento della dichiarazione, per proporre una chiave gia'
esistente: mai il confronto di somiglianza fra nomi, che e' il metodo che ha
prodotto il problema. Il caso misurato va da 6 identita' a 2, non a 1 garantito.

**Il vocabolario `purpose` e' chiuso per scelta.** Un Vite con HMR su porta
separata, o un servizio con HTTP piu' gRPC, non entra sempre in
`primary`+`secondary` con una semantica difendibile. Se la pressione porta ad
allargare il CHECK caso per caso, si ricostruisce per accumulo la lista di varianti
che la regola H vieta. Il prezzo scelto e': una migrazione versionata per ogni
`purpose` nuovo.

**Il periodo di doppia verita' (P4-P7) e' la finestra pericolosa.** Due sistemi che
guardano lo stesso mondo, e per un tratto il vecchio scrive ancora. Vincolo
assoluto gia' dichiarato; il rischio residuo e' che qualcuno lo violi per comodita'.

**Il costo e' alto e il mezzo disegno sarebbe peggio dell'attuale.** Circa 5.800
righe stimate fra nuovo e riscritto, piu' migrazione dati, prompt e pannelli. Da
qui la condizione di non-inizio.

**Il backfill perde informazione.** Nove righe verso due dichiarazioni: le sette
archiviate portavano date e una storia. L'archivio costa poco ma e' debito che
restera' li' a lungo, e nessuno lo guardera'.

**I servizi senza porta hanno un contratto di salute povero.** Worker, watcher,
`tsc --watch` sono rappresentati (`kind='worker'`, nessuna prenotazione) ma il loro
probe si riduce a "pid vivo", che e' il segnale debole su cui il sistema attuale
gia' sbaglia. Non c'e' una risposta migliore e non si finge di averla: il modello
li rappresenta onestamente come non pienamente verificabili.

## Cosa viene eliminato

Non deprecato: **rimosso**. Una funzione deprecata che indovina un'identita' e' una
funzione che qualcuno richiamera'.

- `agent_tools/service.rs` — l'intera macchina che indovina lo scopo:
  `resolve_service_label`, l'enum `ServiceIdentity` con i suoi tre rami,
  `LABEL_NON_SERVIZIO` e la stringa `Service`, `derive_kind_hint` (la lista di
  `contains` su vite/express/server.js), `identita_dal_percorso`,
  `normalizza_label`, `looks_like_web_service`, il ripiego `service-{uuid[..8]}`,
  `scope_from_work_dir`/`scope_dir`/`directory_effettiva` nella parte che serve a
  dedurre il nome, e la lettura del ruolo dalla command line (`cd_dichiarato`,
  `argomento_cd`, `sfila_apici`, `spezza_al_separatore`).
- `agent_tools/service.rs` — la scrittura dell'identita' dall'osservazione:
  `register_detected_port`, `registra_o_audita_porta_rilevata`,
  `detect_port_from_output` (la porta smette di essere letta dallo stdout: la
  sappiamo perche' l'abbiamo prenotata).
- `agent_tools/service.rs` — la manutenzione di un registro che non esiste piu':
  `dedup_and_cleanup_ports`, `cleanup_dead_process_ports`, `release_stale_port`,
  `free_listening_scope_port`, `refuse_if_same_scope_active`.
- `project_workspace/allocate_port.rs` — `find_or_allocate` con i suoi quattro rami
  (riuso per uguaglianza esatta della stringa, riuso per appartenenza raggiungibile
  solo a servizio acceso, ramo stale che ri-timbrava `adopted`, `INSERT 'dynamic'`
  su porta nuova) e `link_allocation_to_service_unit` (l'unit e' una colonna
  generata). Sopravvive l'alloca-e-inietta come passo del flusso di avvio, con il
  vincolo di bindabilita' che era gia' corretto.
- `port_registry.rs` — la parte di **scrittura** e raccolta: `allocate`, `release`,
  `startup_recovery` (l'avvio che svuotava il registro), `cleanup_orphaned_ports`,
  `port_gc_loop`, `extract_ports_from_unit_content` (leggere la porta dal **testo**
  di una unit e' l'inversa vietata in un'altra forma). La scansione del sistema
  operativo resta come **sensore**, non come secondo registro che contraddice il
  primo.
- `project_workspace/service_ownership.rs` — l'appartenenza per indizi:
  `classify_ownership`, `identifying_label`, `process_label_for_pid`,
  `port_label_for_port`, `owned_listener`, `resolve_stale_adoption`,
  `StaleAdoption`. Il tipo `ServiceOwnership{Own|Other|Unknown}` era gia' corretto
  come **vocabolario** e sopravvive in `Membership`: cambia la prova, che diventa un
  dato scritto da noi allo spawn.
- Le **dodici occorrenze** dell'inversa unit -> label nei sei file elencati nel
  contesto. Non si toglie lo stesso bug dodici volte: si toglie la **domanda**. Chi
  ha una unit fa una lookup.
- `agent_processes.rs` — `is_generic_service_label`, `similar_service_labels`,
  `shared_significant_words`, `GENERIC_SERVICE_WORDS`, `stop_similar_running_services`,
  e la colonna `label` come attributo scrivibile del processo (sostituita da
  `service_id` + `instance_id`). L'apparato che stabilisce se due nomi sono lo
  stesso servizio non serve piu', perche' il nome e' uno solo. Sopravvive dentro il
  solo importatore di P3 e muore con esso.
- `nexus-tool-kit/src/ports.rs` — la parte di autorizzazione: `PortRegistrability`,
  `port_authorized_for_project`, `allocation_authorizes_port`,
  `ALLOCATION_MODE_MANUAL`. Sopravvive il bucket, che diventa la sorgente di
  `projects.port_bucket_start`. L'autorizzazione si riduce a: esiste la
  prenotazione, oppure esiste la deroga.
- `service_observer.rs` — la parte edge-triggered (apertura e chiusura di diagnosi
  legate all'evento di avvio, `resolve_open_crashes` che marcava risolto al cambio
  del marcatore d'avvio). Sopravvive la diagnostica sui **log**, che risponde a
  un'altra domanda.
- `service_recovery.rs` — sopravvive quasi intero ed e' la parte migliore
  dell'esistente: `judge_recovery`, `ServiceHealth`, `stable_enough`,
  `restart_and_verify`, `apply_recovery_verdict`. Il contratto di successo che quel
  modulo aveva gia' formulato bene — stato `Running` **e** porta della unit che
  risponde stabilmente, e di nuovo dopo un ulteriore riavvio — non viene buttato:
  diventa la definizione di `running` **per tutti**, invece di valere solo dentro la
  remediation, e si applica alla vista invece che a un registro che si autoassolveva.
- Vocabolario: `allocation_mode` da cinque valori a due, la colonna `label` come
  chiave d'identita', la colonna `service_unit`, e l'indice
  `UNIQUE(project_id, label)` della mig 0434 — rendeva idempotente una stringa
  libera, cioe' garantiva l'unicita' della chiave sbagliata.

## Punti unici introdotti (per il catalogo dell'ADR 0026)

| Concern | Modulo/funzione autoritativa |
|---|---|
| "Chi e' questo servizio?" | `project_services.service_key` + `nexus-service-identity::ServiceKey` |
| Nome della unit (e sua assenza di inversa) | colonna GENERATED `project_services.unit_name` + `UnitName` opaco |
| "Questa porta e' sua?" | `service_port_reservations` (`reserve_for_service`) |
| "Questo processo e' suo?" | `containment_proof` -> `Membership` (Job Object, marcatore+terna, brokered) |
| "Cosa fare, dato desiderato e osservato?" | `nexus-service-identity::decide` (puro) |
| Stato di un servizio per i lettori | vista `service_status` |

## Riferimenti

- Codice attuale citato e verificato il 02/08/2026:
  `crates/mcp-core/src/agent_tools/service.rs` (555, 629, 975-984),
  `crates/mcp-core/src/project_workspace/allocate_port.rs` (101, 320, 371),
  `crates/mcp-core/src/project_workspace/services.rs` (2539
  `deterministic_project_port_for_key`, e le sei occorrenze dell'inversa),
  `crates/mcp-core/src/project_workspace/compose_ports.rs` (22, 83),
  `crates/nexus-tool-kit/src/ports.rs` (62 `project_bucket_start`),
  `db/migrations/0114` (`UNIQUE(port)`), `db/migrations/0434`
  (`UNIQUE(project_id,label)` e direttiva `<port_allocation>`).
- [[0026-punto-unico-de-duplicazione]] — catalogo dei punti unici e meccanismo.
- [[0038-modello-servizi-progetto-multipiattaforma]] — `ServiceManager` e il segnale
  strutturato `acted`; questo ADR ne eredita il confine (solo servizi **di
  progetto**; l'infrastruttura Nexus resta al `services_watchdog`) e ne sostituisce
  la nozione di identita'.
- [[0027-single-instance-per-porta]] — precedente diretto sul principio "una sola
  istanza per porta", qui esteso da lock di processo a vincolo di schema.
- [[0010-port-and-attachment-enforcement]] — bucket delle porte ed enforcement.
- [[0018-segnali-strutturali-vs-euristiche-testuali]], [[0034-esito-conversazione-strutturato-finish-task]] —
  regole M e Q, che questo ADR applica al confine "chi avvia un servizio".
- `CLAUDE.md` regole E (isolamento progetti), G (niente hardcoded, DB unica fonte),
  H (fix definitivi), L (punti unici), N (identificatori canonici), O (lo strumento
  di misura raggiunge il suo oggetto come la produzione), P (lavoro non committato),
  Q (l'esito in un campo).
