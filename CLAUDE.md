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

### Fuori chat (REST diretti a mcp-core, worker schedulati, batch, reflection)
- NESSUNA modalita' UI viene ereditata. Il prompt e' l'unico contratto.
- Istruzioni di autonomia, anti-loop, output format, examples, reflection devono essere **esplicite** nel prompt stesso.
- I prompt agente in `nexus_prompt_templates` (chiavi `agent.*`, `system.*`) seguono lo schema XML standard (`<role>` / `<contesto>` / `<autonomia>` / `<protocollo>` / `<tool_usage>` / `<anti_loop>` / `<output_format>` / `<examples>` / `<reflection>`) — vedi migrazione 0086.
- Sono call site fuori chat (audit a riferimento, non esaustivo): `crates/mcp-core/src/prompt_templates.rs:404,501,549`, `crates/mcp-core/src/orchestrator.rs:1210`, qualunque worker in `crates/nexus-orchestrator/src/workers/`.

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
  - `load_provider_catalog` in `crates/mcp-core` (catalog loader Rust) propaga errore se DB down o tabella vuota (ex `brain/providers/catalog_loader.py`, porting zero-Python completato)

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

Smart routing vision: se il messaggio utente contiene allegati `image/*` il router (mcp-core) preferisce automaticamente un modello con `capabilities.vision=true` (override sulla routing matrix). Configurabile via `nexus_purpose_model` chiave `vision_describe`. Modello di default: `google/gemini-2.0-flash-exp` (mig 0194).


## I. Pipeline allegati robusta (ADR 0012)

Quando l'utente carica un allegato (PDF, DOCX, Figma, ZIP, immagine, ecc.) l'agente deve seguire la pipeline definita in ADR 0010 + 0011 + **0012** senza scorciatoie.

