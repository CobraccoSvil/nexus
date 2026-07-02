# CLAUDE.md — Regole di contribuzione Nexus / IDEAI

Questo file raccoglie le direttive vincolanti per qualunque agente (umano o AI) che contribuisca al repository. Le regole sono autoritative: sovrascrivono comportamenti di default.

## A. Autonomia agente e lettura chirurgica

- Niente emoji in file sorgente, commit message, changelog, documentazione o report generati.
- Leggere i file in modo chirurgico: usare `Grep`/`Glob` e letture parziali (`offset/limit`) prima di caricare file interi. Evitare il ciclo "read intero file -> modifica puntuale".
- Le modifiche devono essere `Edit`/`Write` con `old_string` unico e ben delimitato. Rifiutare patch speculative.
- Non duplicare documentazione: riferire le migrazioni SQL esistenti (`db/migrations/0035*`, `0037*`, `0064*`) invece di riscriverne il contenuto.

## B. Build verification (direttiva #9)

- Dopo ogni modifica non banale eseguire `pnpm verify` (orchestratore: `scripts/verify.sh`).
- `pnpm verify` esegue: `turbo run typecheck lint test` + `cargo check --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace --no-fail-fast`.
- Un commit che rompe `pnpm verify` non può essere unito in `main`. L'hook `lefthook` pre-commit lo blocca localmente.
- CI: `.github/workflows/verify.yml` esegue lo stesso gate su ogni push/PR.

## C. Anti-loop e igiene delle chiamate

- Niente `read_file` loop: se il file è già noto, modificare direttamente.
- Batch di `Edit` sullo stesso file in un singolo turno quando possibile.
- Parametri espliciti e corretti: ogni tool call deve avere path assoluti e argomenti coerenti con la firma.
- Niente `--no-verify`, `--amend` su hook falliti, `reset --hard` senza permesso esplicito.

## D. Prompt: ridondanza vs canale di invocazione

Distinguere sempre il canale di invocazione del prompt prima di decidere quanto controllo metterci dentro.

### Sotto chat (input utente, pulsanti che scrivono in chat)
- I dropdown `Memoria`, `Auto/modello`, `Confirm/Automatico/Continuo` dell'AI Workspace governano gia' il comportamento dell'agente.
- Il prompt utente deve essere **task-only**: descrive cosa, NON come.
- Vietato ripetere "lavora autonomamente", "procedi senza chiedere conferma", "mostra il diff finale" — sono ridondanti e producono rumore (l'LLM puo' interpretarle come richiesta di "extra autonomia").
- Esempio corretto (pulsante "Risolvi con Nexus" su un config_issue): solo descrizione del problema + fix suggerito + "valida che il fix sia corretto e applicalo, segnalando alternative".

### Fuori chat (REST diretti al brain, worker schedulati, batch, reflection)
- NESSUNA modalita' UI viene ereditata. Il prompt e' l'unico contratto.
- Istruzioni di autonomia, anti-loop, output format, examples, reflection devono essere **esplicite** nel prompt stesso.
- I prompt agente in `nexus_prompt_templates` (chiavi `agent.*`, `system.*`) seguono lo schema XML standard (`<role>` / `<contesto>` / `<autonomia>` / `<protocollo>` / `<tool_usage>` / `<anti_loop>` / `<output_format>` / `<examples>` / `<reflection>`) — vedi migrazione 0086.
- Sono call site fuori chat (audit a riferimento, non esaustivo): `brain/grpc_server/main.py` `/agent/project-analyze`, `crates/mcp-core/src/prompt_templates.rs:404,501,549`, `crates/mcp-core/src/orchestrator.rs:1210`, qualunque worker in `crates/nexus-orchestrator/src/workers/`.

### Conseguenze pratiche
- Quando aggiungi un nuovo pulsante UI che invia un messaggio alla chat: prompt minimal, niente istruzioni di processo.
- Quando aggiungi un nuovo endpoint o worker che chiama LLM senza UI: prompt completo, mai assumere autonomia ereditata.
- I system prompt agente in DB **mantengono** `<autonomia>` anche se invocati da chat: il costo (pochi token ridondanti) e' minimo, il beneficio (sicurezza fuori chat) e' alto.

