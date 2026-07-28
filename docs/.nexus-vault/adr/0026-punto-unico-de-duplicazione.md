---
id: adr-0026-punto-unico-de-duplicazione
kind: adr
title: "ADR 0026 - Punto unico di controllo: de-duplicazione e meccanismo di centralizzazione"
slug: 0026-punto-unico-de-duplicazione
tags:
  - adr
  - single-source-of-truth
  - de-duplicazione
  - regola-L
  - tooling
  - enforcement
auto_generated: false
nexus_meta_version: 1
---

# ADR 0026 - Punto unico di controllo: de-duplicazione e meccanismo di centralizzazione

## Stato

In corso. Operativizza la regola L del `CLAUDE.md` con tooling di enforcement e
un catalogo dei punti unici. Implementazione a wave (vedi sezione "Roadmap").

## Contesto

La regola L ("ogni decisione/logica ha UN solo punto di controllo; i call site
delegano, non re-implementano") era un principio senza enforcement automatico.
Un'analisi sistematica del monorepo ha trovato violazioni in tutti e tre i layer,
con divergenza silenziosa gia' in atto. Esempi verificati:

- `parse_user_id` definito 2 volte (`crates/nexus-types/src/lib.rs` + copia
  `pub(crate)` in `crates/mcp-core/src/projects/mod.rs`), ~56 call site con import incoerenti.
- `get_setting` con 5 varianti a semantica errori incompatibile (alcune ingoiano
  l'errore e ritornano `None`, altre lo propagano) -> bug subdoli.
- `TemplateCache` duplicata identica tra `mcp-core` e `admin-service`, due cache TTL
  non coordinate sullo stesso DB.
- Python: pattern "cache 60s" copiato in 4 punti; `psycopg2.connect()` con
  connection-string copiata in ~31 file; estrazione JSON-da-markdown in 3+ nodi.
- Frontend: componenti condivisi esistenti ma quasi inutilizzati (1 uso su ~20 pagine).
- Cross-language: 4 implementazioni Python del chunking divergenti dall'unica Rust.

Senza una misura e un gate, il debito puo' solo crescere a ogni PR.

## Decisione

1. **Meccanismo di centralizzazione per-caso** (non libero), allineato a
   "composition over inheritance".
2. **Catalogo dei punti unici** come riferimento autoritativo (sotto).
3. **Enforcement automatico e duraturo**: misura jscpd + gate "ratchet" + guard
   testuale, innestati in pre-commit e CI.

### Meccanismo di centralizzazione (criterio per-caso)

L'ereditarieta' di classi si usa SOLO per relazioni "is-a" reali e poco profonde,
mai per riusare codice. Vincolo pratico: Rust non ha ereditarieta' di classi
(si usano `trait`/generics/composizione); in React ereditare componenti e' anti-pattern.

| Natura della logica duplicata | Meccanismo corretto | Esempio |
|---|---|---|
| Stateless (calcolo puro, IO singolo) | funzione in un modulo | `get_setting`, `parse_user_id`, `extract_json_block`, `chunk_text`, `formatDate` |
| Stato + comportamento | classe/struct incapsulata + generics | `TtlCache<K,V>` (Rust), `db_pool` (Python) |
| Varianti polimorfiche su contratto comune | `trait` (Rust) / ABC-Protocol (Python) + composizione | `provider_health`, `capability`; provider su `brain/providers/base.py` |
| UI | composizione (componenti + custom hooks) | `AdminPageHeader`, `useListData`, `AdminModal` |

Anti-pattern vietati: incapsulare una funzione stateless in una classe con
sottoclassi ("regno dei sostantivi"); gerarchie di ereditarieta' profonde per
condividere codice (fragile base class, accoppiamento al genitore).

### Catalogo dei punti unici

| Concern | Modulo/funzione autoritativa | Stato |
|---|---|---|
| Gate disponibilita' provider | gate unico (ADR 0020) | esistente |
| SQL-injection detector | detector unificato (ADR 0021) | esistente |
| Capability modello (vision/tool/thinking) | vista `v_model_capabilities` (mig 0318) + classificatore `model_catalog_sync.rs::{classify_capabilities, infer_capabilities_from_name}` (ADR 0024) | esistente |
| Routing/default/purpose model | `routing_matrix.rs` + tabelle mig 0101/0102 | esistente |
| Selezione catalog: eleggibilita' + pesi scoring | `orchestrator/model_selection.rs` (`EligibilityFilter`, `select_models_tierchain`, `default_scoring_weights`) + riga sentinella mig 0379 (ADR 0030) | esistente |
| Scrittura del tier: precedenza fra le fonti (`manual` > `measured` > `synced` > fonte ignota) | `orchestrator/model_service.rs` (`apply_tier`, `TierSource`, `puo_sovrascrivere`); il sync dell'indice (`refresh_tier_prior`) e la batteria (`SQL_QUALIFIED`) delegano. Guard `tier-write` in `check-single-source.sh` | 2026-07-16 |
| Identita' utente/progetto | `crates/nexus-types/src/lib.rs` | Wave 1 |
| Identita' temporale del binario in esecuzione (`build_time` esposto da `/health`) | `crates/nexus-types/src/build_info.rs` (`running_binary`: mtime del proprio eseguibile, letto e memoizzato all'avvio); `domain::HealthSummary::new` popola i campi cosi' il call site non li sceglie. Guard `running_binary` + `build-stamp` in `check-single-source.sh`. Prima nasceva da `mcp-core/build.rs` (`SystemTime::now()` -> `env!("BUILD_TIMESTAMP")`), che cargo rieseguiva solo al cambiare di `build.rs`: misurato 27/07/2026, `/health` dichiarava il 20/07 su un binario linkato quel giorno (hash identico fra `target\debug` e `D:\IDEAI-runtime\bin\debug`) | 2026-07-27 |
| Cache TTL (Rust) | crate `nexus-cache` (`TtlCache<K,V>`) | Wave 2 |
| Lettura settings (Rust) | `nexus-auth::settings` (`get_setting`, `get_setting_nonempty`, bool/int) | Wave 3 |
| Health/cooldown provider | `mcp-core/src/provider_health.rs` | Wave 5 |
| Pool DB (Python) | `brain/utils/db_pool.py` | Wave 6 |
| Cache TTL (Python) | `brain/utils/ttl_cache.py` | Wave 6 |
| Estrazione JSON da markdown (Python) | `brain/utils/json_extract.py` | Wave 6 |
| Intent canonici (Python) | `brain/router/intents.py` | Wave 6 |
| Fetch HTTP frontend | `apps/web-ide/lib/api/_shared.ts` (`fetchJson`) | esistente |
| Formatter frontend | `apps/web-ide/lib/format.ts` | Wave 7 |
| Chunking testo | `crates/mcp-core/src/rag/chunker.rs` (riferimento) + `brain/utils/text_chunk.py` (paritetico, golden test) | esistente |
| Classificazione errore provider (testo) | `crates/mcp-core/src/provider_error_classifier.rs` (paritetico a `brain/providers/error_handler.py`, golden test) | esistente |
| Path-safety workspace (resolve_workspace_target, path_within) | `crates/nexus-types/src/workspace_paths.rs` (adapter HTTP in `mcp-core::projects`) | esistente |
| Esecuzione comandi git (run_git_command, GitCommandOptions) | `crates/nexus-types/src/git_exec.rs` (re-export in `mcp-core::projects`) | esistente |
| Reindex vettoriale post-mutazione (contratto tool agente) | trait `FileReindexer` in `nexus-agent-tools::context_core` (impl `NeuralFileReindexer` in mcp-core, delega a `reindex_single_file`) | esistente |
| Estrazione JSON da output LLM (Rust, paritetico ADR 0032) | `crates/nexus-types/src/llm_json.rs` (re-export in mcp-core via `crate::llm_json`) | esistente |
| Estensioni file di codice (CODE_EXTENSIONS) | `crates/nexus-types/src/code_files.rs` (re-export in `mcp-core::projects`) | esistente |
| Servizi AI del wiki (embed/completion/purpose) | trait `WikiAiServices` in `nexus-wiki::deps` (impl `AppStateWikiAi` in `mcp-core::wiki`, delega a NeuralCoreClient + internal_routing) | esistente |
| Identita' e dedup servizi di progetto (label generiche, similarity con split trattini, stop duplicati pre-spawn) | `mcp-core/src/agent_processes.rs` (`is_generic_service_label`, `similar_service_labels`, `stop_similar_running_services`); delegano: tool `run_service`, wizard install Windows, start/restart pannello, launch run config; visibilita' voci pannello Windows in `project_workspace/services.rs::visible_windows_services` | esistente |
| Guard placeholder di redazione nei tool_input (`[REDACTED:<tipo>]`, `__NEXUS_<KIND>_<N>__` copiati come valori — incidente Beaty-Book 2026-07-02) | `mcp-core/src/security/redaction_guard.rs` (`find_redacted_placeholder`, `enforce_no_redacted_placeholder`); policy `secret/no_redacted_placeholder` (mig 0509); delegano: `enforce_on_write` (write_file/edit_file), `run_command`, `run_service`, `nexus_db_query` | esistente |
| Secret scanner su stringa in-memory (scan, redazione totale, redazione context-preserving) | `nexus-tool-kit/src/secret_text_scanner.rs` (`SecretScanner::{scan, redact, redact_secrets_preserving_context}`); re-export gateway in `nexus-gateway::redaction::secret_scanner`; delegano: pipeline DLP gateway, `mcp-core::agent_processes::redact_secrets_for_persistence` (flush + lettura output processi, `terminal_ws`) | esistente |
| Risoluzione pool DB metadati per-progetto (registry `project_database_config` role `nexus_metadata`, elenco progetti, directory `nexus_data_routing`, cache pool TTL) | crate `nexus-project-pools` (`ProjectPoolError`, `resolve_meta_db_url`, `project_data_pool`, `project_data_pool_by_session`, `list_project_ids`, `project_id_for_entity`, `register_entity_routing`); risoluzione READ-ONLY con errore tipizzato (regola M), niente provisioning; separazione SEMPRE attiva — il flag `db.project_separation.enabled` e' stato rimosso (mig 0527), nessun ramo di configurazione; delegano: `mcp-core::project_db_routes` (che vi aggiunge il layer provisioning+migrazione con lock per-progetto, NON replicabile fuori processo: il migrator sqlx non e' concurrency-safe), `WikiDeps::list_project_ids`, admin-service, billing-service, nexus-tool-kit | esistente |
| Derivazione del nome DB fisico del progetto (sanitizzazione slug + troncamento suffix-aware + fallback su `project_id`) | `mcp-core/src/project_db_routes/provision.rs` (`derive_project_db_name`, prende `DbRole` cosi' il budget si calcola sul suffisso effettivo); delega: `agent_tools::command::ensure_project_db_url`, che ne teneva una copia (`sanitize_app_db_name`) divergente solo sul troncamento (base 52 vs 56): entrambi i nomi stavano sotto il NAMEDATALEN di Postgres (63), quindi la divergenza non produceva errori ma DUE database fisici per lo stesso progetto su slug oltre i 52 caratteri. Guard + 7 test di regressione | 2026-07-14 |
| Ciclo di vita servizi di progetto (list/start/stop/restart, porte in ascolto, stato manager) multipiattaforma Linux/Windows | `mcp-core/src/project_workspace/service_manager.rs` (trait `ServiceBackend` + `active()` con type alias `#[cfg]`; backend `SystemdUserBackend` / `WindowsProcessBackend`; tipi neutri `ServiceState`/`ServiceEntry`/`ServiceActionOutcome`/`ManagerStatus`/`PortListener`; `acted:bool` come segnale strutturato regola M); delegano: `restart_project_unit`, `detect_all_port_bindings`, `cleanup_project_ports`, `restart_all_project_services`, tool `nexus_service_status`/`nexus_service_control`, `system_channel_events`, `mark_existing_services` (wizard), `cleanup_systemd_units`. Confine: SOLO servizi di progetto, non i microservizi infrastruttura Nexus (watchdog/deploy). Vedi ADR 0038 | esistente |
| Aggregazione problemi ripetitivi (pannello Problemi: dedup esatto + raggruppamento semantico cross-fonte) | `mcp-core/src/project_workspace/problem_aggregation.rs` (`problem_group_key`, `aggregate_problems`); delegano: `get_project_problems` in `project_workspace/logs.rs` (vista canonica UI), espansione marker editor `expandProblemMarkers` in `apps/web-ide/lib/api/workspace.ts` | esistente |

| Discendenza di un run (quali altri run compongono il suo lavoro: token, costo, provider) | `mcp-core/src/run_lineage.rs` (`parent_run_by_child`), che legge la parentela run -> run da `nexus_subagent_runs.dispatcher_run_id` (fallback `parent_run_id` per le righe anteriori alla mig project 0010, e solo se e' davvero un run della sessione). Delegano: `trace_store::get_session_traces`, che annota ogni traccia di sub-run col campo `parentRunId`, e da li' il frontend (`tracesForRun` -> `providerCostBreakdown` in `lib/use-chat/activity-stream.ts`). Prima esistevano DUE strade per la stessa domanda: il backend la leggeva dal DB per il cost-cap (`subagent_native::cumulative_cost`, sull'ANCORA di famiglia — concern distinto, resta), il frontend la deduceva dai META-STEP di narrazione, che il review panel non emette. Misurato il 26/07/2026: la barra dichiarava la ripartizione per provider di un run e ometteva openrouter, 4 cicli di review, 21 iterazioni, $0.008453 gia' registrati in `nexus_subagent_runs.cost_usd`. Causa a monte: il review panel e il panel multi-provider costruivano il ctx con `build_ctx(session_id)`, quindi i figli nascevano ancorati alla SESSIONE (che non e' un run) | 2026-07-26 |
| Schema di test del dominio run/chat (le tabelle su cui girano i `#[sqlx::test]`) | crate `nexus-migrations-embedded` (`PROJECT_MIGRATOR` = migrator del set `db/migrations/project`, LO STESSO che `project_db_routes::provision` applica a `<slug>_nexus`); seeder che riempiono i NOT NULL e rispettano le FK in `mcp-core::test_support` (`seed_chat_session`, `seed_agent_run`, `insert_agent_run*`, `seed_plan`, `seed_todo`). Prima: 41 `CREATE TABLE` ricopiati a mano in 15 moduli, per 12 tabelle del set — `nexus_agent_todos` ne aveva DUE, divergenti fra loro e dalla migrazione, e il disallineamento si e' manifestato solo quando `list_todos` ha chiesto `acceptance_criteria` (5 test rossi). Convertendo, il DB ha rifiutato righe che i test creavano da anni: run senza sessione, todo senza piano, step senza `tool_input`. Guard `schema-di-test` (elenco tabelle LETTO dal set, non ricopiato) | 2026-07-22 |
| Forza del vincolo sul provider scelto dall'utente ("preferenza" o "pin duro") | `mcp-core/src/orchestrator/provider_choice.rs` (`ProviderOverrideMode` = vocabolario canonico `preferred\|pinned`, `ProviderChoice::resolve` = unico punto in cui un pin puo' nascere) lato backend; `apps/web-ide/components/chat/provider-choice-logic.ts` (`providerChoiceForSend`, `isProviderPinned`, tooltip) lato UI. Delegano: `chat_messages::handlers` (invio e resend), `run_turn`, `OrchestratorRequest`, `ChatCallSpec`; nel composer, colore del dropdown, pulsante "Forza" e badge `pin non rispettato`. Prima la forza del vincolo NON esisteva: il pulsante "Forza" non arrivava mai al backend e chi leggeva il solo nome del provider la deduceva. Innocuo finche' l'override non aveva effetto; col pin funzionante (vedi riga sotto) ogni selezione dal dropdown sarebbe diventata un vincolo duro — e persistente, perche' `chat_sessions.preferred_provider` lo riproponeva a ogni messaggio successivo. Guard `vocabolario forza-vincolo provider` e `nascita del pin duro`. Vedi ADR 0023 | 2026-07-27 |
| Contabilita' di `ai_usage_ledger`: ogni riga che vi si scrive e il consumo che le quote leggono | crate `nexus-ledger`. Scrittura: `reserve` (riga `reserved` + gate quote), `record_tokens` (riga `finalized` di una chiamata testuale gia' avvenuta), `record_media`, `insert_marker`, `finalize`, `release`, `settle`. Lettura contabile: `active_quotas`, `usage_for_quotas`, `usage_for_scope`. Delegano: `nexus-gateway::server::billing` (che resta l'adapter dai propri tipi: estrae identita' e token da `LlmRequest`/`LlmResponse`, poi chiama) e `mcp-core::billing` (che resta gli handler HTTP admin e i report). Prima erano QUATTRO scrittori in due crate che non si vedevano, con le SQL tenute gemelle a mano — il commento sopra `SQL_UPDATE_LEDGER_FINALIZE` lo dichiarava: "Gemella di `SQL_INSERT_LEDGER_TESTO` nel gateway". Divergenze gia' in atto, tutte sui soldi: (1) nessuno dei due sapeva dell'altro, e una chiamata lasciava DUE righe `finalized` con lo stesso `run_id` — misurato il 27/07/2026, 0.002339 addebitati due volte; invisibile finche' la coppia provider/modello prenotata era impossibile e il listino la prezzava zero; (2) `ai_quota_policies.cost_limit` e' `NUMERIC` e sqlx non lo decodifica in `f64`: la query del gateway aveva il cast `::float8`, quella di mcp-core no — nessuno se n'era accorto perche' senza una quota di COSTO configurata quella query non ha mai avuto una riga da decodificare; (3) il marker del job batch portava una currency `'EUR'` decisa in proprio, con la piattaforma su USD. `settle` e' il punto unico di "chi addebita questa chiamata" e legge il segnale strutturato dal wire, mai l'esito della chiamata (regola M). Il segnale e' `LedgerOutcome` (`written` con la riga scritta, `no_identity`, `write_failed`): tre risposte che prima collassavano tutte in un `Option::None` — anzi in NIENTE, perche' `skip_serializing_if` toglie dal JSON anche il `null` — cosi' "ho deciso di non scrivere" e "non parlo questo contratto" erano indistinguibili, e la seconda vale il doppio addebito (un gateway di build precedente la riga l'ha scritta comunque). Cio' che il chiamante ha potuto LEGGERE e' `Declaration` (`Detta|Muta|Illeggibile`: una dichiarazione presente e non deserializzabile non e' un'assenza, e con un `.ok()` lo diventava in silenzio), e il verdetto e' `Declaration::audit(identita_inviata)`, dove `identita_inviata` si MISURA sulla richiesta vera con `identity_from_metadata` — la stessa regola che applica il gateway alla richiesta che riceve, altrimenti il confronto direbbe soltanto che ciascun lato e' d'accordo con se stesso. Il criterio che ha motivato l'estrazione e' un TEST: `crates/nexus-ledger/tests/una_sola_riga_finalizzata.rs` percorre entrambi i produttori reali sullo stesso database — prima la verifica era spezzata in due crate e la meta' di mcp-core doveva SEMINARE a mano la riga del gateway, dichiarando la premessa in un commento (regola O). Guard `ledger-single-source`. Il confine WIRE fra i due processi (`nexus_gateway::types::LlmResponse` -> `mcp_core::nexus_gateway::GwResponse`, due struct specchiate a mano che nessun tipo condiviso puo' tenere allineate) e' misurato da `mcp-core/src/nexus_gateway.rs::confine_wire_tests`, che serializza col produttore di produzione (`server::billing::record_and_declare`) e rilegge col consumatore vero: mcp-core prende `nexus-gateway` come dev-dependency perche' e' bin-only e nessun terzo crate puo' vedere `GwResponse` | 2026-07-27 |
| Richiesta che la CHAT manda al gateway (modello, pin del provider forzato, coppia prenotata a ledger) | `mcp-core/src/orchestrator/model_routing.rs` (`build_chat_gateway_call` + `ChatCallSpec`/`ChatGatewayCall`); il modello lo risolve UNA volta delegando a `RoutingConfig::resolve_model`, lo stesso punto unico del ramo `(Some(provider), None)` di `resolve_agent_provider`. Delega: `orchestrator::core::execute_via_gateway`. Prima la stessa domanda aveva tre risposte divergenti nel repo e la chat ne usava una quarta, inline: il provider forzato finiva come PREFISSO del nome del modello (`deepseek/coder-large`) invece che in `GwRequest::pin_provider`, cosi' il gateway re-instradava per policy (forzato deepseek, ha risposto google — misurato E2E il 27/07/2026), e la riga di prenotazione portava una coppia inesistente (`deepseek` + un modello di google) perche' il suggerito scavalcava il provider forzato. Vedi ADR 0023 | 2026-07-27 |

### Enforcement

- `jscpd.json` + `scripts/dup-report.sh`: misura cross-linguaggio. Gate "ratchet":
  il numero di cloni puo' solo SCENDERE rispetto a `.dup-baseline.json`. La baseline
  si riallinea al ribasso (`--update-baseline`) dopo ogni wave che riduce il debito,
  mai al rialzo.
- `scripts/check-single-source.sh`: guard testuale che blocca una nuova definizione
  di un punto unico fuori dal suo modulo. I check si attivano per wave.
- `clippy.toml`: punto di config Rust (la dup vera la copre jscpd; clippy non ha
  copy-paste detection in stable).
- `docs/tech-debt-dup.md`: metrica e baseline.
- Innesto: `lefthook.yml` (pre-commit veloce) e `.github/workflows/verify.yml` (gate completo).

## Procedura "prima di scrivere logica che decide"

1. Cerca il concern nel catalogo. Se esiste, **delega** al punto unico.
2. Se e' un concern trasversale nuovo, crea PRIMA il punto unico col meccanismo
   corretto (tabella sopra) e aggiungilo al catalogo.
3. Mai copiare-e-adattare una funzione esistente.

## Definition of Done anti-duplicazione

Un PR che tocca un concern del catalogo deve: passare `scripts/dup-report.sh` senza
aumento di cloni; passare `scripts/check-single-source.sh`; se introduce un nuovo
punto unico, registrarlo in questo ADR e attivare il relativo check.

## Conseguenze

- Positive: divergenza silenziosa impedita strutturalmente; un solo posto da
  modificare per concern; debito misurabile e monotono decrescente.
- Costo: convergenza dei call site a blocchi (il punto unico convive col vecchio
  finche' tutti migrano); falsi positivi jscpd gestiti via `ignore` in `jscpd.json`.
- Cambi di comportamento osservabile (es. semantica errori di `get_setting`) dietro
  flag in `settings` (regola G), con test che cattura la regressione (regola H).

## Roadmap

Wave 0 (questo ADR + tooling) -> 1 (`parse_user_id`) -> 2 (`nexus-cache`) ->
7 (frontend) -> 3 (`get_setting`) -> 4 (capability) -> 6 (Python utils) ->
5 (health/seeding) -> 8 (cross-language). Dettaglio operativo nel piano di campagna.
