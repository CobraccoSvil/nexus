# Separazione DB per-progetto — stato del cutover (handoff)

Stato al 2026-06-30. Branch: `quality/win-refactor`. Feature flag
`db.project_separation.enabled` = **false** (sistema live invariato).

## Obiettivo
Spostare TUTTI i dati per-progetto di Nexus (chat, run, log, KB, costi, ...) dal
meta-DB centrale (`nexus`, porta 5433) a un DB per-progetto `<slug>_nexus` sul
cluster app (5434), accanto al DB applicativo dell'utente `<slug>_app`, cosi' che
ogni progetto sia autocontenuto e portabile.

## Architettura (route-at-helper, regola L)
- **`project_database_config.connection_role`** (`app` | `nexus_metadata`, mig 0494):
  distingue il DB dell'utente dal DB metadati Nexus. Il pannello SQL filtra
  `nexus_metadata` (mai esposto).
- **Resolver core** in `crates/mcp-core/src/project_db_routes/provision.rs`:
  `project_meta_pool_core` / `project_data_pool_core` operano su `(meta, cache)`
  espliciti. Wrapper `&AppState` (`project_data_pool`, ...) per i call-site con
  state; **registry globale** (`init_global_pools` in main.rs, cache CONDIVISA con
  `AppState.project_meta_pools`) + `project_data_pool_from(meta, project_id)` /
  `project_data_pool_by_session_from(meta, session_id)` per gli helper SENZA
  `&AppState`. Provisiona `<slug>_nexus` + applica `db/migrations/project/*` al
  primo accesso. **Flag off -> ritorna il meta-DB (comportamento storico).**
- **Schema per-progetto**: `db/migrations/project/0001_chat.sql` (9 tab) +
  `0002_run.sql` (15 tab). FK verso tabelle GLOBALI (projects, users) rimosse ->
  id logici uuid; FK intra/cross-dominio (es. `agent_runs.run_message_id ->
  chat_messages`) mantenute.
- **Directory di routing** `nexus_data_routing` (mig 0496): `session_id`/`run_id`
  -> `project_id`, per gli handler che hanno solo session/run.
- **Feature flag** `db.project_separation.enabled` (mig 0495, default false).

## Fatto (committato, clippy -D warnings verde a ogni passo)
- Fasi 0-1 **deployate e live** (infra + schema chat + migrator).
- Dati di `beaty-book` migrati in `beaty_book_nexus`: chat (9 tab) + run (15 tab),
  conteggi verificati. ~93K righe **orfane purgate** dal meta-DB (langgraph 50647,
  meta_steps 36701, graph 5240, traces 617) con backup id.
- **Dominio chat** instradato: session CRUD, `list_chat_messages`,
  `load_session_context`, `insert_message`/`load_message_by_id`, 3 insert del
  motore agente (response/panic/resume), `persist_message_attachments`,
  compattazione, worklog (chiamanti in agent_run/handlers/chat_sessions/chat_agent
  + leaf tool `nexus_get_worklog`).
- **Dominio run - punto di assemblaggio** (`native_engine` run_native/run_shadow,
  commit motore run): risolto `run_db` una volta dal session_id e passato a TUTTe
  le store del grafo (run_control->agent_runs, steps->agent_steps, meta_steps,
  todos/plans, verifier_runs, criteria, checkpointer->nexus_graph_checkpoints,
  event_sink->nexus_agent_traces, replay LLM/tool). Punto unico (regola L): le
  ~120 query delle store NON vanno instradate a mano. `todo_store` misto
  (todos/plans su run_db + settings su meta). Restano su meta le porte che leggono
  SOLO config/catalogo globali (summary_store/next_actions/escalation/upscale).
- **Dominio run - call-site standalone** (~24 file, workflow route+verify):
  agent_processes, jobs, agent_runs, nexus_agent_todos, nexus_subagent_runs,
  nexus_agent_traces, monitoring project_workspace, billing counts, trace/rag/
  subagent tool, process_resume. Ogni funzione distingue migrate (->pool progetto)
  da globali (settings/projects/users/git_operations/catalogo ->meta).
- **Worker cross-progetto**: run_reaper + wiki run_summary/chat_note.
- **Hot-path learning**: `orchestrator/core.rs` INSERT orchestrator_runs + UPDATE
  prompt_corrections (retrieved_count) sul pool del progetto.

## Resta da fare (prima del flip)
1. **`chat_learning.rs`** (in corso): query per-progetto instradate; le viste
   admin GLOBALI (`admin_list_feedback_errors` con `LEFT JOIN users` + nessun
   filtro progetto; `run_vector_compaction`; `admin_retrain`) richiedono
   iterazione progetti + split del JOIN verso `users` (meta). Il JOIN
   `prompt_corrections`↔`chat_sessions` (entrambe migrate) e' invece instradabile.