## E. Isolamento progetti e safety Docker

Ogni progetto registrato in Nexus e' un mondo a se' — risorse, config, container, servizi systemd, file e dati appartengono solo a quel progetto.

- **Scope al progetto attivo**: operare esclusivamente dentro la `project_root` del run corrente. Per lavorare su un progetto diverso serve richiesta esplicita dell'utente nel turno.
- **Cleanup Docker filtrato per progetto**:
  - vietato `docker stop $(docker ps -q)`, `docker system prune`, `docker compose down` su compose globali
  - permesso solo `docker compose -f <PATH_COMPOSE_PROGETTO> down`, `docker stop <NOME_CONTAINER>` con nome esatto, oppure filtro `--filter "label=com.docker.compose.project=<SLUG>"`
- **Container `ideai-*` sono infrastruttura Nexus, intoccabili** (`ideai-postgres-nexus-1`, `ideai-qdrant-1`, `ideai-redis-1`, `ideai-grafana-1`, ecc.). Mai fermarli/rimuoverli da agenti operanti su progetti utente.
- **I file del repo meta-progetto Nexus** (`D:\IDEAI`) appartengono a Nexus; modifiche solo se l'utente sta esplicitamente lavorando su Nexus.
- **Letture massive ricorsive fuori root progetto vietate** (rumore + rischio leak). Letture puntuali ammesse per debugging.

Lo stesso vincolo e' replicato come tag `<safety_progetto>` nei system prompt agente principali (`system.nexus_base`, `agent.coder.base`, `agent.general.debugger`) — vedi migrazione `0096_project_isolation_rules.sql`.

## F. Produzione-ready, test indipendenti

- Ogni test deve essere idempotente e non dipendere dall'ordine di esecuzione o da stato condiviso non resettato.
- Nessun leak in log: `tracing::*!` non deve ricevere campi `payload`, `prompt`, `response` in chiaro. Usare hashing o redaction.
- Errori propagati con `thiserror`/`anyhow`: `unwrap()`/`expect()` ammessi SOLO dentro `#[cfg(test)]` e `tests/`.
- Feature flag e sensitivity tier rispettati: le policy in `config/policies/*.yaml` sono il contratto; testarne le ramificazioni critiche.

## G. Modelli AI mai hardcoded — registry DB unica fonte di verita'

I nomi dei modelli AI (`mistral-small-latest`, `gemini-2.5-flash`, `claude-haiku-4-5-20251001`, `gpt-4o-mini`, `deepseek-chat`, ecc.) **non vanno mai hardcoded** nel codice Rust o Python. **Niente env var, niente fallback hardcoded, niente default di emergenza**: la configurazione ha UN solo posto, il DB.

### API
- **Rust** (cache 60s, refresh background):
  - `state.orchestrator.routing_matrix.current_async().await?` ritorna `Result<Arc<RoutingMatrix>, String>` — propaga errore (HTTP 503) se DB down
  - `matrix.lookup(intent, mode)` per routing utente
  - `matrix.default_model(provider)` per default per provider
  - `internal_routing::resolve_purpose_model(state, purpose)` per task interni (chat title, doc gen, ecc.) — tier-aware: un purpose con `tier` valorizzato ignora il `model_id` statico
- **Python** (cache 60s):
  - `_load_analyzer_provider_chain()` solleva `AnalyzerChainUnavailable` se DB down
  - `_default_model_for_provider(provider)` solleva `DefaultModelUnavailable` se DB down o provider non configurato
  - `load_provider_catalog(provider)` in `brain/providers/catalog_loader.py` solleva `ProviderCatalogUnavailable` se DB down o tabella vuota

### Schema DB (tabelle uniche fonti di verita')
- `nexus_routing_matrix` (intent x behavior_mode → provider+model) — mig **0101**
- `nexus_provider_default_model` (provider → model) — mig **0101**
- `nexus_purpose_model` (purpose → provider+model) per task interni — mig **0102**
- `ai_price_catalog` (provider+model → costi+capabilities) per `list_models()` e `detect_model_switch`

