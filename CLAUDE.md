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
| Convenzione dei token sul confine col provider (prompt LORDO, output FATTURABILE) | `nexus-types/src/token_usage.rs` (`prompt_tokens_gross`, `completion_tokens_billable`). Il VERSO lo dichiara l'adapter con un tipo — `PromptCacheReporting` e `ReasoningTokens{IncludedInOutput\|Separate}` — perche' la domanda non e' «quanti», e' «erano gia' contati?», e la risposta sbagliata non produce un numero strano: produce un addebito doppio o mancante. `ReasoningTokens` non ammette un numero nella variante che afferma l'inclusione. Google e' l'unico che riporta il ragionamento a parte: `candidatesTokenCount` porta il solo testo VISIBILE, `thoughtsTokenCount` viaggia fuori e finche' non era dichiarato in `GoogleUsageMetadata` serde lo scartava in silenzio — misurato il 30/07/2026 su `gemini-2.5-flash`, 3 token visibili contro 157 di pensiero, cioe' il 98% dell'output fatturabile perso su OGNI turno Google senza tool. La somma sta LI' e non alla fonte: `output_tokens` ha un secondo consumatore che misura il testo PRODOTTO (`is_degenerate_completion`), e sommare a monte sostituirebbe una sottostima del costo con un turno vuoto che nessuno vede piu'. La chiamano i due lati che pagano: `nexus-gateway::server::billing::token_usage_from` (riga di ledger) e `mcp-core::agent_graph_adapter::llm_gateway::turn_cost_usd` (freno di spesa del run) |
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
| Memorie di progetto nel prompt (QUALI entrano: gate, soglia di pertinenza, taglio; e COME si rendono) | `mcp-core/src/prompt_memories.rs` (`ProjectMemories::{load, section}`; `MemoryRecall` isola il solo confine esterno, embedder + Qdrant). Delegano i DUE percorsi della chat: `Orchestrator::run` (turno singolo, Studio) e `compose_agent_system_text` in `chat_messages/agent_run.rs` (run agentico, Conferma/Automatico). Il secondo prima non esisteva: il consumo viveva dentro `run`, raggiungibile solo da `run_turn`, e l'handler in modalita' agente dispatcha a `spawn_agent_run` e ritorna prima — il pannello "Memoria del progetto" non aveva alcun effetto sui run agentici. Il caricamento sta DENTRO il compositore, non nel chiamante: cosi' quel prompt non e' componibile senza passare dal richiamo. Guard `blocco-memorie-prompt` e `memorie-nel-prompt` |
| IDENTITA' di un servizio di progetto (la label da cui nascono `{slug}-{label}.service`, la riga `nexus_port_allocations` legata a quell'unit e il match porta<->servizio del pannello) | `mcp-core/src/agent_tools/service.rs` (`resolve_service_label`): mai una label generica, nemmeno come ripiego del sistema. Lo scopo lo dice `derive_kind_hint` da comando + working dir SOTTO la radice (`scope_dir`); senza scopo l'identita' viene dal percorso, sempre SOTTO la radice. Il nome della cartella di PROGETTO non e' mai un candidato, e i due criteri lo escludono per la stessa ragione: l'unit e' gia' `{slug}-{label}.service`, quindi lo slug e' la parte che il progetto mette e la label e' cio' che distingue un servizio dall'altro dentro quel progetto — `{slug}-{slug}.service` dice due volte il progetto e non dice il ruolo. Il ripiego che lo produceva fu misurato il 30/07/2026 su bacheca-attivita: l'unica identita' che avesse MAI prodotto, sull'intero parco progetti, era un `npx eslint` lanciato dalla radice (un lint, non un servizio), e il pannello la mostrava come servizio fantasma. Quando nessun segnale dice il ruolo, `ServiceIdentity` dichiara l'assenza invece di inventare, e la conseguenza la traduce `classifica` in un punto solo: un comando che avvia un server riceve un ancoraggio al progetto per COSTRUZIONE (`service-{uuid[..8]}`, stabile fra i riavvii, quindi capace di ricevere e conservare una porta); tutto il resto e' declassato a `kind='task'` — gira identico, ma senza unit, senza allocazione e fuori dal pannello Servizi, come il one-shot. Il vocabolario "generica"/"simile" resta `agent_processes` (`is_generic_service_label`, `similar_service_labels`): il ripiego storico `unwrap_or("Service")` produceva esattamente cio' che quel vocabolario dichiara privo di significato, e un servizio con quel nome non incontra mai la propria allocazione |
| «Questo rimando in correzione ha prodotto progresso?» (il ciclo review contava i TENTATIVI, non i fatti) | `nexus-agent-graph/src/decisions/correction_progress.rs` (`WriteFact::cambia_il_contenuto` = il criterio, `classify_correction_progress` -> `CorrectionProgress{Effettivo\|SoloRiscritture\|NessunaScrittura}`). Il criterio e' `before_sha256 != after_sha256`, MAI il conteggio delle chiamate ai tool di scrittura: riscrivere un file identico e' il modo in cui un agente simula attivita' senza produrne. L'I/O e' la porta `MutationProgressPort` (impl `mcp-core/src/agent_graph_adapter/mutation_progress.rs`), che porta i fatti e NON li filtra: un filtro in SQL renderebbe una riscrittura identica indistinguibile da "non ha scritto". Il final_gate NON usa il taglio (i suoi criteri misurano l'ambiente, non i file): motivazione nel modulo. Guard `correction-progress` |
| «Questa riparazione automatica ha rimesso in piedi il servizio?» (contratto di successo di una remediation di servizio) | `mcp-core/src/project_workspace/service_recovery.rs` (`judge_recovery` = il criterio puro, `ServiceHealth::{is_conforming,meets_contract}`, `stable_enough`, `probe_port` -> `PortAnswer{Http\|Tcp\|Silence}`, `restart_and_verify` = il ciclo, `apply_recovery_verdict` = l'unica scrittura dell'esito). Il ciclo chiudeva su DUE segnali deboli, entrambi «e' nato un processo»: `restart_project_unit` -> "riavvio effettuato", e `service_observer::resolve_open_crashes` invocata al cambio del marcatore d'avvio, che marcava `resolved` qualunque diagnosi attiva. Misurato il 28/07/2026 (gestione-spese): «servizio frontend avviato» alle 21:29, diagnosi chiusa, e due ore dopo in ascolto c'era solo il backend. Il contratto e' invece osservabile: stato `Running` E almeno una porta ALLOCATA a quella unit (`nexus_port_allocations.service_unit`, stesso criterio con cui l'observer APRE la diagnosi) che risponde, per una durata ininterrotta, e di nuovo dopo un ulteriore riavvio. Nessun controllo sulla VARIANTE osservata: inseguire le varianti a codice lascerebbe muto il ciclo alla prossima. Non riusa `endpoint_probes` (fonte, criterio di successo ed esecutore sono altri: motivazione in fondo al modulo); riusa `port_recovery::tcp_probe` per il ripiego non-HTTP. `resolve_open_crashes` non tocca piu' i `diagnosing`, e il pannello Problemi mostra tutto cio' che non e' `resolved` — altrimenti un crash in `failed_remediation` sparirebbe come spariva da `resolved`. Lo stesso contratto fa da gate di readiness al runner Playwright (`await_port_ready`, delegato da `agent_tools/testing.rs`): una suite lanciata a t+0 da un riavvio trova una porta che risponde ma un servizio freddo (misurato il 31/07/2026 su bacheca-attivita: 31 rossi su 53 giri, due regressioni fabbricate su codice sano), quindi la suite parte solo quando la porta della unit risponde stabilmente entro `agent.playwright.readiness_timeout_seconds` (mig 0662), altrimenti setup_failed "servizio non pronto" |
| «I requisiti emessi dal Consiglio sono stati applicati?» (il ciclo si chiudeva senza che nessuno guardasse) | `nexus-agent-graph/src/decisions/requirement_conformance.rs` (`derive_criterion` = la DOMANDA, `judge` = il verdetto sul CONTENUTO del file, `nota` = l'unico punto in cui la misura diventa testo). Il segnale era prodotto bene ed entrava nel prompt del coordinatore (`pre_run_advisory_synthesis`); il confronto fra requisiti emessi e codice prodotto non esisteva. Il criterio e' il file, mai la dichiarazione dell'agente (regola M), e DETERMINISTICO per vincolo: nessuna chiamata al modello — dove servirebbe un giudizio semantico, il caso e' `unverifiable`. Entrano SOLO i `requirements`: una raccomandazione non applicata non e' uno scostamento. Ogni ambiguita' degrada a `unverifiable` col motivo dichiarato, MAI a `satisfied`. INFORMATIVO: la nota si aggiunge al resoconto e non declassa lo status (il Consiglio e' advisory per decisione del 13/07/2026). Guard `requisiti-consiglio` |
| Quali chiamate HTTP prova il final gate prima di dichiarare «verificato» | `nexus-agent-graph/src/decisions/endpoint_probes.rs` (`normalize_endpoints`, `endpoint_criteria_from_declaration`, `DEFAULT_SUCCESS_STATUSES`). Due fonti che confluiscono li': gli endpoint CONFIGURATI del progetto (`run_configurations` role='endpoint' + `http_spec`, letti da `mcp-core::native_engine::load_configured_endpoint_criteria`) e quelli DICHIARATI dall'agente (`task_complete.endpoints`, ADR 0034). Prima il campo era SINGOLARE e nel motore nativo restava al `Default`: il criterio HTTP non veniva costruito mai, e il gate dichiarava «superato» un'app la cui POST rispondeva 500. La dichiarazione e' l'unica fonte che conosce i metodi di SCRITTURA; la history serve a un'altra domanda (`signals::http_probes_in_history`: il silenzio e' sospetto?) e mai a costruire criteri, perche' contiene solo le GET che l'agente aveva gia' provato. Vedi ADR 0026 |
| Fornitore VIETATO a un sub-run («giudice != worker»: un revisore non gira sul fornitore che ha scritto il codice, nemmeno ripiegando) | `mcp-core/src/agent_tools/subagent_native.rs` (`veto_del_giudice`) esprime la regola; `orchestrator/provider_choice.rs` (`ProviderVeto`) la porta, duale NEGATIVO di `ProviderPin` e tipo distinto perche' scambiarli darebbe un run che puo' usare solo il fornitore da evitare. Selezione e ripiego interrogano lo stesso punto: prima il vincolo viveva solo nella selezione e il failover, che conosce i soli provider gia' tentati nel turno, riportava il giudice sul worker. Guard `veto-alle-porte` |
| «Questa porta e' del bucket di QUESTO progetto?» + estremi del bucket | `nexus-tool-kit/src/ports.rs`: `project_bucket_range` (estremi INCLUSIVI: prima la stessa somma girava in sei punti con DUE convenzioni, e una porta di confine cadeva dentro o fuori a seconda del file), `port_in_project_bucket`, `classify_project_port` -> `PortRegistrability{Registrable\|Reserved\|OutOfProjectRange\|OutOfProjectBucket}`. `is_project_registrable_port` prende il `project_id`: senza, la sola domanda ponibile era «e' di QUALCHE progetto?», e una porta del bucket altrui entrava in `nexus_port_allocations` come allocazione del progetto sbagliato. Li' faceva da prova di legittimita' a se stessa - il resource_linter tace sulle porte «allocate» e il port_enforcer non le uccide - quindi piu' il sistema sbagliava, meno lo segnalava. Il registro da solo non basta: `security::resource_linter::legitimate_ports_for_project` = registrata E autorizzata, e port_scanner vi delega. Guard `estremi-bucket-porte` |
| «Questo progetto puo' USARE questa porta?» (autorizzazione, distinta dalla registrabilita') | `nexus-tool-kit/src/ports.rs` (`port_authorized_for_project` = nel bucket OPPURE riga `manual`; parte pura `allocation_authorizes_port`, vocabolario `ALLOCATION_MODE_MANUAL`). Prima la stessa domanda aveva QUATTRO risposte: il sandbox accettava una riga di qualunque `allocation_mode` (quindi un'allocazione nata da un automatismo autorizzava se stessa), il port_enforcer ammetteva le sole `manual` - la risposta giusta, ma data in un posto solo -, il GC (`port_registry::cleanup_orphaned_ports`) proteggeva tutto cio' che stava nel range globale (e quindi anche il bucket altrui, finche' un processo ci ascoltava), il linter guardava il solo bucket e segnalava le `manual` legittime. Solo `manual` e' una decisione umana: gli altri modi (`auto\|dynamic\|existing\|adopted`) nascono da automatismi, e uno sbaglio non puo' rendersi lecito da se'. Guard `estremi-bucket-porte` |
| «Di CHI e' questo processo in ascolto?» (appartenenza di un processo a un servizio, prima di legargli una porta) | `mcp-core/src/project_workspace/service_ownership.rs` (`classify_ownership` -> `ServiceOwnership{Own\|Other\|Unknown}`, `owned_listener` = «fra questi, quale e' il mio?», `resolve_stale_adoption` -> `StaleAdoption{AdoptOrphan\|ReuseStale}`). Vi passano i TRE rami di `find_or_allocate` che possono legare a un servizio una porta gia' in uso: occupante della porta allocata, adozione di un processo del bucket, riuso quando non esiste riga per la label. Ognuno aveva la SUA domanda larga — `is_tracked_pid` («e' del progetto?»), il bucket («e' nel mio intervallo?»), `labels_match` («e' della stessa CLASSE?», dove la classe frontend include web/ui/client/vue/next/react e per un processo non registrato la label la inventa `derive_orphan_label` dal NOME DEL PROGRAMMA) — e tutte e tre rispondevano si' per il servizio sbagliato. Allocando per `frontend`, l'adozione prendeva il BACKEND (33649), la porta occupata veniva iniettata come `PORT`, Vite senza strictPort ripiegava su 33650/33651 FUORI bucket, e la porta legittima restava libera. Stessa forma di difetto a monte, alla FONTE che alimenta quei rami, tolta li': `resource_resolver::orphan_placeholder_label` (ex `derive_orphan_label`) pretendeva di dare a un processo NON registrato uno scopo indovinato dal nome del programma, e quella label sarebbe finita nella stessa lista di quelle lette dal DB, indistinguibile — con tre consumatori che vi decidono sopra, fino a `free_listening_scope_port` che non eredita una porta ma UCCIDE l'albero del processo «riconosciuto». MISURATO prima di rimuoverla: era INERTE, non rara. Il campo `program` di `listening_ports` porta il nome dell'ESEGUIBILE, mai la command line (Windows `szExeFile`, Linux `users:(("node",...))`), e ogni dev-server JS gira come `node`: l'euristica non ha mai potuto produrre "frontend", il che spiega anche gli zero `port_reuse` misurati sopra — non un riuso raro, un riuso impossibile. Il fix toglie una trappola ARMATA, non una che scattava: sarebbe scattata al primo che facesse passare la command line in quel campo credendo di migliorare la diagnostica. Lezione di metodo: una catena puo' essere reale nel CODICE e irraggiungibile nei DATI — si verifica cosa contiene davvero il campo su cui l'euristica decide, prima di descrivere il difetto come attivo. Guard `identita-non-indovinata`. L'appartenenza si prova da dati strutturati (`agent_processes.label` del pid, `nexus_port_allocations.label` della porta), MAI dal nome del programma: `node` non dice quale servizio sia. Quale prova sia pertinente dipende da quale porta si guarda, e i costruttori lo dichiarano (`own_port_occupant` non ammette la label della porta: quella e' registrata al richiedente per costruzione, e userebbe come prova cio' che deve dimostrare). Non provata = NON si adotta: una porta libera in piu' non costa nulla, una porta condivisa fra due servizi e' l'incidente. Vocabolario «due label sono lo stesso servizio» riusato da `agent_processes::similar_service_labels`. Guard `appartenenza-processo` |
| «Lo stile che il codice DICHIARA e' applicato?» (la resa visiva accertabile, distinta dal gusto) | `nexus-agent-tools/src/ui_styling.rs` (`classify_styling` = il criterio puro -> `StylingVerdict`+`CausaMancata`; `collect_evidence` = l'unico confine col filesystem; vocabolario dei pacchetti in `settings.agent.ui_styling.*`, mig 0655). Tool read-only `ui_styling_audit`, usato dalla figura `ui_ux_designer` e dal revisore `ui_reviewer`. La domanda NON e' «c'e' Tailwind?» (istanza, e inseguirne le varianti a codice e' la toppa della regola H) ma «le classi che il codice scrive hanno una fonte che le produce?», con le fonti come CATEGORIE. Deterministico perche' e' un FATTO, non un gusto: «bello» non e' un criterio e un giudice senza metro moltiplica i rimandi a vuoto; ed e' l'unica voce della lente che leggendo un file per volta non si accerta, perche' la risposta sta nell'incrocio fra sorgenti, manifest, configurazione e fogli RAGGIUNTI. Un foglio mai importato non e' una fonte, un foglio che non copre nessuna classe usata non assolve, un framework in `dependencies` ma non configurato e' una causa distinta (la dipendenza fa da alibi). Cio' che non si e' potuto guardare non e' un difetto: `NonConcludente` e `VocabolarioAssente` esistono perche' «non ho visto» non diventi «non c'e'». Guard `stile-applicato` e `vocabolario-stile-nel-db` |
| Convenzione `step_index` di `agent_steps` (diretta: `iteration*STRIDE+idx`; inversa: `MAX(step_index)/STRIDE` = ultima iterazione che ha lasciato un passo) | `nexus-agent-graph/src/runtime/ports.rs` (`STEP_INDEX_STRIDE`); delegano l'impl concreta (`mcp-core::agent_graph_adapter::agent_step_store`), il mock di test e la chiusura dei sub-run (`subagent_native::mark_run`). L'inversa e' il rimedio ai contatori mai scritti dei run morti senza outcome: un run in timeout chiudeva con `iteration_count=0` a dispetto di decine di step persistiti (misurato il 30/07/2026, run 1845a0ce su bacheca-attivita: 38 step fino a step_index 23000 = iterazione 23), e chi legge i dati attribuiva zero lavoro a run che ne avevano fatto parecchio. `SubRunClosure.iterations` e' `Option`: l'ignoto lo dichiara il tipo (regola M), MAI uno 0, e lo risolve `mark_run` dai fatti persistiti |
| «Qual e' la richiesta dell'utente per QUESTO turno?» | `nexus-agent-graph/src/decisions/turn_task.rs` (`ORIGINAL_TASK_KEY`, `current_turn_task` = la fonte autoritativa, `extract_original_task` = quella piu' il ripiego del solo supervisore). La risposta e' un DATO fissato all'origine da `native_engine::build_initial_state`, non una lettura della cronologia: sul canale interno il ruolo `user` significa «questo lo legge il modello», non «questo lo ha scritto l'utente» — `tool_dispatch` consegna i risultati dei tool come `Message::Human` a blocchi e vi appende i promemoria come blocchi `<system-reminder>`, l'executor inietta i nudge anti-stallo come `Message::Human` di testo, il resume HITL ne aggiunge uno di conferma. Due consumatori indovinavano, ciascuno a modo suo: il supervisore col PRIMO `Human` (in una sessione multi-turno il task del turno prima: incidente Chat 11, 60 iterazioni sul crash frontend invece del task auth), il focus del turno con l'ULTIMO — e dal secondo turno di un run agentico quell'ultimo e' un tool_result o un promemoria, dichiarato al modello come «la richiesta da portare a termine ADESSO» con l'autorita' del system prompt. Il focus NON ha ripiego: senza il dato non scrive nulla, perche' una directive che AFFERMA la richiesta sbagliata e' peggio della sua assenza. Il difetto non era il tag `<system-reminder>` sfuggito al filtro di `turn_focus::user_text_only`: aggiungerlo li' avrebbe zittito il caso peggiore confermando la premessa sbagliata, che la richiesta si riconosca guardando il contenuto dei messaggi (regola M). Guard `task-del-turno`, `chiave-task-del-turno` e `focus-non-legge-la-cronologia` |
| POSIZIONE di un blocco nel system prompt («questo blocco puo' precedere quello?») | `nexus-types/src/system_prompt.rs` (`CONFINE_DI_TURNO`, `appendi_blocco_di_turno`, `componi_system_di_run`, `parte_stabile`) piu' `nexus-agent-graph/src/decisions/context_reduction.rs` (`inject_turn_focus` = l'unica porta d'ingresso del focus, che vi delega la posizione e aggiunge l'idempotenza). Un fornitore riusa il prefisso solo se i primi token sono identici a quelli di una richiesta gia' vista, quindi un blocco RICALCOLATO messo in testa taglia il riuso di tutto cio' che lo segue — e non fallisce nulla, perche' un prompt con la testa instabile e' corretto in tutto tranne che nel prezzo. Il criterio e' un marcatore TESTUALE dentro il system, non un campo a parte: viaggia da solo fino al gateway per qualunque percorso, ed e' la stessa ragione per cui la chiave di raggruppamento e' DERIVATA dal prefisso (`prompt_cache_key` filtra su `parte_stabile`) invece di essere passata dai chiamanti. Dei due consumatori dichiarati in `turn_focus`, il PLANNER l'ha anteposta fino al 30/07/2026 con un `format!` a mano: fra due run la testa in comune scendeva da ~5860 caratteri (il `planner_system_text` fino al RUN_ID) a ~75, sotto qualunque blocco minimo, e nessuna difesa a valle poteva accorgersene perche' il confine non veniva emesso. Chi produce la directive non ne sceglie il posto. Guard `composizione-system-prompt` |
| Corpo della richiesta verso un endpoint OpenAI-compat (preferenza di fornitore a valle + dialetto di cache + quirk di forma) | `nexus-gateway/src/providers/openai_compat.rs` (`OpenAiCompatClient::corpo_della_richiesta`); `complete_with_reasoning` e `stream_with_reasoning` delegano. La duplicazione non era solo debito: era la ragione per cui NESSUN test attraversava quella giunzione — i test del corpo chiamavano `build_request_body` a mano passando dialetto e ordine, cioe' fissando l'assunto da verificare (regola O). MISURATO il 29/07/2026: revocando nei due call site i tre livelli di affinita' del prefisso (`self.cache_keying` -> `ProviderManaged`, ordine -> `None`), `cargo test -p nexus-gateway` restava a 407 passati e 0 falliti, identico alla baseline. Con un punto solo la stessa mutazione fa rosseggiare tre test, uno per livello. Guard `corpo-richiesta-openai-compat` |
| Esito di una suite di test («com'e' andata, e per quale STATO del codice?») | `mcp-core/src/suite_verification/` (`verifica_suite` = memoria + esecuzione + classificazione; `SuiteOutcome` = vocabolario canonico `passed\|flaky\|tests_failed\|setup_failed`; `state_key::digest_albero` + generazione dei servizi vivi = la chiave; `memo::PgSuiteMemo` = memoria su `jobs`, mig project 0014). Delegano i TRE che se la ponevano ognuno per conto suo: `criteria_runner::check_run_command` (final_gate, e per suo tramite il ciclo review), il tool `run_playwright_tests` e `agent_tools::command::record_playwright_job`. Prima l'esito non era legato a NIENTE — la riga `jobs` diceva "passata" o "fallita", non su quale codice — quindi nessuno riconosceva la risposta dell'altro: MISURATE 53 esecuzioni della stessa suite in una serata sulla stessa app (bacheca-attivita, 31/07/2026), 31 rosse e 21 verdi, dove i rossi erano i 2 test sensibili al cold-start di Vite. Il rosso non riprodotto NON e' un difetto (regola M): un fallito i cui test ripassano alla riesecuzione mirata (`--last-failed`, segnale strutturato di Playwright, mai i titoli estratti dall'output) a chiave IDENTICA e' `flaky` — non apre il ciclo di correzione, non boccia il gate, e resta scritto e conteggiato come debito di TEST. Non e' un ritenta-finche'-verde: UNA riesecuzione, un fallimento riprodotto resta fallito e ogni caso non classificabile resta fallito col motivo dichiarato. La chiave comprende la generazione dei servizi perche' una suite E2E li interroga: senza, un `passed` sopravviverebbe allo spegnimento del servizio che lo aveva reso vero. Guard `suite-outcome`, `suite-riconoscimento`, `chiave-di-stato` |
| Alloca+inietta della porta di un web service (`PORT`/`HOST` passati allo spawn) | `mcp-core/src/project_workspace/allocate_port.rs` (`web_service_port_env`): alloca, lega all'unit e PRETENDE che la porta sia bindabile (`port_recovery::wait_port_bindable` — il bind e' la domanda che il processo in avvio porra' al SO, `tcp_probe` ne e' un'altra). Iniettare `PORT` e' una promessa: un framework non in strictPort che la trova occupata non fallisce, ripiega su un'altra porta e il servizio finisce fuori bucket senza che nessuno lo dichiari. I tre percorsi di avvio (pannello Servizi, `service_manager`, tool agente `run_service`) delegano; prima ognuno ricopiava la sequenza e due su tre, in caso di errore, proseguivano «senza PORT iniettato» — cioe' lasciando scegliere la porta proprio al framework. L'esito del bind e' TIPIZZATO (`PortBind::{Libera\|Occupata\|NonInterrogabile}`, classificato in un solo punto da `port_recovery::classifica_bind`): `bind(..).is_ok()` faceva di «occupata da un processo» (`AddrInUse`, 10048) e «il sistema non ha piu' porte effimere» (`WSAENOBUFS`, 10055 — misurato a pool esaurito il 29/07/2026) lo stesso `false`, e il messaggio d'errore mandava a cercare un occupante che nel secondo caso non esiste (regola M). Solo `Occupata` autorizza a parlare di un occupante. Guard `iniezione-porta-libera` |
| «Questa riga di shell chiede la SUITE Playwright?» e, se si', chi la esegue | `mcp-core/src/agent_tools/playwright_cli.rs` (`comandi` = scomposizione della riga, `invocazione_suite` = il riconoscimento, `intercetta_suite` = la conseguenza; l'esecuzione la fa `testing::esegui_suite_delegata` chiamando il runner unico). La suite ha UN esecutore, `tool_run_playwright_tests`: BASE_URL dalle porte allocate, preflight Chromium, streaming live e record `jobs` del pannello. E' anche il punto in cui si innesta l'attesa che il servizio bersaglio risponda stabilmente prima del lancio: con due esecutori, un gate di readiness copre un percorso su due, ed e' il percorso che l'agente sceglie a decidere se la garanzia vale. `run_command` e `run_tests` la lanciavano in proprio e ne registravano il job A POSTERIORI (`record_playwright_job`, rimosso): stessa suite, contratto diverso, e nel pannello un esito indistinguibile da quello vero — misurato il 31/07/2026 su bacheca-attivita, 31 failed / 21 passed su 53 giri della stessa suite coi rossi concentrati nei giri partiti a servizio freddo. Il riconoscimento e' LESSICALE (la riga scomposta come farebbe la shell), non `contains("playwright")`: quel testo faceva di `playwright install`, `show-report` e perfino `cat playwright.config.ts` altrettante «esecuzioni di test». Non e' la regola M al contrario — li' il testo e' il racconto di un esito, qui la command line E' l'oggetto. Una riga che chiede la suite INSIEME ad altri comandi non si delega (il runner eseguirebbe la sola suite, e un `npm ci &&` saltato in silenzio produce un rosso che nessuno sa spiegare): si rifiuta dicendo come spezzarla. Guard `esecutore-suite-playwright` |
| «Dove sta girando questo agente, e cosa puo' davvero invocarci?» (sistema operativo, shell reale, gestori di pacchetti presenti e ASSENTI) | `nexus-agent-tools/src/ambiente.rs` (`rileva` + `AmbienteEsecuzione::blocco`, vocabolario in `settings.agent.environment.*`, mig 0670); l'innesto nei due compositori di system prompt e' `mcp-core/src/prompt_ambiente.rs` (`con_ambiente`). NON e' configurazione: sistema operativo, shell e gestori installati sono cio' che c'e', e si MISURANO — un `settings.agent.platform` sarebbe una seconda verita' da allineare a mano, e il giorno in cui divergesse mentirebbe con l'aria di una configurazione. La shell viene da `nexus_tool_kit::sandbox::agent_shell`, lo stesso punto unico che ESEGUE i comandi: dedurla da un `cfg!(windows)` scritto altrove non vedrebbe l'override `NEXUS_SHELL` e dichiarerebbe all'agente una shell diversa dalla sua (regola O). Misurato il 02/08/2026 su bacheca-attivita: la figura `verify` del sub-run a5f7419c ha speso 180s e 16 iterazioni per scoprire A TENTATIVI di essere su Windows (`which jq` -> exit 1 con un PATH `/mingw64/bin` che gia' lo diceva, poi `sudo apt-get update` -> «binary nexus-sudo-runner non trovato»), e non era ignoranza casuale: il blocco `<privilegi_sistema>` di `system.nexus_base` le ORDINAVA di installare con `sudo apt-get install -y`. Per questo la dichiarazione e la rimozione di quella direttiva stanno nella STESSA funzione: separarle produrrebbe un contesto che dice «apt-get non esiste qui» e poche righe dopo «usa apt-get», che e' peggio di uno incompleto. La direttiva si toglie solo su un'assenza ACCERTATA: `NonInterrogabile` (PATH illeggibile, nome fuori vocabolario) lascia il prompt intatto, perche' «non ho guardato» non e' una prova. Il blocco elenca gli ASSENTI per nome — e' quella riga a chiudere il giro di tentativi, perche' davanti al silenzio un modello addestrato su host Linux prova. Guard `blocco-ambiente-prompt`, `ambiente-dichiarato` |
| «Un run e' morto per tempo scaduto: su che cosa lo stava spendendo?» | `nexus-agent-graph/src/decisions/timeout_cause.rs` (`classifica_causa_timeout` -> `CausaTimeout{RepeatedFailures\|LastAttemptFailed\|NoFailureAtEnd\|NotObservable}`, `nota` = l'unico punto in cui la misura diventa testo); i FATTI li porta `mcp-core/src/agent_tools/subagent_timeout.rs`, che LEGGE `agent_steps` e non giudica. «Tempo scaduto» era vero e inutile: non distingue un run fermo su una strada chiusa da uno che stava lavorando, e solo per il secondo ha senso chiedersi se il tetto del kind sia dimensionato bene — con una parola sola per entrambi, la domanda sul budget resta al buio (misurato il 02/08/2026 su bacheca-attivita: `verify` 19 scadenze su 37 chiusure, il 51%, contro il ~5% di `review` e `sysadmin`, e i completati addensati contro il tetto — max 179,6s su 180). Il fallimento si legge dal CAMPO dell'esito passando dal ponte unico `nexus_types::tool_outcome::RispostaTool::da_testo_legacy`, mai da un riconoscimento scritto nella porta (regola M): `agent_steps.tool_result` e' testo perche' la colonna non ha ancora un campo per l'esito, e quel ponte e' l'unico autorizzato a rileggerlo. La FIRMA («e' la stessa strada?») la costruisce la porta, che sola conosce la forma dell'input del tool: senza il primo token del comando, tre `run_command` su gestori DIVERSI sembrerebbero una ripetizione, e la diagnosi direbbe «bloccato» a chi stava provando alternative. Un exit code != 0 non e' di per se' un tentativo fallito (un build rosso e' un tool riuscito), e un successo in mezzo interrompe la serie. E' una MISURA: non allunga budget e non riavvia nulla. Guard `causa-timeout` |
| Resa dell'elenco servizi di un progetto (quali campi, in quale ordine, con quale vocabolario di stato) | `mcp-core/src/agent_tools/service_listing.rs` (`StatoServizio` = il vocabolario, `ServizioElencato`/`ElencoServizi` = i campi, `elenco_da_processi` = la costruzione dai fatti, `ElencoServizi::testo` = l'unica composizione). Il tool `list_active_services` raccoglie e delega; la sua firma e' migrata a `RispostaTool` (regola Q: l'esito nel campo, il testo composto DAI campi). Il testo lo leggono in DUE, e sono lo stesso testo: il modello, che dall'elenco ricava i `process_id` per `stop_service`/`read_service_output`, e l'utente nel nastro attivita' — quindi l'uuid non si puo' togliere, ma non deve stare dove ruba lo sguardo (riga principale: stato, label, porta, eta'; riga secondaria: id intero, pid, comando abbreviato). Prima la riga si componeva a mano, un `push_str` per colonna, con l'ordine dettato dalle colonne del `SELECT`: misurato il 02/08/2026 su bacheca-attivita, `[?] service-66f4bf72 (id: e3711047-…, pid: 24220, status: failed, exit: 1, avviato: 2026-08-02T07:42:13.968450+00:00)` piu' una riga `cmd:` — tre servizi cosi' riempivano il riquadro (font monospaziato, a capo automatico, taglio a 500 caratteri) e QUALE/VIVO/PORTA annegavano, con la PORTA che non c'era affatto. Il timestamp non si formatta meglio, non si appiattisce alla fonte: `ProcessSummary.created_at` porta un `DateTime<Utc>` e l'eta' si calcola (ri-parsare un RFC3339 gia' formattato per riottenere il dato che c'era e' la regola M al contrario). La porta si lega alla label ESATTA — `find_or_allocate` chiava l'allocazione su `(project_id, label)`, quindi la corrispondenza e' strutturale; il vocabolario largo di `similar_service_labels` («frontend» e «web» sono lo stesso RUOLO) risponde a un'altra domanda e attribuirebbe a un servizio la porta di un altro. Guard `resa-elenco-servizi` |
| Contratto di persistenza di UN passo dell'agente (che cosa arriva in colonna su `agent_steps`) | `nexus-agent-graph/src/runtime/ports.rs` (`PersistedStep` = i campi, `StepStatus` = il vocabolario canonico chiuso con `from_is_error`); il produttore e' `nodes::tool_dispatch`, l'unica impl che scrive e' `mcp-core/src/agent_graph_adapter/agent_step_store.rs`, che NON interpreta: destruttura e mette in colonna. Il contratto erano due `Value` opachi, e i due lati usavano chiavi DIVERSE per la stessa cosa: il produttore scriveva `{"tool_name","tool_input"}`, l'impl leggeva `block.get("name")`/`block.get("input")`, e lo `status` che il produttore aveva appena derivato da `is_error` veniva scartato per un letterale `"completed"`. Nessuno dei due lati era sbagliato da solo: mancava la giunzione come contratto, e nessun tipo la imponeva. Misurato il 02/08/2026 sul DB di bacheca-attivita: 8860 righe su 8860 con `tool_name` vuoto e `status='completed'` — una sola riga di GROUP BY, nessuna eccezione — dentro cui 536 fallimenti reali su 159 run distinti, letti come successo da quattro consumatori. Coi campi il disallineamento non e' piu' rappresentabile: un rinominamento lo ferma il compilatore invece del DB. `StepStatus` ha due sole varianti e non e' una semplificazione: la fonte e' un `bool` strutturato sempre presente, quindi qui non esiste un ignoto da rappresentare (gli altri valori che la colonna ammette descrivono stati transitori del RUN, che questo produttore non emette mai). Lo storico lo rimette in colonna la migrazione project 0015, che dichiara anche cio' che NON si recupera: le decisioni gia' prese su quei dati restano quelle che furono. Guard `passo-persistito` |
| Istruzioni apprese nel prompt (QUALI regole del progetto entrano, e COME si rendono) | `mcp-core/src/prompt_learned.rs` (`LearnedInstructions::{load, section}`, `ORDINE`, `MAX_REGOLE`); delegano i DUE compositori, `Orchestrator::compose_prompt` (turno singolo) e `compose_agent_system_text` (run agentico), come per [[prompt_memories]]. Il distillatore le ricava dall'esperienza operativa e le scrive in `nexus_learned_instructions` con `status='active'`, il pannello admin le mostra e le fa correggere, il template `system.learned_instructions_block` esisteva col suo `{{rules}}` — e NESSUN compositore leggeva la tabella. Misurato il 03/08/2026 sul DB vivo: 68 regole `active` e 3 `proposed`, con ogni lettura della tabella confinata dentro `learned_instructions.rs` stesso (il distillatore che scrive, le rotte admin che mostrano). Il ciclo di apprendimento era completo tranne nell'unico punto in cui serviva. Non era astratto: fra le 68 attive c'erano «Evita URL hardcoded come localhost/127.0.0.1» e «Non scegliere manualmente le porte nei file .env», cioe' esattamente i due difetti che l'app generata la sera prima aveva riprodotto entrambi. Il blocco sta nella parte STABILE, a differenza delle memorie: quelle sono richiamate per pertinenza alla domanda (cambiano a ogni messaggio), queste sono le regole del progetto e cambiano solo quando il distillatore gira. L'ordine e' deterministico (`confidence DESC, id`) perche' due run producano gli STESSI byte: senza il secondo criterio due righe a pari confidenza uscirebbero in ordine variabile e il prefisso cambierebbe senza che nulla sia cambiato. Il testo viene dal template e, se il template manca, il blocco NON entra — nessun letterale di ripiego (regola G). Guard `istruzioni-apprese-nel-prompt` |
| «Come sa il MODELLO che un tool e' fallito?» (l'esito sul WIRE, e il degrado dove il dialetto non ha il campo) | Il campo attraversa la catena senza mai passare dal testo: `ContentBlock::ToolResult.is_error` -> `extract_tool_results` (`nexus-agent-graph::nodes::executor`, che e' anche il punto unico della lettura di quei blocchi per i DUE percorsi, `Message::Tool` e turno a blocchi) -> `LlmMessage.is_error` -> `GwMessage.is_error` -> `nexus-gateway::types::LlmMessage.is_error` -> adapter. Il primo consumatore di quell'esito e' il MODELLO, e li' non c'era un campo: finche' i tool scrivevano il marker `U+274C` in testa al risultato la dichiarazione arrivava comunque, ma per ogni tool migrato a `RispostaTool` (regola Q) il marker non c'e' piu' e il fallimento diventava un tool_result indistinguibile da uno riuscito — la migrazione toglieva il ponte testuale senza che quello strutturato esistesse. `Option<bool>` perche' i casi sono TRE: l'assenza e' «non dichiarato» (messaggio ricostruito dal sanitizer, chiamante che non parla questa versione del contratto) e non deve degradare a successo. Al confine ogni dialetto fa cio' che il suo protocollo consente, e nessuno finge: Anthropic ha `is_error` sul blocco `tool_result` e lo emette NATIVO (li' il testo resta testo); OpenAI-compat e Google non hanno un campo equivalente, e il degrado e' DICHIARATO in un punto solo da `nexus-gateway/src/providers/tool_error_channel.rs` (`testo_con_esito_dichiarato`), che compone il testo DAL campo — direzione consentita dalla regola Q (punto 3), perche' li' il consumatore e' il modello e nessun codice di Nexus rilegge quel prefisso. L'`exit_code` NON sale sul wire: nessuno dei tre protocolli ha dove metterlo, e un campo inventato sul blocco Anthropic sarebbe un HTTP 400 o un esito dichiarato solo a noi stessi; resta nel canale interno, dove lo legge `tool_result_outcome_after` |