2. **Endpoint feedback message-keyed** (`feedback_error`, `feedback_positive` in
   `chat_messages/handlers.rs`): keyed solo da `message_id`, senza session/project
   a monte -> il pool non e' risolvibile prima di leggere il messaggio (che vive
   nel DB del progetto). **Restano su meta** (coerenti a flag OFF). Fix deliberato:
   passare `session_id` nel body dal frontend, oppure directory `message->project`
   (sconsigliata: una riga di routing per messaggio). A flag ON questi 2 endpoint
   degradano (404), NON corrompono.
3. **Scan globali processi** (`main.rs` boot-recovery riga ~239, `task_watchdog`):
   `SELECT/UPDATE agent_processes` su TUTTI i progetti -> iterare `list_project_ids`
   (pattern run_reaper). NB: codice `/proc`+`kill -0` Linux-specifico, in porting
   su questa branch Windows; degrada (non reconcilia i processi per-progetto), non
   corrompe.
4. **`chat-service`** (crate separato, PROCESSO distinto): **NON nel percorso
   attivo** — `next.config.ts` (righe 84-86) instrada TUTTE le `/api/chat/*` a
   mcp-core (:4000), non a chat-service (:4020): "chat-service e' ancora uno stub
   incompleto". Quindi NON e' bloccante per il flip. Se/quando verra' completato:
   essendo un processo separato non vede il registry in-process di mcp-core ->
   serve un resolver proprio (flag + `nexus_data_routing` + `connection_secret` da
   `project_database_config`, NO provisioning), via crate basso `nexus-project-pool`
   condiviso (regola L).
5. **Dominio costi** ("costi"): tabella `ai_usage_ledger` (per-progetto), scritta in
   `mcp-core/billing.rs` (righe ~368/404/438) DENTRO una transazione `db.begin()`
   che legge/scrive anche le tabelle quote (`read_active_quotas`) NON migrate ->
   transazione MISTA (non instradabile senza untangle). Schema NON ancora in
   `db/migrations/project/*`. `billing-service` (crate) e' **dormiente** (next.config
   riga 90: `/api/billing/*` non routati). Richiede: migrazione schema ledger (+
   decidere se migrare anche le quote o splittare la tx) + dati + routing.
6. **Flip + deploy + test UI** (vedi sotto).

> Nota: tutto il codice instradato e' behavior-preserving a flag OFF
> (`project_data_pool_*` ritorna il meta-DB). I punti 1-5 residui degradano ma NON
> corrompono a flag ON: viste admin incomplete / feedback 404 / recovery processi
> parziale / costi sul meta. Nessuno crea split-brain dei WRITE del dominio
> chat/run gia' instradati.

## Procedura di flip
Il grosso (dominio chat + run) e' instradato: si puo' flippare per i progetti
gia' migrati anche coi punti 1-5 residui aperti (degradano, non corrompono).
1. `pnpm verify` verde.
2. Migrare i dati residui dei domini non ancora copiati per i progetti vivi
   (oltre chat+run gia' fatti) e **backfillare `nexus_data_routing`** per le
   sessioni ESISTENTI (le nuove si auto-registrano a chat_sessions.rs:261; le
   vecchie no -> senza backfill ricadono su meta a flag ON):
   `INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id) SELECT 'session', id, project_id FROM chat_sessions ON CONFLICT DO NOTHING;`
   (da eseguire nel meta-DB per i progetti gia' migrati).
3. `deploy/deploy-local.ps1 -Rust` (rebuild + restart, applica le migrazioni).
4. `UPDATE settings SET value='true' WHERE key='db.project_separation.enabled';`
   (cache flag TTL 30s -> attende max 30s; nessun redeploy).
5. **Test da UI**: aprire la web-ide, creare sessione, inviare messaggi, far
   girare un agent run, verificare cronologia/compattazione. Confrontare che i
   nuovi dati finiscano in `beaty_book_nexus` (psql) e non nel meta-DB.
6. Rollback sicuro: `UPDATE settings SET value='false'` -> torna al meta-DB
   (i dati scritti nel DB-progetto mentre il flag era on restano li').

## Sicurezza / stato attuale
Flag **off**, cutover **non deployato** (gira Phase 0-1): l'app live e' intatta,
dati in dual-presenza (meta-DB + `beaty_book_nexus`). Niente da rollbackare.

## Migrazioni introdotte
`0494` connection_role, `0495` seed flag, `0496` nexus_data_routing,
`db/migrations/project/0001_chat.sql`, `0002_run.sql`.
