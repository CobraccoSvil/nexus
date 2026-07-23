//! Adapter del trait [`nexus_agent_graph::runtime::ports::TodoStore`].
//!
//! Esegue l'I/O sui todo del DAG su `nexus_agent_todos` via
//! `sqlx`. INVARIANTE (regola H): `list_todos`
//! restituisce `depends_on` come `Vec` (cast `::text[]`), MAI una stringa
//! `"{...}"`, e i todo gia' ordinati per `seq` ASC. Le scritture (`mark_status`,
//! `increment_iteration_seen`) sono gata `Real` (no-op in `ExecMode::Replay`,
//! punto unico del gate shadow). La LOGICA DAG resta pura in
//! `nexus_agent_graph::decisions::dag_scheduler` (questo adapter isola SOLO il DB).

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::decisions::dag_scheduler::{Todo, TodoStatus};
use nexus_agent_graph::runtime::ports::{ExecMode, PlanRow, PortError, TodoStore};

/// Adapter [`TodoStore`] -> `nexus_agent_todos` via `sqlx`.
pub struct PgTodoStore {
    /// Pool del dominio run (todos/plans del progetto): `<slug>_nexus` a flag
    /// separazione ON, meta a flag OFF.
    db: PgPool,
    /// Pool meta-DB per le letture di `settings` (config GLOBALE, regola G: non
    /// per-progetto). A flag OFF coincide con `db`.
    meta: PgPool,
    /// Canali per l'evento live `TodoUpdated` (checklist del piano in chat).
    /// `None` = store senza eventi (letture, test): la scrittura funziona
    /// comunque, semplicemente non viene annunciata.
    events: Option<(nexus_events::ProjectChannels, Uuid)>,
}

impl PgTodoStore {
    /// Costruisce lo store: `db` = pool del dominio run (todos/plans), `meta` =
    /// pool meta-DB per la config globale (`settings`). Senza canali: nessun
    /// evento live (va bene per i percorsi di sola lettura e per i test).
    pub fn new(db: PgPool, meta: PgPool) -> Self {
        Self {
            db,
            meta,
            events: None,
        }
    }

    /// Variante che ANNUNCIA i cambi di stato dei todo (`TodoUpdated`), cosi' la
    /// checklist del piano in chat spunta le voci mentre il lavoro procede.
    ///
    /// Serve perche' sotto todo-isolation lo stato dei todo lo scrive QUESTO
    /// adapter (via `TodoRunner`), non il tool `todos` ne' la UI: erano i due soli
    /// punti che emettevano l'evento, quindi la checklist restava ferma su `[ ]`
    /// per tutto il run anche a todo completati.
    pub fn with_events(
        db: PgPool,
        meta: PgPool,
        channels: nexus_events::ProjectChannels,
        project_id: Uuid,
    ) -> Self {
        Self {
            db,
            meta,
            events: Some((channels, project_id)),
        }
    }
}

/// Mappa lo `status` testuale del DB (CHECK constraint mig 0148) sull'enum del
/// grafo. Punto unico del mapping (regola L). Stringa ignota -> `Pending`
/// conservativo (un todo non riconosciuto resta da fare, non viene saltato).
fn status_from_db(s: &str) -> TodoStatus {
    match s {
        "in_progress" => TodoStatus::InProgress,
        "completed" => TodoStatus::Completed,
        "skipped" => TodoStatus::Skipped,
        "blocked" => TodoStatus::Blocked,
        _ => TodoStatus::Pending,
    }
}

/// Mappa l'enum del grafo sulla stringa attesa dal CHECK constraint del DB.
fn status_to_db(s: TodoStatus) -> &'static str {
    match s {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Completed => "completed",
        TodoStatus::Skipped => "skipped",
        TodoStatus::Blocked => "blocked",
    }
}

