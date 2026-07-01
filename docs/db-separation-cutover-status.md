# Separazione DB per-progetto — stato del cutover (handoff)

Stato al 2026-07-01. Branch: `quality/win-refactor`. Feature flag
`db.project_separation.enabled` = **true** — **GO-LIVE ESEGUITO E CONFERMATO E2E**.
I dati per-progetto di beaty-book vivono ora in `beaty_book_nexus`; lettura e
scrittura instradate, nessuno split-brain (verifica: nuovi messaggi in beaty, meta
fermo). Rebuild col fix PROVISION_LOCKS + ri-migrazione fresca (2808 righe) + backfill
routing completati. Resta opzionale il cleanup dual-presence (copie meta ridondanti).

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
1. **Scan globali processi** — stato: PARZIALE (critico fatto, boot-recovery residuo).
   - `task_watchdog` (riconciliazione PERIODICA `agent_processes`, >10min senza
     heartbeat): INSTRADATO per-progetto (`list_all_project_ids` + `project_data_pool_from`,
     UPDATE idempotente). A flag ON copre i processi orfani di TUTTI i progetti. FATTO.
   - `main.rs` boot-recovery (`SELECT id, pid FROM agent_processes` all'avvio,
     ~riga 372): resta META-only. Instradarlo per-progetto NON e' banale per due
     vincoli reali: (a) l'ordine di init — `init_global_pools` (registry pool) e' a
     riga 655, DOPO il boot-recovery, quindi `project_data_pool_from` a riga 372
     troverebbe il registry non inizializzato e ricadrebbe sempre sul meta; (b)
     `spawn_reattach_monitor` ha un side-effect NON idempotente (spawna un task di
     monitoring), quindi iterare i progetti a flag OFF — dove `list_all_project_ids`
     ritorna comunque tutti i progetti mappati al meta — creerebbe monitor duplicati.
     Il porting Windows di `spawn_reattach_monitor` (ramo `#[cfg(windows)]`, poll
     `process_alive`) e' invece COMPLETO. Impatto del gap: a flag ON, dopo un restart
     di mcp-core, i processi VIVI dei progetti non vengono re-attached immediatamente.
     PRECISAZIONE (audit): la copertura del `task_watchdog` periodico e' PARZIALE per i
     processi VIVI — il watchdog marca `failed` solo i processi BLOCCATI (>10min senza
     heartbeat), ma NON re-attacha un monitor ai processi vivi; quindi un processo di
     progetto ancora vivo dopo un restart resta senza monitoring di liveness finche' non
     ne parte uno nuovo. Il blocco reattach `agent_processes` a `main.rs:372-418` usa
     `&db` diretto (meta), quindi NON e' instradato affatto (distinto dal reap dei run,
     che invece e' instradato in `run_reaper`). Degrada (monitoring parziale dei processi
     vivi al restart), NON corrompe. Il fix incrementale (passo boot per-progetto DOPO
     il registry, con helper condivisa + `seen` per il dedup, regola L) va fatto con
     test di avvio dedicati, non e' bloccante per il flip.
2. **KB (wiki_docs) resta sul meta**: i worker wiki leggono run/chat dal pool
   progetto ma scrivono `wiki_docs`/Qdrant sul meta (dominio KB non migrato).
   Multi-tenant per `scope`/`project_id`, non split-brain.