| «Il registro delle migrazioni corrisponde ai file del set?» (verdetto per versione, e la sua riparazione) | `nexus-migrations/src/registro.rs` (`classifica` -> `CausaDivergenza{FineRigaNelRegistro\|FineRigaSulDisco\|ContenutoDiverso}`, `censisci` -> `VerdettoVersione{Allineata\|Pendente\|Divergente\|ApplicataSenzaFile}`, `ripara_fine_riga`); `xtask migrate` vi delega `--check` e `--repair-checksums`. La prova che una divergenza sia di soli fine-riga e' COSTRUTTIVA — si genera la variante e se ne confronta l'hash — e l'hash canonico lo produce sqlx, non questo modulo: e' il valore che il migrator confrontera' davvero (regola O), col ponte fra i due verificato da un test. Il verso non e' simmetrico e i tipi lo dicono: col file sporco sul disco si ricrea il FILE, mai il registro, perche' riparare in quel verso fisserebbe l'hash di byte che nessun checkout conforme riprodurra'. MISURATO il 05/08/2026: 2 file su 695 dichiarati `eol=lf` erano materializzati CRLF (mig 117 e 118) e i checksum registrati erano lo SHA-384 dei byte CRLF, quindi il DB accettava SOLO l'albero non conforme che lo aveva migrato — ogni worktree corretto era respinto, e con esso l'avvio di mcp-core. Non e' veicolabile da una migrazione versionata per circolarita': il migrator valida tutti i checksum PRIMA di applicare. Vi sono confluiti anche `--check`, che confrontando le sole liste di versioni diceva «nessuna migrazione pendente» su un DB che rifiutava di migrare, e la nota diagnostica di `errore.rs`, che guardava il solo file e taceva nel verso realmente accaduto. Guard di albero: `scripts/check-eol.sh` |

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