#[async_trait]
impl TodoStore for PgTodoStore {
    /// Tutti i todo del run, ordinati per `seq` ASC, con `depends_on` come `Vec`
    /// (cast `depends_on::text[]`). 1:1 con `todo_store.list_todos` Python.
    ///
    /// INVARIANTE (regola H, bug 2026-06-10): senza il cast `::text[]` un `uuid[]`
    /// tornerebbe come STRINGA `'{...}'` rompendo il DAG; il tipo Rust `Vec<String>`
    /// + il cast lo rendono non-eludibile. `id::text` allinea il tipo per il
    /// confronto `depends_on` (id come stringa) nelle funzioni pure.
    async fn list_todos(&self, run_id: &str) -> Result<Vec<Todo>, PortError> {
        let run_uuid = Uuid::parse_str(run_id)
            .map_err(|e| PortError::Tool(format!("list_todos: run_id non UUID: {e}").into()))?;
        let rows = sqlx::query_as::<
            _,
            (
                String,
                Option<i64>,
                String,
                Vec<String>,
                Vec<String>,
                Option<String>,
                Option<String>,
                serde_json::Value,
            ),
        >(
            "SELECT id::text, seq::bigint, status, depends_on::text[] AS depends_on, \
                    write_scope::text[] AS write_scope, content, priority, \
                    acceptance_criteria \
             FROM nexus_agent_todos \
             WHERE run_id = $1 \
             ORDER BY seq ASC",
        )
        .bind(run_uuid)
        .fetch_all(&self.db)
        .await
        .map_err(|e| PortError::Tool(format!("list_todos: query fallita: {e}").into()))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, seq, status, depends_on, write_scope, content, priority, criteria)| Todo {
                    id,
                    status: status_from_db(&status),
                    depends_on,
                    seq,
                    // Testo/priorita' del todo: trasportati per il meta-step "plan"
                    // (presentazione nel nastro), non usati dallo scheduling DAG.
                    content,
                    priority,
                    // PR5 (mig project 0006): scope di scrittura dichiarato dal todo,
                    // letto dalla colonna `write_scope` (TEXT[] NOT NULL DEFAULT '{}').
                    // Retrocompat: i todo senza scope hanno '{}' -> Vec vuoto -> il
                    // gating dell'isolamento a valle (dispatch_wave -> subtasks_are_disjoint)
                    // degrada a sequenziale (comportamento invariato).
                    write_scope,
                    // Criteri di accettazione: la colonna e' JSONB NOT NULL DEFAULT
                    // '[]', quindi un todo senza criteri da' un array vuoto e il
                    // verifier prende il ramo di sempre. Finche' questa colonna non
                    // veniva letta, il verifier NON ne eseguiva mai uno.
                    acceptance_criteria: match criteria {
                        serde_json::Value::Array(a) => a,
                        _ => Vec::new(),
                    },
                },
            )
            .collect())
    }

    /// Il piano esistente per il run (1:1 con `todo_store.fetch_plan`). Trasporta
    /// SOLO `user_intent`/`behavior_mode` (i due campi su cui il planner decide il
    /// riuso intent/mode-aware). `None` = nessun piano (prima pianificazione).
    async fn fetch_plan(&self, run_id: &str) -> Result<Option<PlanRow>, PortError> {
        let run_uuid = Uuid::parse_str(run_id)
            .map_err(|e| PortError::Tool(format!("fetch_plan: run_id non UUID: {e}").into()))?;
        let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT user_intent, behavior_mode FROM nexus_agent_plans WHERE run_id = $1",
        )
        .bind(run_uuid)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| PortError::Tool(format!("fetch_plan: query fallita: {e}").into()))?;
        Ok(row.map(|(user_intent, behavior_mode)| PlanRow {
            user_intent,
            behavior_mode,
        }))
    }

    /// Aggiorna lo status di un todo (UPDATE best-effort). 1:1 con
    /// `verifier_node._mark_todo_status` / `dag_scheduler._mark`: incrementa
    /// `verify_failures` quando il nuovo status e' `blocked` (heuristic anti-stall).
    /// Gata `Real` (no-op in shadow, regola L). Best-effort: errore loggato, `Ok(())`.
    async fn mark_status(
        &self,
        todo_id: &str,
        status: TodoStatus,
        mode: ExecMode,
    ) -> Result<(), PortError> {
        if mode != ExecMode::Real {
            return Ok(());
        }
        let todo_uuid = match Uuid::parse_str(todo_id) {
            Ok(u) => u,
            Err(_) => return Ok(()),
        };
        let status_str = status_to_db(status);
        // `RETURNING` dei due campi che servono all'evento live: sono gia' nella
        // riga che stiamo scrivendo, quindi non costa una query in piu'.
        let res = sqlx::query_as::<_, (Option<String>, Option<i32>)>(
            "UPDATE nexus_agent_todos \
             SET status = $1, updated_at = NOW(), \
                 verify_failures = CASE WHEN $1 = 'blocked' \
                                        THEN verify_failures + 1 \
                                        ELSE verify_failures END \
             WHERE id = $2 \
             RETURNING run_id::text, seq::int",
        )
        .bind(status_str)
        .bind(todo_uuid)
        .fetch_optional(&self.db)
        .await;
        match res {
            Err(e) => {
                tracing::warn!(
                    todo_id = %todo_id,
                    status = %status_str,
                    error = %e,
                    "todo_store: mark_status fallito (best-effort)"
                );
            }
            // Nessuna riga aggiornata (todo inesistente): niente da annunciare.
            Ok(None) => {}
            Ok(Some((run_id, seq))) => {
                // ANNUNCIO del cambio di stato: e' quello che fa spuntare la voce
                // nella checklist del piano in chat mentre il lavoro procede.
                // Sotto todo-isolation questo adapter e' l'UNICO a scrivere lo
                // stato, quindi senza questa emissione la checklist restava ferma
                // su `[ ]` per tutto il run, anche a todo completati.
                if let Some((channels, project_id)) = &self.events {
                    nexus_events::dispatcher::emit(
                        channels,
                        *project_id,
                        nexus_events::event::ProjectEvent::TodoUpdated {
                            run_id: run_id.unwrap_or_default(),
                            todo_id: todo_id.to_string(),
                            seq,
                            status: status_str.to_string(),
                        },
                    );
                }
            }
        }
        Ok(())
    }

    /// Incrementa `iteration_seen` dei todo non terminali del run (telemetria
    /// avanzamento, 1:1 con `todo_store.increment_iteration_seen`). Gata `Real`
    /// (no-op in shadow). Best-effort.
    async fn increment_iteration_seen(
        &self,
        run_id: &str,
        mode: ExecMode,
    ) -> Result<(), PortError> {
        if mode != ExecMode::Real {
            return Ok(());
        }
        let run_uuid = match Uuid::parse_str(run_id) {
            Ok(u) => u,
            Err(_) => return Ok(()),
        };
        let res = sqlx::query(
            "UPDATE nexus_agent_todos \
             SET iteration_seen = iteration_seen + 1 \
             WHERE run_id = $1 AND status IN ('pending', 'in_progress')",
        )
        .bind(run_uuid)
        .execute(&self.db)
        .await;
        if let Err(e) = res {
            tracing::warn!(
                run_id = %run_id,
                error = %e,
                "todo_store: increment_iteration_seen fallito (best-effort)"
            );
        }
        Ok(())
    }

    /// Reminder testuale dei todo per l'executor (1:1 con
    /// `todo_reminder.build_reminder_text`). `None` se:
    /// - `orchestrator.plan_phase_enabled` = false (feature spenta),
    /// - nessun todo per il run,
    /// - todo pending/in_progress sotto la soglia
    ///   `orchestrator.todo_reminder_min_todos` (anti-spam chat brevi).
    ///
    /// Render: checklist con prefix per status + cursore sul todo attivo (fallback
    /// inline ASCII, niente emoji). SOLA LETTURA: nessun gate `mode`. Le soglie sono
    /// DB-driven (regola G), niente hardcode nella logica di business.
    async fn build_reminder_text(&self, run_id: &str) -> Result<Option<String>, PortError> {
        let plan_enabled =
            crate::settings::get_setting(&self.meta, "orchestrator.plan_phase_enabled")
                .await
                .ok()
                .flatten()
                .map(|v| {
                    matches!(
                        v.trim().to_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                })
                .unwrap_or(false);
        if !plan_enabled {
            return Ok(None);
        }
        let todos = self.list_todos(run_id).await?;
        if todos.is_empty() {
            return Ok(None);
        }
        let min_todos: i64 =
            crate::settings::get_setting(&self.meta, "orchestrator.todo_reminder_min_todos")
                .await
                .ok()
                .flatten()
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(3);
        let pending = todos
            .iter()
            .filter(|t| matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress))
            .count() as i64;
        if pending < min_todos {
            return Ok(None);
        }
        // active_todo: primo in_progress, altrimenti primo pending (default trait,
        // regola L: niente selezione duplicata).
        let active = self.active_todo(run_id).await?;
        let active_id = active.as_ref().map(|t| t.id.as_str());

        // Content dei todo non e' in `Todo` (forma minimale del grafo): lo leggo
        // per il render leggibile. Mappa id::text -> content.
        let run_uuid = Uuid::parse_str(run_id)
            .map_err(|e| PortError::Tool(format!("build_reminder_text: run_id non UUID: {e}").into()))?;
        let content_rows = sqlx::query_as::<_, (String, String)>(
            "SELECT id::text, content FROM nexus_agent_todos WHERE run_id = $1",
        )
        .bind(run_uuid)
        .fetch_all(&self.db)
        .await
        .map_err(|e| PortError::Tool(format!("build_reminder_text: content query fallita: {e}").into()))?;
        let content_of = |id: &str| -> String {
            content_rows
                .iter()
                .find(|(rid, _)| rid == id)
                .map(|(_, c)| c.trim().to_string())
                .unwrap_or_default()
        };

        let total = todos.len();
        let mut lines: Vec<String> = Vec::with_capacity(total);
        for t in &todos {
            let box_glyph = match t.status {
                TodoStatus::Completed => "[x]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Blocked => "[!]",
                TodoStatus::Skipped => "[-]",
                TodoStatus::Pending => "[ ]",
            };
            let prefix = if active_id == Some(t.id.as_str()) {
                ">"
            } else {
                " "
            };
            let seq = t
                .seq
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".to_string());
            lines.push(format!("{prefix} {seq}. {box_glyph} {}", content_of(&t.id)));
        }
        let todos_rendered = lines.join("\n");
        let active_seq = active
            .as_ref()
            .and_then(|t| t.seq)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "?".to_string());
        let active_content = active
            .as_ref()
            .map(|t| content_of(&t.id))
            .unwrap_or_default();
        Ok(Some(format!(
            "<todo_list>\n{todos_rendered}\n</todo_list>\n\
             Stai lavorando sul todo {active_seq}/{total}: \"{active_content}\". \
             Procedi voce per voce, aggiorna via nexus_todo_write action='check'."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tabelle META che il set project NON contiene (la config globale vive nel
    /// meta-DB, regola G) e che quindi restano a carico del test.
    async fn create_meta_schema(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE settings ( \
                 key TEXT PRIMARY KEY, \
                 value TEXT NOT NULL \
             )",
        )
        .execute(pool)
        .await
        .expect("create settings");
    }

    /// Preambolo comune: tabelle meta + un run con il suo piano. Ritorna il
    /// `run_id` su cui i todo sono inseribili.
    ///
    /// Il piano non e' decorazione: `nexus_agent_todos.run_id` e' vincolato da
    /// una FK verso `nexus_agent_plans(run_id)`, quindi un todo senza piano - che
    /// la vecchia fixture a mano, priva di FK, accettava - in produzione non puo'
    /// esistere.
    async fn setup_run_con_piano(pool: &PgPool) -> Uuid {
        create_meta_schema(pool).await;
        let run_id = Uuid::new_v4();
        crate::test_support::seed_plan(pool, run_id, Uuid::new_v4()).await;
        run_id
    }

    async fn insert_todo(
        pool: &PgPool,
        run_id: Uuid,
        seq: i32,
        content: &str,
        status: &str,
        depends_on: &[Uuid],
    ) -> Uuid {
        let id = Uuid::new_v4();
        // `project_id` e' NOT NULL nello schema reale: si legge dal piano del run
        // invece di inventarlo, cosi' piano e todo restano coerenti.
        sqlx::query(
            "INSERT INTO nexus_agent_todos (id, run_id, project_id, seq, content, status, depends_on) \
             SELECT $1, $2, p.project_id, $3, $4, $5, $6 \
             FROM nexus_agent_plans p WHERE p.run_id = $2",
        )
        .bind(id)
        .bind(run_id)
        .bind(seq)
        .bind(content)
        .bind(status)
        .bind(depends_on)
        .execute(pool)
        .await
        .expect("insert todo");
        id
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn list_todos_cast_depends_on_e_ordine_seq(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        let a = insert_todo(&pool, run_id, 1, "primo", "completed", &[]).await;
        // b dipende da a; inserito con seq 2 ma per verificare l'ORDER BY lo
        // inseriamo dopo c (seq 3) per disordinare l'ordine fisico.
        let _c = insert_todo(&pool, run_id, 3, "terzo", "pending", &[]).await;
        let _b = insert_todo(&pool, run_id, 2, "secondo", "pending", &[a]).await;

        let store = PgTodoStore::new(pool.clone(), pool.clone());
        let todos = store.list_todos(&run_id.to_string()).await.expect("ok");
        // ORDER BY seq ASC: primo, secondo, terzo.
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[0].seq, Some(1));
        assert_eq!(todos[1].seq, Some(2));
        assert_eq!(todos[2].seq, Some(3));
        // INVARIANTE depends_on: Vec di UN elemento (id di a), MAI stringa "{...}".
        assert_eq!(todos[1].depends_on, vec![a.to_string()]);
        assert!(todos[0].depends_on.is_empty());
        // PR5: nessun write_scope inserito -> DEFAULT '{}' -> Vec vuoto (retrocompat).
        assert!(todos[0].write_scope.is_empty());
        assert!(todos[1].write_scope.is_empty());
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn list_todos_round_trip_write_scope(pool: PgPool) {
        // PR5 (mig project 0006): write_scope persistito e riletto da list_todos.
        // Un todo con scope -> Vec non vuoto; uno senza -> Vec vuoto (DEFAULT '{}').
        let run_id = setup_run_con_piano(&pool).await;
        // Todo con scope dichiarato.
        let scoped = insert_todo(&pool, run_id, 1, "con scope", "pending", &[]).await;
        sqlx::query("UPDATE nexus_agent_todos SET write_scope = $2 WHERE id = $1")
            .bind(scoped)
            .bind(vec!["crates/api/".to_string(), "db/".to_string()])
            .execute(&pool)
            .await
            .expect("dichiara lo scope del todo");
        // Todo senza scope (colonna omessa -> DEFAULT '{}').
        insert_todo(&pool, run_id, 2, "senza scope", "pending", &[]).await;

        let store = PgTodoStore::new(pool.clone(), pool.clone());
        let todos = store.list_todos(&run_id.to_string()).await.expect("ok");
        assert_eq!(todos.len(), 2);
        // seq 1: scope riletto 1:1 (ordine preservato).
        assert_eq!(
            todos[0].write_scope,
            vec!["crates/api/".to_string(), "db/".to_string()]
        );
        // seq 2: nessuno scope -> Vec vuoto (retrocompat bit-identica).
        assert!(todos[1].write_scope.is_empty());
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn active_todo_preferisce_in_progress(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        insert_todo(&pool, run_id, 1, "a", "completed", &[]).await;
        insert_todo(&pool, run_id, 2, "b", "pending", &[]).await;
        let ip = insert_todo(&pool, run_id, 3, "c", "in_progress", &[]).await;
        let store = PgTodoStore::new(pool.clone(), pool.clone());
        let active = store.active_todo(&run_id.to_string()).await.expect("ok");
        assert_eq!(active.expect("attivo").id, ip.to_string());
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn mark_status_real_aggiorna_e_blocked_incrementa_failures(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        let t = insert_todo(&pool, run_id, 1, "a", "pending", &[]).await;
        let store = PgTodoStore::new(pool.clone(), pool.clone());
        store
            .mark_status(&t.to_string(), TodoStatus::Blocked, ExecMode::Real)
            .await
            .expect("ok");
        let (status, vf): (String, i32) =
            sqlx::query_as("SELECT status, verify_failures FROM nexus_agent_todos WHERE id = $1")
                .bind(t)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert_eq!(status, "blocked");
        assert_eq!(vf, 1, "blocked incrementa verify_failures");
    }

    /// Il cambio di stato dev'essere ANNUNCIATO, non solo scritto: e' l'evento
    /// che fa spuntare la voce nella checklist del piano in chat. Sotto
    /// todo-isolation questo adapter e' l'unico a scrivere lo stato, quindi senza
    /// emissione la checklist resta ferma su `[ ]` per tutto il run anche a todo
    /// completati (segnalato dall'utente il 2026-07-22).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn mark_status_annuncia_il_cambio(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        let t = insert_todo(&pool, run_id, 1, "a", "pending", &[]).await;
        let channels: nexus_events::ProjectChannels =
            std::sync::Arc::new(dashmap::DashMap::new());
        let project_id = Uuid::new_v4();
        let store =
            PgTodoStore::with_events(pool.clone(), pool.clone(), channels.clone(), project_id);

        store
            .mark_status(&t.to_string(), TodoStatus::Completed, ExecMode::Real)
            .await
            .expect("ok");

        assert!(
            channels.contains_key(&project_id),
            "il cambio di stato deve essere emesso sul canale del progetto"
        );

        // Lo store SENZA canali resta valido: scrive e basta (percorsi di sola
        // lettura e test non devono essere costretti a fornire i canali).
        let muto = PgTodoStore::new(pool.clone(), pool.clone());
        muto.mark_status(&t.to_string(), TodoStatus::Blocked, ExecMode::Real)
            .await
            .expect("ok anche senza canali");
        let status: String =
            sqlx::query_scalar("SELECT status FROM nexus_agent_todos WHERE id = $1")
                .bind(t)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert_eq!(status, "blocked");
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn mark_status_replay_e_no_op(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        let t = insert_todo(&pool, run_id, 1, "a", "pending", &[]).await;
        let store = PgTodoStore::new(pool.clone(), pool.clone());
        store
            .mark_status(&t.to_string(), TodoStatus::Completed, ExecMode::Replay)
            .await
            .expect("ok");
        let status: String =
            sqlx::query_scalar("SELECT status FROM nexus_agent_todos WHERE id = $1")
                .bind(t)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert_eq!(status, "pending", "in Replay lo status NON cambia");
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn increment_iteration_seen_solo_non_terminali(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        let p = insert_todo(&pool, run_id, 1, "a", "pending", &[]).await;
        let c = insert_todo(&pool, run_id, 2, "b", "completed", &[]).await;
        let store = PgTodoStore::new(pool.clone(), pool.clone());
        store
            .increment_iteration_seen(&run_id.to_string(), ExecMode::Real)
            .await
            .expect("ok");
        let seen_p: i32 =
            sqlx::query_scalar("SELECT iteration_seen FROM nexus_agent_todos WHERE id = $1")
                .bind(p)
                .fetch_one(&pool)
                .await
                .expect("riga");
        let seen_c: i32 =
            sqlx::query_scalar("SELECT iteration_seen FROM nexus_agent_todos WHERE id = $1")
                .bind(c)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert_eq!(seen_p, 1, "pending incrementato");
        assert_eq!(seen_c, 0, "completed NON incrementato");
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn fetch_plan_legge_intent_e_mode(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        // Il piano esiste gia' (i NOT NULL reali sono seminati dal preambolo):
        // il test valorizza i due soli campi che `fetch_plan` legge.
        sqlx::query("UPDATE nexus_agent_plans SET user_intent = $2, behavior_mode = $3 WHERE run_id = $1")
            .bind(run_id)
            .bind("fix")
            .bind("bilanciata")
            .execute(&pool)
            .await
            .expect("valorizza intent/mode del piano");
        let store = PgTodoStore::new(pool.clone(), pool.clone());
        let plan = store.fetch_plan(&run_id.to_string()).await.expect("ok");
        let plan = plan.expect("piano presente");
        assert_eq!(plan.user_intent.as_deref(), Some("fix"));
        assert_eq!(plan.behavior_mode.as_deref(), Some("bilanciata"));
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn build_reminder_none_se_plan_phase_off(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        for i in 1..=4 {
            insert_todo(&pool, run_id, i, "x", "pending", &[]).await;
        }
        // settings vuoto -> plan_phase_enabled default false.
        let store = PgTodoStore::new(pool.clone(), pool.clone());
        let r = store
            .build_reminder_text(&run_id.to_string())
            .await
            .expect("ok");
        assert!(r.is_none(), "feature OFF -> nessun reminder");
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn build_reminder_render_quando_attivo_e_sopra_soglia(pool: PgPool) {
        let run_id = setup_run_con_piano(&pool).await;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('orchestrator.plan_phase_enabled', 'true')",
        )
        .execute(&pool)
        .await
        .expect("set flag");
        sqlx::query("INSERT INTO settings (key, value) VALUES ('orchestrator.todo_reminder_min_todos', '3')")
            .execute(&pool)
            .await
            .expect("set soglia");
        insert_todo(&pool, run_id, 1, "fatto", "completed", &[]).await;
        insert_todo(&pool, run_id, 2, "in corso", "in_progress", &[]).await;
        insert_todo(&pool, run_id, 3, "da fare", "pending", &[]).await;
        insert_todo(&pool, run_id, 4, "altro", "pending", &[]).await;
        let store = PgTodoStore::new(pool.clone(), pool.clone());
        let r = store
            .build_reminder_text(&run_id.to_string())
            .await
            .expect("ok")
            .expect("reminder presente (3 pending/in_progress >= soglia 3)");
        assert!(r.contains("<todo_list>"));
        assert!(r.contains("[x]") && r.contains("[~]") && r.contains("[ ]"));
        // cursore sul todo attivo (in_progress, seq 2).
        assert!(r.contains("> 2. [~] in corso"));
    }
}