3. **Flip + deploy + test UI** (vedi sotto).
4. **Race provisioning progetti freschi**: RISOLTA (commit `e786a82`,
   `PROVISION_LOCKS` in `provision.rs`). A flag ON, piu' worker che aprivano lo stesso
   DB per-progetto MAI provisionato eseguivano il Migrator in parallelo ->
   "_sqlx_migrations non esiste". Ora serializzato per-progetto (double-checked
   locking). RICHIEDE UN REBUILD del binario per avere effetto (il binario in
   esecuzione non lo contiene ancora).

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
   NB (audit): il backfill sopra e' SESSION-only. Gli endpoint by-run/by-message si
   risolvono derivati (via `session_id` + resolver self-healing), quindi il backfill
   run non e' strettamente necessario, ma per gli endpoint by-run PURI il primo accesso
   dipende dal fallback iterativo. Per completezza/performance conviene backfillare
   anche i run: `INSERT INTO nexus_data_routing (entity_kind, entity_id, project_id)
   SELECT 'run', id, project_id FROM agent_runs WHERE project_id IS NOT NULL ON CONFLICT DO NOTHING;`
   (stato attuale: 24/28 run di beaty gia' in routing, 4 coperti solo dal fallback).
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

### Validazione tecnica del flip (fatta questa sessione)
Il flip e' stato acceso e verificato end-to-end sul binario corrente, poi
rollbackato. Due bug del percorso di flip sono emersi e stati risolti:
- **`\restrict`/`\unrestrict`**: `db/migrations/project/0001,0002` contenevano i
  meta-comandi psql emessi da `pg_dump`, che rompono il Migrator sqlx ("errore di
  sintassi presso \\"). Rimossi + `CREATE ... IF NOT EXISTS` (commit `358862f`).
- **Race provisioning**: risolta con `PROVISION_LOCKS` (commit `e786a82`, punto 4 sopra).

Verifica: pre-provisionando `beaty_book_nexus` con lo schema corretto (owner
`nexus_app` — NON `nexus_admin`, altrimenti il migrator connesso come `nexus_app`
prende "permesso negato per _sqlx_migrations") e i 3 record `_sqlx_migrations` con i
checksum SHA-384 REALI dei file su disco, a flag ON il Migrator SALTA pulito (nessun
errore), il binario apre 1 connessione `nexus_app` attiva su beaty (prova che il
routing punta al DB per-progetto), health `ok`. Poi rollback a flag OFF.

### Prerequisiti per il GO-LIVE definitivo (non ancora fatto)
1. **Rebuild** del binario per includere `PROVISION_LOCKS` (`deploy/dev-build.ps1 -Rust`
   + restart via `deploy/dev-start.ps1`): senza, i progetti FRESCHI (mai provisionati)
   colpiscono ancora la race al primo accesso concorrente.
2. **Ri-migrazione FRESCA** dei dati per ogni progetto vivo: `beaty_book_nexus` e'
   stato ricreato VUOTO per validare il Migrator. Prima del go-live va ripopolato dal
   meta (il meta e' la fonte AVANTI: 68 chat_messages, 28 agent_runs) + backfill
   `nexus_data_routing` (procedura step 2), altrimenti a flag ON la cronologia storica
   non sarebbe visibile (la UI leggerebbe da beaty vuoto).

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

## Ambiente di esecuzione (aggiornato)
Lo stack NON gira piu' come servizi Windows WinSW: i 10 servizi applicativi
(`nexus-web-ide`, `nexus-chat`, `nexus-mcp-core`, ...) sono stati disinstallati
(`deploy/uninstall-winsw-services.ps1`, include il `nexus-chat` orfano del crate
eliminato). Restano servizi Windows SOLO i 3 database (`postgresql-x64-17`,
`nexus-pg-nexus`, `nexus-pg-app`, i dati devono persistere). Lo stack applicativo
gira come PROCESSI dev via `deploy/dev-start.ps1` (build `deploy/dev-build.ps1`,
nessun admin richiesto, niente lock `.exe` da servizio elevato durante il rebuild).

## Sicurezza / stato attuale
Flag **true** — **GO-LIVE ATTIVO**. `beaty_book_nexus` e' la fonte viva dei dati
per-progetto (chat/run); il meta conserva le copie pre-flip (dual-presenza) come rete
di rollback. Rollback in un comando: `UPDATE settings SET value='false'` -> torna a
leggere/scrivere dal meta entro ~30s (i dati scritti in beaty a flag ON restano li',
da ri-sincronizzare se si vuole tornare stabilmente su meta).

Verifica E2E del go-live (2026-07-01, ~15:18): inviato un messaggio da UI su beaty-book
-> `beaty_book_nexus.chat_messages` 68 -> 70 (user + assistant), `nexus.chat_messages`
FERMO a 68 (nessuno split-brain), `nexus_data_routing` auto-aggiornato. Migrator salta
pulito (fix PROVISION_LOCKS nel binario rebuiltato), 2 connessioni nexus_app attive su
beaty, health ok.

### Cleanup dual-presence (prossimo passo, opzionale)
Le copie meta pre-flip (chat/run di beaty) sono ora ridondanti. CONSIGLIO: attendere
qualche giorno di go-live stabile prima di rimuoverle (restano la rete di rollback).
Poi `scripts/db-cleanup-dual-presence.sh 98138624-... --apply` + `VACUUM (ANALYZE)`.

### Nota: rischio flip accidentale (ora mitigato)
Il flag e' scrivibile dalla UI admin generica (`admin-service/src/settings.rs`,
`PUT /setting/:key`, nessuna whitelist). Con beaty ora POPOLATO il rischio del flip
accidentale e' neutralizzato (un flip off->on->off non nasconde piu' la cronologia).
Resta valido come hardening futuro: whitelist/guard sui setting critici.

## Migrazioni introdotte
`0494` connection_role, `0495` seed flag, `0496` nexus_data_routing,
`0497` drop tabelle morte, `0498` seed retention,
`db/migrations/project/0001_chat.sql`, `0002_run.sql`, `0003` drop dead.
