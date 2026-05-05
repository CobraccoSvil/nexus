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
- **File `/home/administrator/ideai/`** appartengono al meta-progetto Nexus; modifiche solo se l'utente sta esplicitamente lavorando su Nexus.
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
  - `matrix.purpose_model(purpose_key)` per task interni (chat title, doc gen, ecc.)
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

## Esecuzione locale canonica

- Ambiente di sviluppo: **solo WSL**, percorso `/home/administrator/ideai`. Non modificare mai `D:\Sviluppo\IDEAI` dall'host Windows.
- Tutto gira in locale su WSL; nessun `preview_start` e nessun server remoto.
- Comandi chiave:
  - `pnpm verify` — gate completo
  - `pnpm smoke` — smoke test dei servizi (porte configurabili via env)
  - `pnpm xtask lint-commits <base> <head>` — controllo redazionale commit
  - `./deploy/deploy-local.sh` — build + restart tutti i servizi in locale
  - `./deploy/deploy-local.sh --rust` — solo Rust (es. dopo modifiche backend)
  - `./deploy/deploy-local.sh --web` — solo web-ide (es. dopo modifiche frontend)
  - `./deploy/deploy-local.sh --service mcp-core` — singolo servizio

## Riferimenti incrociati

- `docs/contributing.md` — workflow study -> confirm -> automatic
- `docs/tech-debt-rust.md` — backlog `unwrap`/clippy
- `docs/tech-debt-ts.md` — backlog `any`/strict
- `config/policies/` — profili cloud/onprem/hybrid (contratto gateway LLM)