- **Pre-extraction automatica** (FIX 3): per PDF/DOCX/ZIP-con-canvas.fig il blocco <allegati> del primo messaggio gia' contiene un sub-blocco ### Pre-extracted content. Il modello NON deve chiamare nexus_inspect_attachment / nexus_extract_* per ottenere informazioni che sono gia' visibili.
- **`nexus_inspect_attachment` quando serve**: il tool ora ritorna `next_action_recommended` con `{tool, input, rationale, expected_tokens_output}`. **Dopo** averlo chiamato, l'agente deve chiamare ESATTAMENTE quel tool con quegli input. Vietato chiamare `nexus_read_attachment` / `nexus_read_archive_entry` con offset crescenti su file binari.
- **Cache deduplica** (FIX 2): chiamate identiche a read_attachment / read_archive_entry vengono servite dalla cache. Se il payload include `from_cache: true` + `hint`, l'agente deve cambiare strategia (passare a un tool di estrazione strutturata o a una entry diversa).
- **Budget letture per sessione** (FIX 4): max 500 KB cumulativi (default DB) per nexus_read_attachment + nexus_read_archive_entry. Oltre la soglia mcp-core ritorna un tool_result sintetico che invita a usare gli estrattori strutturati.
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
| Stato + comportamento | classe/struct incapsulata + generics | `TtlCache<K,V>` (crate `nexus-cache`) |
| Varianti polimorfiche su contratto comune | `trait` (Rust) + composizione | provider in `crates/nexus-gateway/src/providers/` |
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
| Richiesta della CHAT al gateway (modello, pin del provider forzato, coppia prenotata a ledger) | `mcp-core/src/orchestrator/model_routing.rs` (`build_chat_gateway_call`); il modello lo risolve delegando a `RoutingConfig::resolve_model`. Guard `richiesta gateway chat`. Vedi ADR 0023 |
| Forza del vincolo sul provider scelto in chat (preferenza vs pin duro) | `mcp-core/src/orchestrator/provider_choice.rs` (`ProviderOverrideMode` = `preferred\|pinned`, `ProviderChoice::resolve` = unico punto in cui un pin nasce, e nasce solo dalla richiesta in corso: il pin non si eredita da sessione o resend) + `apps/web-ide/components/chat/provider-choice-logic.ts` lato UI. Guard `vocabolario forza-vincolo provider`, `nascita del pin duro`. Vedi ADR 0023 |
| Scrittura del tier di un modello (precedenza `manual` > `measured` > `synced` > fonte ignota) | `mcp-core/src/orchestrator/model_service.rs` (`apply_tier`, `TierSource`, `puo_sovrascrivere`); il sync dell'indice e la batteria delegano. Guard `tier-write` |
| Listino modelli (prezzo di una chiamata + currency di piattaforma) | crate `nexus-pricing` (`resolve_active_price` -> `PriceLookup{Priced\|Unknown\|NotInCatalog}`, `platform_currency`, `calculate_cost`, `assert_configured`). Guard `pricing-single-source` |
| Contabilita' di `ai_usage_ledger` (ogni riga scritta + il consumo che le quote leggono) | crate `nexus-ledger` (`reserve`, `record_tokens`, `record_media`, `insert_marker`, `finalize`, `release`, `settle`; `active_quotas`, `usage_for_quotas`, `usage_for_scope`). `settle` e' il punto unico di "chi addebita questa chiamata" e legge la `Declaration` dal wire, non l'esito (regola M). Guard `ledger-single-source` |
| Identita' contabile utilizzabile ("queste due stringhe di metadata valgono un addebito?") | `nexus-ledger` (`identity_from_metadata`). Se la pongono i due lati del wire: il gateway sulla richiesta che RICEVE (per decidere se scrivere), mcp-core su quella che MANDA (per sapere se un "non ho scritto" e' legittimo). Due copie renderebbero quel confronto una recita |
| Verdetto su cio' che il gateway ha dichiarato della contabilita' | `nexus-ledger` (`LedgerOutcome` sul wire = `written\|no_identity\|write_failed`; `Declaration` = cio' che si e' potuto leggere, incluso `Illeggibile`; `Declaration::audit` = il verdetto dato cio' che si e' mandato). Il campo ASSENTE significa "gateway che non parla questa versione del contratto", e su una chiamata con identita' valida e' un sospetto di doppio addebito, non un ripiego innocuo |
| Identita' utente/progetto | `crates/nexus-types/src/lib.rs` (`parse_user_id`, ...) |
| Lettura settings | `nexus-auth::settings` (`get_setting`) |
| Scrittura settings (aggiorna, non crea: chiave assente -> 404) | `nexus-auth` (`update_setting_value` -> `SettingWriteError{UnknownKey\|Db}` + `status_code()`). Guard `update_setting_value` e `settings INSERT di ripiego` |
| Cache TTL | crate `nexus-cache` (`TtlCache<K,V>`) |
| Identita' temporale del binario in esecuzione (`build_time` di `/health`) | `nexus-types/src/build_info.rs` (`running_binary`, mtime del proprio eseguibile letto all'avvio); `HealthSummary::new` popola i campi, il call site non li sceglie. Guard `running_binary` e `build-stamp`. NON incidere timestamp da uno script di build: cargo non lo riesegue a ogni link |
| Lettura di un manifest di servizio Windows (eseguibile, working dir, argomenti, env) | `deploy/lib/nexus-manifest.ps1` (`Read-NexusServiceManifest`); dev-start, dev-service e nexus-publish delegano. Legge i tag OPZIONALI (`<arguments>`, `<env>`) con XPath, non con l'adapter a proprieta': `$x.service.arguments` si rompe solo sotto StrictMode, cioe' solo per certi percorsi di invocazione. Guard `lettura-manifest-servizio` e `strictmode-non-si-propaga`. Il lettore vero e' PowerShell: `parse_winsw` (Rust) e' una controfigura per `--check`, e i test che passano da lei non misurano il consumatore (regola O) |
| Pool DB metadati per-progetto (registry, elenco progetti, directory routing, cache pool) | crate `nexus-project-pools` (separazione sempre attiva, flag rimosso mig 0527); `mcp-core::project_db_routes` delega e vi aggiunge solo provisioning+migrazione |
| Fetch HTTP frontend | `apps/web-ide/lib/api/_shared.ts` (`fetchJson`) |
| Completion testuale via gateway per i crate FUORI da mcp-core (admin-service, worker di nexus-orchestrator) | `nexus-types/src/gateway_client.rs` (`gateway_text_complete`). Dentro mcp-core resta `nexus_gateway::NexusGatewayClient`. ADR 0041 |
| Aggregazione problemi ripetitivi (pannello Problemi) | `mcp-core/src/project_workspace/problem_aggregation.rs` (`problem_group_key`, `aggregate_problems`); `get_project_problems` delega |
| Discendenza di un run (quali run compongono il suo lavoro: token, costo, provider) | `mcp-core/src/run_lineage.rs` (`parent_run_by_child`, da `nexus_subagent_runs.dispatcher_run_id`); `trace_store::get_session_traces` annota `parentRunId` sulle tracce dei sub-run e il frontend (`tracesForRun` -> `providerCostBreakdown`) vi delega. NON dedurre la parentela dai meta-step di narrazione: sono un canale di presentazione che il review panel non emette |
| Dimensionamento dei panel multi-agente (quante figure/revisori/provider/avvocati) | `nexus-agent-graph/src/decisions/orchestration_sizing.rs` (`resolve_orchestration_plan`; i cap storici restano backstop). ADR 0040 |
| Tesi contrapposte (assegnazione posizioni + selezione opzione) | `nexus-agent-graph/src/decisions/debate_panel.rs` (`plan_debate`, `compose_debate_synthesis`). ADR 0040 |
| Vocabolario gravita' evidenza (alta/media/bassa) + test "evidenza grave" | `nexus-agent-graph/src/decisions/severity.rs`; advisory/review/debate delegano |
| Vocabolario performance-tier (light<medium<high<heavy<frontier) | `nexus-types/src/tiers.rs`; `decisions/tiers.rs` e' un re-export |
| Tool mutativo ("questo tool scrive?") | `nexus-agent-graph/src/decisions/hitl.rs` (`is_mutator_tool_name`, `pending_contains_mutator`) su `agent.tools.result_cache_mutators`; gate HITL e barriera advisory delegano |
| Whitelist runtime dei kind (CSV `orchestrator.subagent_kinds_whitelist`) | `admin-service/src/figures.rs` (`mutate_kinds_whitelist`) |
| Schema di test del DB-progetto (i `#[sqlx::test]` girano sulla migrazione reale, mai su un `CREATE TABLE` ricopiato) | crate `nexus-migrations-embedded` (`PROJECT_MIGRATOR` = set `db/migrations/project`) + seeder in `mcp-core::test_support` (`seed_chat_session`, `seed_agent_run`, `seed_plan`, `seed_todo`). Guard `schema-di-test` |
| «Questo path scritto sta dentro lo scope dichiarato dal piano?» + verdetto persistito | `nexus-agent-graph/src/decisions/orchestration_reason.rs` (`path_in_scope`, `classify_write` -> `ScopeVerdict{no_scope_declared\|in_scope\|out_of_scope}`), accanto a `normalize_scope_path` che RIUSA: due normalizzazioni diverse darebbero due idee diverse di "dentro". Direzionale, a differenza di `scopes_overlap` che e' simmetrico perche' li' la domanda e' "due aree si pestano?". E' una MISURA, non un enforcement: nessun tool rifiuta una scrittura. Registrata da `record_mutation` in `file_mutations` (colonne mig 0646, DB META) e aggregata dalle viste `file_mutations_scope_audit` / `_out_of_scope_paths`. Guard `catena-write-scope` |
| Fornitore VIETATO a un sub-run («giudice != worker»: un revisore non gira sul fornitore che ha scritto il codice, nemmeno ripiegando) | `mcp-core/src/agent_tools/subagent_native.rs` (`veto_del_giudice`) esprime la regola; `orchestrator/provider_choice.rs` (`ProviderVeto`) la porta, duale NEGATIVO di `ProviderPin` e tipo distinto perche' scambiarli darebbe un run che puo' usare solo il fornitore da evitare. Selezione e ripiego interrogano lo stesso punto: prima il vincolo viveva solo nella selezione e il failover, che conosce i soli provider gia' tentati nel turno, riportava il giudice sul worker. Guard `veto-alle-porte` |
| «Questa porta e' del bucket di QUESTO progetto?» + estremi del bucket | `nexus-tool-kit/src/ports.rs`: `project_bucket_range` (estremi INCLUSIVI: prima la stessa somma girava in sei punti con DUE convenzioni, e una porta di confine cadeva dentro o fuori a seconda del file), `port_in_project_bucket`, `classify_project_port` -> `PortRegistrability{Registrable\|Reserved\|OutOfProjectRange\|OutOfProjectBucket}`. `is_project_registrable_port` prende il `project_id`: senza, la sola domanda ponibile era «e' di QUALCHE progetto?», e una porta del bucket altrui entrava in `nexus_port_allocations` come allocazione del progetto sbagliato. Li' faceva da prova di legittimita' a se stessa - il resource_linter tace sulle porte «allocate» e il port_enforcer non le uccide - quindi piu' il sistema sbagliava, meno lo segnalava. Il registro da solo non basta: `security::resource_linter::legitimate_ports_for_project` = registrata E nel bucket, e port_scanner vi delega. Guard `estremi-bucket-porte` |

