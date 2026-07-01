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

## Completato (porting separazione DB)
1. **Dominio chat + run instradato + write split-brain chiuse**: audit avversariale
   del write-path (4 gruppi tabelle) -> instradato il ciclo di vita `agent_runs`
   (spawn/finalize/panic/confirm/cancel), mutazioni sessione, `stop_process`,
   worklog, e le letture incoerenti. Vedi commit be215c6.
2. **Endpoint by-id** (feedback_error/positive, delete_chat_message, confirm/cancel
   run, toggle_project_memory, admin_review/delete): risolti via **directory di
   routing generalizzata** `nexus_data_routing` (entity_kind session/message/run/
   correction/feedback) con `register_entity_routing` ai punti di creazione +
   resolver `project_data_pool_by_{message,run,correction,feedback}_from`. I
   resolver hanno fallback a ricerca iterando i DB-progetto + AUTO-registrazione
   (self-healing) per le entita' inserite inline. Commit 3d78d44.
3. **`chat_learning`**: query per-progetto instradate; viste admin GLOBALI
   (`admin_list_feedback_errors` con split del JOIN `users`, `admin_list_prompt_
   corrections`, `run_vector_compaction`) via iterazione progetti + dedup. Commit
   a100f6f.
4. **`chat-service` ELIMINATO**: era uno stub abbandonato non nel percorso (tutte
   le `/api/chat/*` vanno a mcp-core). Crate + riferimenti rimossi (commit 1519c1b).
5. **Dominio costi**: RESTA sul meta-DB **per design**. Le quote (`ai_quota_policies`,
   scope `user`/`project`/`user_project`) aggregano `ai_usage_ledger` per utente
   CROSS-progetto; migrare il ledger per-progetto romperebbe l'enforcement quota
   utente (impossibile sommare cross-DB in una transazione `FOR UPDATE`). La
   visibilita' costi per-progetto e' data dalla colonna `project_id` filtrata sul
   ledger meta. Non e' un gap.

## Resta da fare (prima del flip)
1. **Scan globali processi** (`main.rs` boot-recovery ~riga 239, `task_watchdog`):
   `SELECT/UPDATE agent_processes` su TUTTI i progetti -> iterare `list_all_project_ids`
   (pattern run_reaper) con dedup. NB codice `/proc`+`kill -0` Linux-specifico, in
   porting su questa branch Windows; degrada (recovery processi parziale), NON
   corrompe. Da fare quando il porting Windows di main.rs si stabilizza.
2. **KB (wiki_docs) resta sul meta**: i worker wiki leggono run/chat dal pool
   progetto ma scrivono `wiki_docs`/Qdrant sul meta (dominio KB non migrato).
   Multi-tenant per `scope`/`project_id`, non split-brain.
3. **Flip + deploy + test UI** (vedi sotto).

> Nota: tutto il codice instradato e' behavior-preserving a flag OFF
> (`project_data_pool_*` ritorna il meta-DB). I residui 1-2 degradano ma NON
> corrompono a flag ON. Nessuno split-brain dei WRITE dei domini chat/run/learning
> gia' instradati.

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
7. **Cleanup dual-presence** (dopo aver verificato il flip per un progetto): le
   copie meta pre-flip dei dati chat/run del progetto sono ora ridondanti. Rimuovile
   per-progetto con `scripts/db-cleanup-dual-presence.sh <PROJECT_ID>` (dry-run) poi
   `--apply` (guardato dal flag, transazione FK-safe), infine `VACUUM (ANALYZE)`.

## Igiene DB (fatta questa sessione)
- **Schema morto rimosso** (mig 0497 + project/0003): droppate 4 tabelle mai scritte
  da alcun codice (nexus_agent_clarifications, nexus_conversation_summaries,
  nexus_e2e_runs, nexus_events_audit). Audit su tutto il repo.
- **Retention** (worker `db_retention` + mig 0498 seed): pota i checkpoint dei run
  terminali (`nexus_graph_checkpoints`, ~72% del meta-DB, ~10MB/run senza pruning) +
  TTL sulla telemetria provider (`*_health_history`). Finestre DB-driven
  (`db.retention.*`). Chiude alla causa la crescita illimitata (regola H).
- **Ridondanza dual-presence**: le 24 tabelle migrate esistono in ENTRAMBI i DB;
  `nexus` e' AVANTI (scritture live a flag OFF), `beaty_book_nexus` e' snapshot stale
  -> ri-migrare fresco prima del flip, poi cleanup (punto 7).

## Sicurezza / stato attuale
Flag **off**, cutover **non deployato** (gira Phase 0-1): l'app live e' intatta,
dati in dual-presenza (meta-DB + `beaty_book_nexus`). Niente da rollbackare.

## Migrazioni introdotte
`0494` connection_role, `0495` seed flag, `0496` nexus_data_routing,
`0497` drop tabelle morte, `0498` seed retention,
`db/migrations/project/0001_chat.sql`, `0002_run.sql`, `0003` drop dead.