### Comportamento se DB down
- **All'avvio**: `RoutingMatrixCache::init()` esegue retry-loop di **5 tentativi × 5 secondi** (totale 25s). Se dopo 5 tentativi il DB e' ancora irraggiungibile o le tabelle vuote, mcp-core **PANICA** all'avvio con messaggio chiaro che indica quali migrazioni applicare.
- **A runtime** (DB cade dopo l'avvio): la cache mantiene l'ultima matrice valida; il refresh fallisce in background ma loggato come WARN. Se tutta la cache e' ricreata mentre il DB e' down (mai in pratica), gli handler ritornano HTTP **503 Service Unavailable** con messaggio esplicito.

### Niente "magic fallback"
**Vietato** scrivere `unwrap_or_else(|| "claude-sonnet")` o `data.get("model", "gemini-2.5-flash")`. Se la configurazione manca, il sistema deve fallire visibilmente — un fallback nascosto produce bug subdoli (es. il sistema usa silenziosamente il modello sbagliato dopo che l'admin ha "rimosso" un provider dalla tabella).

### Quando un modello viene deprecato
Es: Mistral ha rinominato `mistral-small-4` → `mistral-small-latest`, l'API ritorna 400 invalid_model in prod.

Soluzione: `UPDATE nexus_routing_matrix SET model_id = 'mistral-small-latest' WHERE model_id = 'mistral-small-4'`. Attendi ≤60s per il refresh cache. **Niente patch codice, niente redeploy, niente env var da cambiare.**

## H. Fix definitivi, mai toppe

Regola autoritativa, valida per qualunque incidente, bug report, errore in produzione o richiesta utente: **non si applica mai un fix immediato come toppa**. Si va sempre alla causa radice e si chiude il problema lì, anche se richiede più tempo.

### Cosa è una toppa (vietata)

Sono toppe — e quindi vietate — i seguenti pattern, anche quando "funzionano":

- **UPDATE/INSERT SQL ad-hoc** per aggirare un comportamento sbagliato di un codice (es. `UPDATE catalog SET is_enabled=false WHERE model='foo'` perché `foo` non funziona). Il fix definitivo è capire *perché* `foo` è entrato nel catalog e correggere il `catalog_sync` o il provider che lo emette.
- **Aumento di timeout** per nascondere una latenza patologica (es. classifier timeout 5→8s perché Vertex va in cold start). Il fix definitivo è precaricare il provider, predicare il modello veloce, o togliere il provider dalla chain se inadatto.
- **Disabilitazione di provider/modello dal DB** come azione manuale ricorrente. Il fix definitivo è una policy di auto-disable basata su metriche (`billing_error` → cooldown automatico; N fallimenti consecutivi → escludere).
- **kill -9 + restart** per sbloccare un servizio. Il fix definitivo è capire perché il graceful shutdown si è bloccato (worker stuck, channel mai chiuso, await senza cancellation token) e correggerlo nel codice.
- **Modifica diretta di file di config / env var** sul filesystem dell'utente per aggirare un comportamento di default sbagliato. Se il default è sbagliato, va corretto il codice che produce il default.
- **try/except che inghiotte un errore** "per non far crashare il servizio". L'errore va capito e gestito esplicitamente, oppure deve davvero risalire al chiamante con uno status code coerente.
- **Hardcode di valori "che ora vanno bene"** (es. nome modello, URL provider, soglia di chunking) dentro la logica di business. Tutto ciò che è configurabile va nel DB (`settings`, `nexus_*`) con TTL/cache documentato.
- **Restart manuale che diventa abitudine**. Se devi restartare un servizio per farlo riprendere, c'è un fix architetturale dietro (memoria leak, deadlock, cache corrotta).

### Cosa è un fix definitivo (richiesto)

Un fix è definitivo quando:

1. Risolve la causa nel codice o nello schema, non l'effetto.
2. Sopravvive a un riavvio, a un deploy, a un wipe del DB e re-applicazione delle migrazioni.
3. È testato (`pnpm verify` passa) e ha almeno un test che cattura la regressione.
4. È documentato con un commit message che descrive il root cause, non solo il sintomo.
5. Se richiede dati nuovi/modificati nel DB, è veicolato da una **migrazione SQL versionata** (`db/migrations/NNNN_*.sql`), non da un `psql -c "UPDATE ..."` lanciato a mano.
6. Se modifica un comportamento configurabile, espone il flag in `settings` o `nexus_*` (vedi sezione G) ed elimina ogni fallback hardcoded.

### Workflow quando arriva un sintomo

1. **Diagnostica fino al codice / migrazione / config sorgente del problema.** Non fermarsi all'osservazione che "se cambio X il sintomo sparisce".
2. **Identifica il fix architetturale** e stimalo. Se richiede un'ora o meno: implementalo subito. Se richiede di più: apri un task descrittivo e chiedi all'utente come prioritizzarlo.
3. **Se l'utente è bloccato** e non è ragionevole fargli aspettare il fix completo, il workaround temporaneo è ammesso *solo se*:
   - L'utente lo richiede esplicitamente con piena consapevolezza ("dammi un workaround mentre sistemi la causa").
   - Viene immediatamente aperto un task per il fix definitivo, con priorità chiara.
   - Il workaround è etichettato come tale nei commit (`workaround:` o `temp:`), mai come `fix:` o `feat:`.
   - C'è un piano (data o trigger) per rimuovere il workaround.
4. **Senza richiesta esplicita dell'utente, niente workaround.** Si va al fix definitivo.

### Esempi pratici (riferiti a incidenti reali in questo repo)

| Sintomo | Toppa (vietata) | Fix definitivo (richiesto) |
|---|---|---|
| Anthropic ritorna `billing_error` ogni chiamata | `UPDATE matrix SET is_active=false WHERE provider='anthropic'` | Aggiungere detection di `billing_error` nel `provider_health_probe` con auto-disable su tutta la routing matrix; ripristino automatico al primo 200 successivo |
| Classifier Google va in timeout su cold start | `UPDATE settings SET value='8.0' WHERE key='routing.llm_classifier_timeout_seconds'` | Precaricare Vertex SA all'avvio brain (`startup_event`) o spostare Google fuori dalla chain finché non c'è warming pool |
| `gemini-3.5-flash` appare nel catalog e fallisce | `UPDATE catalog SET is_enabled=false WHERE model='gemini-3.5-flash'` | Correggere `catalog_sync_loop` che lo include: filtro per modelli `chat`-compatibili o whitelist da Google API live |
| `mcp-core` resta in `deactivating` 1+ min al restart | `pkill -9` + restart | Tracciare il task tokio che non risponde a SIGTERM, aggiungere cancellation token, mettere `TimeoutStopSec=10` sull'unit systemd come safety net |
| Body 2MB rifiutato per file allegati | Alzare a 25MB nel codice frontend | OK come fix definitivo se accompagnato da `DefaultBodyLimit::max(...)` esplicito sul backend (vedi commit "feat: chat upload limit") |

### Conseguenza pratica per gli agenti

Prima di proporre un fix, chiediti:
- *Sto sistemando la causa o sto mascherando il sintomo?*
- *Se domani arriva un nuovo modello rotto / un nuovo provider con billing zero / un nuovo cold start lento, il mio fix di oggi mi servirà a qualcosa?*

Se la risposta a una delle due è "no, ma intanto sblocca l'utente", **fermarsi e ripartire dalla causa**. Comunicare onestamente all'utente: "il fix richiede N minuti/ore in più, procedo lo stesso al fix definitivo perché un workaround creerebbe debito tecnico". Solo se l'utente esplicitamente preferisce sbloccarsi subito, applicare workaround temporaneo seguendo le regole sopra.

## I. Allocazione porte e accesso allegati

Vedi [ADR 0010](docs/.nexus-vault/adr/0010-port-and-attachment-enforcement.md)
per il razionale completo. Punti operativi:

- **Porte hardcoded vietate nei sorgenti**: `write_file`/`edit_file` su file
  che NON siano `.env*` / `docker-compose*.yml` / `Dockerfile*` vengono
  rifiutati se contengono `app.listen(NNNN)`, `bind("...:NNNN")`, `listen=NNNN`
  o `PORT = NNNN` con porte fuori dal bucket 20000-39999 e diverse da quelle
  riservate (<1024). Eccezione: righe che leggono la porta da env
  (`process.env.PORT`, `os.environ.get("PORT")`, `env::var("PORT")`,
  `getenv("PORT")`, `PORT=$`, `PORT=${`). Il flusso corretto e' sempre
  `request_port(label=...)` -> usa il valore ritornato.
- **Flag enforcement**: `settings.key = 'agent.enforce_port_allocation'`
  (default `'true'`). Disattivabile per debug locale, mai per produzione.
  Cache 60s lato Rust, niente env var (regola G).
- **Allegati grandi**: il blocco `<allegati>` nel prompt iniziale ha budget
  inline 50KB totali (30KB per singolo file). Sopra soglia mostra solo metadata
  e impone all'agente di chiamare `nexus_list_attachments` ->
  `nexus_read_attachment(attachment_id, offset?, length?)` (max 100KB per
  chiamata, encoding `auto|text|base64`).
- **Mai inventare contenuti di allegati non letti**: la direttiva
  `<attachment_access>` nei system prompt `system.nexus_base` /
  `agent.coder.base` (mig 0192) lo ricorda esplicitamente.


### Investigazione allegati e vision routing

Quando un agente vede un allegato, NON deve assumere che il contenuto sia gia
nel prompt. Workflow:

1. `nexus_inspect_attachment(attachment_id)` — magic byte detection. Ritorna `kind`, `mime_reale`, `extraction_tools`.
2. Usa il tool appropriato in base al kind:
   - `zip|tar|gzip` -> `nexus_list_archive_entries` + `nexus_read_archive_entry`
   - `pdf` -> `nexus_extract_pdf_text`
   - `docx|xlsx|pptx` -> `nexus_extract_docx_text` / `nexus_extract_xlsx_data`
   - `figma` -> `nexus_extract_figma_structure`
   - `image_*` -> `nexus_describe_image_attachment` (chiama modello vision configurato in `nexus_purpose_model.vision_describe`)
   - `text|json|markdown|...` -> `nexus_read_attachment` con encoding=text
   - `binary` opaco -> ultimo resort `nexus_read_attachment` con encoding=base64

Smart routing vision: se il messaggio utente contiene allegati `image/*` il brain router preferisce automaticamente un modello con `capabilities.vision=true` (override sulla routing matrix). Configurabile via `nexus_purpose_model` chiave `vision_describe`. Modello di default: `google/gemini-2.0-flash-exp` (mig 0194).


## I. Pipeline allegati robusta (ADR 0012)

Quando l'utente carica un allegato (PDF, DOCX, Figma, ZIP, immagine, ecc.) l'agente deve seguire la pipeline definita in ADR 0010 + 0011 + **0012** senza scorciatoie.

- **Pre-extraction automatica** (FIX 3): per PDF/DOCX/ZIP-con-canvas.fig il blocco <allegati> del primo messaggio gia' contiene un sub-blocco ### Pre-extracted content. Il modello NON deve chiamare nexus_inspect_attachment / nexus_extract_* per ottenere informazioni che sono gia' visibili.
- **`nexus_inspect_attachment` quando serve**: il tool ora ritorna `next_action_recommended` con `{tool, input, rationale, expected_tokens_output}`. **Dopo** averlo chiamato, l'agente deve chiamare ESATTAMENTE quel tool con quegli input. Vietato chiamare `nexus_read_attachment` / `nexus_read_archive_entry` con offset crescenti su file binari.
- **Cache deduplica** (FIX 2): chiamate identiche a read_attachment / read_archive_entry vengono servite dalla cache. Se il payload include `from_cache: true` + `hint`, l'agente deve cambiare strategia (passare a un tool di estrazione strutturata o a una entry diversa).
- **Budget letture per sessione** (FIX 4): max 500 KB cumulativi (default DB) per nexus_read_attachment + nexus_read_archive_entry. Oltre la soglia il brain ritorna un tool_result sintetico che invita a usare gli estrattori strutturati.
- **Tuning DB-driven**: i 4 setting in `agent.attachment.*` (preextract_enabled, preextract_max_chars, session_read_budget_bytes, read_cache_ttl_seconds) governano l'intera pipeline. Niente fallback hardcoded nel codice (regola G).

## L. Punti unici di controllo (un solo punto di verita' per logica)

Regola autoritativa, valida per qualunque concern trasversale: **ogni decisione o
logica deve avere UN solo punto di controllo (una funzione/modulo autoritativo); i
call site delegano a quello, non re-implementano la stessa logica.** Generalizza G
(unica fonte dati nel DB) e H (un punto di enforcement) a tutta l'architettura.

### Cosa e' vietato

- **Logica duplicata/dispersa**: la stessa decisione implementata in piu' punti
  (es. piu' query SQL diverse che selezionano "il modello giusto" con filtri
  copiati a mano in `best_model_for_tier`, re-route context-aware, cascade
  fallback, gate, ecc.). Se due punti devono rispondere alla stessa domanda,
  devono chiamare la **stessa** funzione.
- **Aggiungere un filtro/condizione in N posti** quando arriva un nuovo requisito
  (es. spargere `agentic_thinking_policy <> 'exclude'` in ogni query): e' il
  sintomo che manca il punto unico. Prima si crea/usa la funzione autoritativa,
  poi si aggiunge il requisito UNA volta li' dentro.
- **Copiare e adattare** una funzione esistente invece di estrarne una versione
  parametrica riusabile.

### Cosa e' richiesto

1. Prima di scrivere una nuova funzione/query che decide qualcosa, cercare se
   esiste gia' il punto autoritativo per quel concern e **delegare** ad esso.
2. Se la logica e' gia' duplicata, **consolidarla** in un unico punto e far
   convergere i call site (vedi WikiAcl, gate di routing ADR 0020, fonte unica
   capability ADR 0024 come esempi del pattern corretto).
3. Il punto unico e' parametrico (input espliciti) e testato una volta sola;
   estenderlo li' copre automaticamente tutti i chiamanti.
4. Vale per tutti i layer: selezione provider/modello, capability, cooldown,
   classificazione, validazione, accesso DB ripetuto, costruzione prompt.

### Conseguenza pratica

Se un fix richiede di toccare "lo stesso `if` in piu' file", **fermarsi**:
significa che il punto unico non esiste ancora. Crearlo (o consolidare l'esistente)
e applicare il requisito una sola volta. Un PR che introduce logica dispersa
duplicata e' rifiutato come una toppa (regola H).

### Meccanismo di centralizzazione (criterio per-caso)

Il punto unico e' agnostico rispetto al meccanismo, ma la scelta NON e' libera:
si applica "composition over inheritance". L'ereditarieta' di classi si usa SOLO
per relazioni "is-a" reali e poco profonde, mai per riusare codice (Rust non ha
ereditarieta' di classi; in React ereditare componenti e' anti-pattern).

| Natura della logica | Meccanismo corretto | Esempio |
|---|---|---|
| Stateless (calcolo puro, IO singolo) | funzione in un modulo | `get_setting`, `parse_user_id`, `extract_json_block` |
| Stato + comportamento | classe/struct incapsulata + generics | `TtlCache<K,V>` (Rust), `db_pool` (Python) |
| Varianti polimorfiche su contratto comune | `trait` (Rust) / ABC-Protocol (Python) + composizione | provider su `brain/providers/base.py` |
| UI | composizione (componenti + custom hooks) | `AdminPageHeader`, `useListData` |

Anti-pattern vietati: incapsulare una funzione stateless in una classe con
sottoclassi ("regno dei sostantivi"); gerarchie di ereditarieta' profonde per
condividere codice (fragile base class).

### Punti unici noti (catalogo sintetico, dettaglio in ADR 0026)

| Concern | Modulo/funzione autoritativa |
|---|---|
| Gate disponibilita' provider | ADR 0020 |
| SQL-injection detector | ADR 0021 |
| Capability modello (vision/tool/thinking) | vista `0318` + `mcp-core/src/capability.rs` (ADR 0024) |
| Routing/default/purpose model | `routing_matrix.rs` + tabelle mig 0101/0102 (regola G) |
| Identita' utente/progetto | `crates/nexus-types/src/lib.rs` (`parse_user_id`, ...) |
| Lettura settings | `nexus-auth::settings` (`get_setting`) |
| Cache TTL | crate `nexus-cache` (`TtlCache<K,V>`) |
| Fetch HTTP frontend | `apps/web-ide/lib/api/_shared.ts` (`fetchJson`) |

### Enforcement automatico (la regola e' duratura, non una-tantum)

- `jscpd.json` + `scripts/dup-report.sh`: misura cross-linguaggio (TS/JS/Rust/Python)
  con gate "ratchet" — il numero di cloni puo' solo SCENDERE rispetto a
  `.dup-baseline.json`. Si riallinea la baseline al ribasso dopo ogni consolidamento.
- `scripts/check-single-source.sh`: guard testuale che blocca nuove definizioni di
  un punto unico fuori dal suo modulo. I check si attivano per wave.
- `docs/tech-debt-dup.md`: metrica del debito e baseline.
- Innesto: `lefthook.yml` (pre-commit veloce) + `.github/workflows/verify.yml` (gate completo).

### Trigger imperativo

Se stai per scrivere la 2a query/funzione/componente che risponde alla stessa
domanda, FERMATI: cerca il punto unico nel catalogo (ADR 0026); se esiste, delega;
se e' un concern nuovo, crea PRIMA il punto unico col meccanismo corretto, poi
aggiungilo al catalogo. Mai copiare-e-adattare.

## M. Stato tecnico dai segnali strutturati, mai dal testo (regola assoluta)

Regola autoritativa e vincolante per qualunque decisione basata sull'esito di una
richiesta a un provider/modello/servizio esterno o interno: **le informazioni
tecniche su una richiesta (successo, fallimento, tipo di errore, ritentabilita',
credito, rate-limit, esito di un run) devono essere lette da SEGNALI STRUTTURATI e
codificati alla fonte, MAI dedotte dal parsing del testo umano del messaggio.**

Il testo in linguaggio naturale cambia per provider, versione dell'API e lingua:
classificarci sopra e' fragile per definizione ed e' una toppa (regola H).

### Cosa e' vietato

- **Classificare un errore con `contains("...")`/regex sul messaggio** (es.
  `msg.contains("insufficient_quota")`, `contains("not enabled")`, `contains("rate
  limit")`) per decidere retry, cooldown, fallback, billing, routing.
- **Dedurre l'esito di un run dell'agente** dal matching di frasi ("non riesco",
  "unable to", ...): l'esito va da un CAMPO strutturato (enum) dichiarato dal
  modello o da una verifica oggettiva.
- **Ri-derivare informazione strutturata da una stringa gia' appiattita** (es.
  formattare `"HTTP {status}: {body}"` e poi ri-parsarne lo status con una regex).
- **Dedurre l'esito di un tool/comando dal parsing dell'output** invece che dal
  segnale strutturato (`exit_code`/`is_error` del tool_result): es. cercare
  `"error"`/`"❌"` nell'stdout per decidere se e' fallito.
- **Trattare una ripetizione come "loop/stallo da abortire" senza guardare il
  segnale di esito**: un'azione ripetuta che FALLISCE per segnale strutturato
  (exit_code!=0 / is_error) e' una CAUSA RADICE da diagnosticare, non un loop a
  vuoto. Solo una ripetizione che RIESCE senza progresso e' uno stallo vero.

### Cosa e' richiesto

1. **Segnale primario: lo status/codice macchina.** Per un errore HTTP: lo
   **status code numerico** (certo, standard) e il **codice d'errore strutturato**
   dal JSON del provider (`error.type`/`error.code`/`error.status` — identificatore
   macchina stabile). Vedi ADR 0033.
2. **Propagare l'errore in forma tipizzata**, non come stringa: il punto di
   costruzione (l'adapter che conosce il formato) codifica status+codice in un tipo
   (`ProviderHttpError` nel gateway); il punto di decisione fa `downcast`, non
   `to_string()`. Trasporto errori tramite tipi/predicati (`reqwest::Error::status()`,
   `is_timeout()`), non messaggi.
3. **Il testo umano solo per display/log**, mai per decidere (come RFC 9457
   distingue `type`/`code` da `detail`).
4. **Quirk di un provider senza codice strutturato** (raro) va ISOLATO nel suo
   adapter, che traduce il proprio errore in un codice strutturato; il punto di
   decisione generico resta deterministico.
5. **Esito conversazione/run**: preferire output strutturato del modello (tool a
   schema strict / structured outputs, enum `outcome`/`blocker`/`refusal`) e
   verifica oggettiva (`final_gate`), mai il pattern-matching della prosa. Vedi
   ADR 0034.
6. **Esito tool/comando: `exit_code` + `is_error` STRUTTURATI del tool_result**
   (0 = successo), mai il parsing dell'output. L'anti-loop decide su questo
   segnale: un'azione ripetuta che fallisce davvero (es. `curl` health-check con
   exit 7 = servizio non in ascolto) viene instradata a diagnosi della causa
   radice, mai chiusa come "il modello non riesce". Punto unico:
   `tool_result_outcome_after` / `RepeatedActionHit.failed` +
   `repeated_action_failed` in `crates/nexus-agent-graph`.

### Punto unico e riferimenti

- Classificazione errori provider: `classify_provider_error(&anyhow::Error)` +
  `ProviderHttpError` in `crates/nexus-gateway/src/providers/openai_compat.rs`
  (punto unico, regola L). Tutti i provider (openai/deepseek/mistral/vllm via
  `OpenAiCompatClient`, google incl. Vertex, anthropic) emettono `ProviderHttpError`
  su OGNI risposta HTTP non-2xx, incluse liste modelli, poll e token endpoint.
- ADR 0033 (classificazione deterministica), ADR 0034 (esito strutturato).

### Conseguenza pratica

Prima di scrivere un `if msg.contains(...)` per decidere qualcosa su una richiesta,
FERMATI: quel segnale esiste gia' in forma strutturata (status, codice, campo enum)
o va reso disponibile alla fonte. Un PR che classifica lo stato tecnico dal testo e'
rifiutato come toppa (regola H).

## Esecuzione locale canonica

- Ambiente di sviluppo locale: **Windows nativo**, repo Git in `D:\IDEAI`. Shell:
  **PowerShell** (più la Bash tool POSIX solo per comandi Unix puntuali). Niente WSL,
  niente percorsi `/home/...`, niente `wsl.exe`.
- Build/gate Rust via PowerShell con toolchain MSVC (`cargo check` / `clippy` / `test`).
- Nessun `preview_start` per il dev locale.
- Comandi chiave:
  - `pnpm verify` — gate completo (turbo typecheck/lint/test + cargo check/clippy/test)
  - `pnpm smoke` — smoke test dei servizi (porte configurabili via env)
  - `pnpm xtask lint-commits <base> <head>` — controllo redazionale commit
  - `deploy/deploy-local.ps1` — build + restart dei servizi Windows (WinSW); per i
    parametri vedere lo script stesso

## Riferimenti incrociati

- `docs/contributing.md` — workflow study -> confirm -> automatic
- `docs/tech-debt-rust.md` — backlog `unwrap`/clippy
- `docs/tech-debt-ts.md` — backlog `any`/strict
- `docs/tech-debt-dup.md` — metrica duplicazione e baseline ratchet (regola L)
- `docs/.nexus-vault/adr/0026-punto-unico-de-duplicazione.md` — catalogo punti unici + meccanismo
- `config/policies/` — profili cloud/onprem/hybrid (contratto gateway LLM)