## P. Il lavoro non committato di un worktree non esiste per nessuna query

Il working tree e' l'unico posto del repo senza rete di sicurezza: non e' in
`git log --all`, non e' un branch, non e' una PR, e non ha reflog. Rimuovere il
worktree lo DISTRUGGE. Misurato il 29-30/07/2026: su tredici sessioni CCD
completate, SETTE si sono fermate col lavoro solo nel worktree (fino a 26 file, un
modulo nuovo e due punti unici nuovi fra questi).

### Le cause, misurate sui transcript (non assunte)

Sono tre, e vogliono rimedi diversi. L'ipotesi piu' ovvia — il pre-commit ucciso
per esaurimento di memoria — non e' fra loro: `Killed` compare in **una** sessione
su tredici, e non e' una di quelle rimaste appese.

| causa | quante | come si riconosce |
|---|---|---|
| il commit non e' nel modello di "finito" | 3 | chiude con "Non ho committato — dimmi tu" / "non me l'hai chiesto". La frase compare in 19 sessioni sul totale storico: e' la modalita' prevalente, non un incidente |
| il turno finisce prima del commit | 2 | ultimo evento a meta' di un gate. Il pre-commit fa `cargo check --workspace` con `CARGO_INCREMENTAL=0`: in un worktree a target freddo e' un cold build dell'intero workspace, piu' lungo del turno. Una sessione chiude con "In attesa del completamento del commit" — il commit era nel futuro, non dimenticato |
| blocco reale sull'infrastruttura | 1 | tenta, trova un difetto vero (lefthook risolve la propria root dal percorso del binario in `D:\IDEAI\node_modules`, quindi esegue i gate con CWD `D:\IDEAI` e materializza lo staged del worktree nel repo principale) e si ferma a chiedere. Comportamento corretto: NON forzare `--no-verify` |