### Enforcement automatico (la regola e' duratura, non una-tantum)

- `jscpd.json` + `scripts/dup-report.sh`: misura cross-linguaggio (TS/JS/Rust/Python)
  con gate "ratchet" — il numero di cloni puo' solo SCENDERE rispetto a
  `.dup-baseline.json`. Si riallinea la baseline al ribasso dopo ogni consolidamento.
- `scripts/check-single-source.sh`: guard testuale che blocca nuove definizioni di
  un punto unico fuori dal suo modulo. I check si attivano per wave. Include il
  check `migrazione-stub`, che rifiuta nuove migrazioni con corpo solo `SELECT 1;`
  (informazione di schema distrutta in modo irrecuperabile).
- `scripts/markers-ratchet.sh` + `scripts/markers-baseline.json`: gate "ratchet"
  su due famiglie di marker testuali — debito esplicito
  (`TODO|FIXME|HACK|XXX|WORKAROUND|DEBITO`) e frasi di inerzia
  (`INERTE|mai raggiunt|non ancora cablat/portat/instradat`) — che possono solo
  SCENDERE. Impedisce di reintrodurre commenti fossili (regola O).
- `docs/tech-debt-dup.md`, `docs/tech-debt-markers.md`: metrica del debito e baseline.
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

## N. Identificatori canonici (inglese, univoci)

