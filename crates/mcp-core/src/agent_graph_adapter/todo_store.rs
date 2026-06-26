//! Adapter del trait [`nexus_agent_graph::runtime::ports::TodoStore`].
//!
//! IMPLEMENTERA' (FASE 2) l'I/O sui todo del DAG su `nexus_agent_todos` via
//! `sqlx` (1:1 con `brain/agents/todo_store.py`). INVARIANTE (regola H): `list_todos`
//! restituisce `depends_on` come `Vec` (cast `::text[]`), MAI una stringa
//! `"{...}"`, e i todo gia' ordinati per `seq` ASC. Le scritture (`mark_status`,
//! `increment_iteration_seen`) sono gata `Real` (no-op in `ExecMode::Replay`,
//! punto unico del gate shadow). La LOGICA DAG resta pura in
//! `nexus_agent_graph::decisions::dag_scheduler` (questo adapter isola SOLO il DB).

use sqlx::PgPool;

/// Adapter [`TodoStore`] -> `nexus_agent_todos` via `sqlx`.
///
/// F2 implementera' il trait `TodoStore` su questa struct.
pub struct PgTodoStore {
    /// Pool Postgres su cui le query/UPDATE dei todo gireranno in F2.
    db: PgPool,
}

impl PgTodoStore {
    /// Costruisce lo store sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