Solo la prima e' un difetto dell'agente. La seconda e' un costo, la terza e' un bug
dell'ambiente: chiedere "committa" piu' forte non tocca nessuna delle due.

### Il presidio

`scripts/worktree-wip.ps1` — `-Report` (censimento, exit 1 se c'e' lavoro non
salvato), `-Save`, `-List`, `-Restore`. Test end-to-end incluso il caso distruttivo:
`scripts/worktree-wip-selftest.sh`.

- **Mettere al sicuro non e' dichiarare pronto.** I salvataggi stanno in
  `refs/wip/<worktree>`, FUORI da `refs/heads`: non mergeabili per sbaglio, assenti
  da `git branch`, esclusi da `git push --all`. Uno dei recuperi del 30/07 non
  compilava: un commit automatico su un branch l'avrebbe presentato come finito.
- **Funziona quando il commit non puo' funzionare.** Plumbing (`write-tree`,
  `commit-tree`, `update-ref`, come `crates/mcp-core/src/session_autocommit.rs` fa
  per i progetti utente): nessun hook, quindi nessun gate rosso, cold build o
  lefthook rotto lo blocca.
- **Non dipende dalla sessione.** Tutte e sette avevano l'istruzione nel prompt: un
  rimedio che richieda alla sessione di ricordarsi qualcosa non copre il caso
  osservato. `-Save` e' idempotente sul contenuto, quindi si registra come attivita'
  periodica (comando in testa allo script).
