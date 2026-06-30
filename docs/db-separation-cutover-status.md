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
- Cutover core mcp-core instradato: session CRUD, `list_chat_messages`,
  `load_session_context`, `insert_message`/`load_message_by_id`, 3 insert del
  motore agente (response/panic/resume), `persist_message_attachments`,
  compattazione, worklog.

## Resta da fare (prima del flip)
1. **Helper mcp-core residui**: SELECT worklog, lista `prompt_corrections`,
   `ai_response_feedback` (JOIN `users` -> id logico), cascata `vector_memory`
   (`upsert_prompt_correction_point` scrive `prompt_corrections`).
2. **Worker cross-progetto** (`run_reaper`, `run_summary_worker`,
   `chat_note_worker`): fanno una query su TUTTI i progetti. Vanno **ristrutturati**
   per iterare l'elenco progetti e girare per-progetto sul pool di ciascuno, con
   `WHERE project_id = $p` (cosi' funziona sia flag off su meta sia flag on su
   `<slug>_nexus`, senza duplicati).
3. **`chat-service`** (crate separato): non vede il registry globale di mcp-core.
   Serve il suo resolver (replica di `project_data_pool` + accesso a
   `project_database_config` / `nexus_data_routing`).
4. **Flip + deploy + test UI** (vedi sotto).

## Procedura di flip (quando 1-3 sono completi)
1. Convertire e committare 1-3; `pnpm verify` verde.
2. Migrare i dati residui dei domini non ancora copiati per i progetti vivi
   (oltre chat+run gia' fatti).
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