Regola autoritativa per nomi di comandi, enum wire, valori API/DB e chiavi
configurazione: **un solo identificatore in inglese per azione**, valido in tutto
il codebase.

### Cosa e' vietato

- Parser "lenienti" con sinonimi multipli (`automatic`/`automatico`/`auto`,
  `confirm`/`conferma`, `study`/`studio`, alias supervisor `a`/`b`/`c`, ecc.).
- Duplicare la stessa logica di parse in piu' moduli: un solo punto unico per enum.
- Usare etichette UI tradotte come `value` inviato al backend.

### Cosa e' richiesto

1. Identificatori canonici documentati e usati ovunque (es. automation:
   `study|confirm|automatic`; supervisor: `none|anomaly|interleaved|continuous`).
2. Punto unico parse: `orchestrator::AutomationMode::try_parse` (mcp-core).
3. Etichette UI tradotte solo per display; il `value` del controllo e' sempre
   l'identificatore canonico.
4. Valori legacy nel DB normalizzati via migrazione SQL versionata (es. mig `0558`),
   non accettati permanentemente nel codice.
5. Guard in `scripts/check-single-source.sh` (check `canonical-identifiers`).

### Punto unico noto

| Concern | Modulo/funzione autoritativa |
|---|---|
| Parse automation_mode | `crates/mcp-core/src/orchestrator/mod.rs` (`AutomationMode::try_parse`) |
| Parse supervisor_mode | `SupervisorMode` FromStr in `agent_types.rs` / `nexus-agent-graph` state |

## O. Lo strumento di misura raggiunge il suo oggetto come la produzione

Regola autoritativa per test, script diagnostici e gate: **la misura deve arrivare
al suo oggetto per la STESSA strada della produzione**. Uno strumento che
ri-costruisce l'input a mano, ri-scrive la query o risolve il percorso a modo suo
non misura il sistema: misura una sua imitazione, e quando le due divergono non
fallisce — mente con la faccia seria.