- **Fuori portata del repo:** rifiutare l'archiviazione di un worktree sporco.
  Rimuoverlo e' azione di CCD, non passa da git: nessun hook in cui interporsi.

### Recuperare a mano: due forme sbagliate

Misurato dal selftest, su un worktree con modifica in staging, modifica non in
staging, file nuovo e file cancellato:

- `git diff > patch` confronta il working tree con l'**indice**: perde tutto cio'
  che la sessione aveva messo in staging. E' l'errore del 30/07 su
  `interesting-wozniak` — patch di 25 file applicata pulita, e mancava un terzo del
  lavoro (un modulo nuovo e sei file di un altro crate). Se ne e' accorto solo
  `cargo check`, con "file not found for module".
- `git diff HEAD > patch` recupera lo staging ma perde ancora i file **non
  tracciati**: un diff non li vede. Il 30/07 non si e' visto solo perche' quel
  modulo era in staging.

Non esiste una forma di `git diff` che li copra tutti: usare `-Save` + `-Restore`,
che parte dall'indice reale e fa `add -A`.

## Q. Una risposta agentica dichiara l'esito in un CAMPO, non nel testo

Regola autoritativa, duale della M e con lo stesso peso: **cio' che un tool, un
agente, un nodo o un servizio interno RESTITUISCE deve portare il proprio esito in
campi tipizzati; il testo libero resta per l'umano e non trasporta mai
informazione che qualcuno a valle debba estrarre.**

La M vieta di LEGGERE lo stato tecnico dal testo. Ma finche' il produttore
consegna solo testo, il consumatore non ha alternative: e' costretto a parsare, e
la M diventa inapplicabile per costruzione. Le due regole sono la stessa regola
vista dai due lati del confine, e il lato del produttore e' quello che decide se
l'altro puo' rispettarla.

### Cosa e' vietato

- **Firme che non hanno spazio per l'esito**: `async fn tool_x(...) -> String`. Il
  tipo di ritorno E' il contratto: se non ha un campo per il verdetto, il verdetto
  finira' nel testo, sempre, per necessita'.
- **Marker dentro la stringa** (`"ERRORE: ..."`, un carattere in testa, un prefisso
  convenzionale) come canale dell'esito. Anche quando il marker e' costante,
  documentato e prodotto da un punto unico, resta un campo travestito da prosa:
  chiunque componga quella stringa lo puo' spostare, seppellire o perdere, e non
  c'e' tipo che lo impedisca. Misurato in questo repo: il marker di fallimento dei
  tool viveva in testa alla stringa e `is_tool_failure` lo cercava li', mentre due
  composizioni legittime gli anteponevano prosa di successo — l'apparato anti-loop
  dedicato a quella firma era irraggiungibile per costruzione, e nessun test poteva
  accorgersene perche' il contratto non era un tipo.