E' la regola L applicata agli strumenti. Oggi test e script sono trattati come se
non fossero codice: sono invece la parte di codice che decide **se ti fidi**.

### Il pattern, e perche' non si vede

Sempre lo stesso: lo strumento e il sistema usano fonti diverse per la stessa
domanda. Entrambi funzionano, su cose diverse. Casi REALI di questo repo:

| Strumento | Come raggiungeva l'oggetto | Cosa non poteva vedere |
|---|---|---|
| `xtask quality-scan --root` | risolveva il path dalla CWD | misurava un albero, dichiarava l'altro (fix `2ae08818`) |
| 3 test di `error_class_from_gateway` | chiamavano la funzione a mano | che in produzione non era MAI raggiunta (codice morto con test verdi) |
| `classify_deterministica_da_status_e_codice` | passava il codice a mano | che l'estrattore quel codice non lo produceva mai (groq 413) |
| `read_turn_signals` + i suoi test | inventavano `turn['result']` | che il produttore scrive `content`: 0 per costruzione |
| helper di test `run()` | fissava `inconclusive: 0` | il ramo del silenzio, mai esercitato |
| script diagnostico | ricopiava `SQL_CLAIM` a mano | leggeva la suite dalla tabella sbagliata: "0 candidati" contro 29 |
| `rg -rn` | `-r` e' `--replace`, non "recursive" | output falsato per un'intera sessione |
| fixture `CREATE TABLE` nei `mod tests` | ricopiavano lo schema a mano | 41 copie divergenti dalla migrazione: righe che il DB di produzione RIFIUTA (run senza sessione, todo senza piano, step senza `tool_input`) create dai test per anni. Fix: `nexus-migrations-embedded::PROJECT_MIGRATOR` + guard `schema-di-test` |

### Cosa e' richiesto

1. **Un test attraversa il produttore.** Se in produzione un valore nasce da una
   funzione nota (`agent_turn_value_from_gw`, `ProviderHttpError::from_response`,
   `GatewayHttpError::from_response`, `catalog_select!`), il test parte da LI'.
   Costruire a mano l'input equivale a fissare l'assunto che si vuole verificare:
   codice e test condividono l'errore e restano verdi per sempre.
2. **Un test arriva alla CONSEGUENZA, non alla stringa.** Asserire che una funzione
   ritorni `"model_not_found"` non prova niente se nessun consumatore conosce quella
   parola (accaduto: cadeva nel catch-all `Transient`). Si asserisce il verdetto.
3. **La diagnostica chiama il codice, non lo imita.** Uno script che ricopia una
   query o una regola di produzione e' un punto unico violato: la copia divergera'.
   Chi diagnostica pone la domanda al sistema; se non c'e' un modo per porla, si
   aggiunge (un `explain`), non si riscrive la query.
4. **Un numero senza la sua premessa e' un'opinione.** Ogni strumento dichiara DA
   DOVE guarda: quale albero, quale fonte, quale seed. `0` non e' un risultato;
   `0 (suite letta da ai_price_catalog)` lo e' — e si vede subito che e' sbagliato.
5. **Ogni fix ha il suo test di mutazione.** Si rompe apposta il codice appena
   corretto e si verifica che il test ROSSEGGI, col valore del difetto reale. Un
   test che non fallisce quando reintroduci il bug non copre il bug: copre se
   stesso.

### Conseguenza pratica

Prima di fidarti di un verde o di un numero, chiediti: *questo strumento tocca il
sistema dove lo tocca la produzione?* Se il test costruisce l'input, se lo script
riscrive la query, se il gate risolve il path da solo — non stai misurando il
sistema. Un PR che aggiunge un test che fabbrica un input gia' prodotto altrove e'
rifiutato come una toppa (regola H).

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
- `docs/tech-debt-markers.md` — marker di debito e frasi di inerzia, gate ratchet (regola O)
- `docs/.nexus-vault/adr/0026-punto-unico-de-duplicazione.md` — catalogo punti unici + meccanismo
- `config/policies/` — profili cloud/onprem/hybrid (contratto gateway LLM)