- **Numeri e stati incorporati nel testo** perche' il chiamante li rilegga
  (`"EXIT CODE: 0"`, `"3 file modificati"`, `"status: ok"`). Chi li scrive sta
  serializzando a mano in un formato senza schema.
- **Un `Display` usato come protocollo**: se un `to_string()` viene poi analizzato
  da codice, quel tipo aveva bisogno di un campo, non di una `impl Display`.

### Cosa e' richiesto

1. **Il tipo di ritorno porta l'esito.** Minimo: cosa e' successo (enum chiuso, mai
   `bool` quando i casi sono tre — l'ignoto e' un caso), i dati misurati, e il testo
   per l'umano come UN campo fra gli altri. Il vocabolario e' in inglese e canonico
   (regola N).
2. **L'ignoto e' una variante, non un valore comodo.** `NonMisurabile` /
   `Inconclusive` / `Unknown` esistono perche' "non ho potuto guardare" non degradi
   ne' a "va bene" ne' a "e' rotto". Un `Option` che collassa due cause diverse in
   un `None` e' lo stesso difetto in forma piu' educata.
3. **Il testo si compone DOPO, dai campi.** Mai il contrario. Un renderer che
   traduce la struttura in prosa e' legittimo e va bene ovunque; un parser che
   ricostruisce la struttura dalla prosa e' il difetto.
4. **Verso il modello, lo schema e' il contratto.** Quando la risposta la produce
   un LLM, si usano structured output / tool a schema strict con enum ed evidenza
   obbligatoria, mai il pattern-matching sulla prosa (ADR 0034). Cio' che il modello
   DICHIARA resta una dichiarazione: non diventa stato tecnico persistito senza che
   qualcuno l'abbia osservata (vedi corollario in fondo).
5. **Migrazione incrementale ammessa, marker nuovi no.** Dove le firme legacy
   ritornano testo, il ponte esistente resta finche' non e' migrato — ma un
   intervento NUOVO non introduce un altro marker: introduce il campo. Se il campo
   non c'e', il lavoro e' aggiungerlo, non aggirarlo.

### Il corollario che vale per tutti

Una struttura non rende vera l'affermazione che contiene. Un modello che compila
`{"outcome": "done"}` sta dichiarando, non accertando: la forma strutturata elimina
il parsing, non il bisogno di misurare. Il campo va bene come DICHIARAZIONE; per
diventare stato tecnico (`passed`, `resolved`, `status='closed'`) serve
un'osservazione del codice.

### Conseguenza pratica

Prima di scrivere `-> String` su qualunque cosa che un altro pezzo di sistema
dovra' interpretare, FERMATI: quel valore ha un esito, e l'esito vuole un campo.
Un PR che aggiunge un marker testuale nuovo, o una firma che costringe il
chiamante a parsare, e' rifiutato come una toppa (regola H).

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
- `scripts/worktree-wip.ps1` — censimento e messa in sicurezza del lavoro non committato dei worktree (regola P)
- `docs/.nexus-vault/adr/0026-punto-unico-de-duplicazione.md` — catalogo punti unici + meccanismo
- `config/policies/` — profili cloud/onprem/hybrid (contratto gateway LLM)
