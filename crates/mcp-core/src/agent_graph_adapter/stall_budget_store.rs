//! Adapter del trait [`nexus_agent_graph::runtime::ports::StallBudgetPort`].
//!
//! IMPLEMENTA il budget CROSS-RUN delle consultazioni del meta-reasoner di
//! recovery-da-stallo, per SESSIONE. Il cap `extra["stall_moves_used"]`
//! dell'executor e' checkpointato con lo stato e si AZZERA tra run diversi della
//! stessa sessione; il loop email (chat Beaty-Book) e' cross-run (9 run, 6
//! richieste identiche), quindi un cap solo per-run non lo ferma. Questa porta
//! persiste+conta le consultazioni della SESSIONE cosi' che il cap
//! `agent.stall_recovery.max_moves_per_session` (regola G) sia effettivamente
//! per-sessione.
//!
//! MECCANISMO (scelto: il MENO invasivo, documentato): NESSUNA DDL. Il conteggio
//! vive come righe `nexus_agent_meta_steps` con `kind='stall_budget'`,
//! append-and-count per sessione (join `agent_runs.session_id`), STESSO pattern
//! di [`super::clarify_history_store`]. La tabella e' gia' nel DB per-progetto
//! (separazione DB, regola L): NON serve migrazione. Ogni consultazione EFFETTIVA
//! (mossa applicata) appende una riga; la lettura conta le righe della sessione.
//!
//! POOL: la lettura/scrittura gira sul pool del DOMINIO RUN (`run_db`, separazione
//! DB per-progetto): `agent_runs` e `nexus_agent_meta_steps` sono tabelle migrate
//! al DB del progetto (regola L, [`crate::project_db_routes`]). Il call site
//! risolve il pool e lo passa qui col `run_id` corrente.
//!
//! FAIL-OPEN (sicurezza, come [`super::escalation_port`]/[`super::clarify_history_store`]):
//! [`StallBudgetPort::consultations_in_session`] ritorna `Ok(0)` su guasto DB
//! (budget non esaurito -> il meta-reasoner resta consultabile, degrado al solo cap
//! per-run), MAI un `PortError`. [`StallBudgetPort::record_consultation`] e'
//! best-effort. CONFINE (regola L): qui SOLO l'I/O; la DECISIONE (cap raggiunto?)
//! resta nel gate di emissione dell'executor.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{PortError, StallBudgetPort};

/// `kind` della riga `nexus_agent_meta_steps` usata come contatore append-only
/// delle consultazioni del meta-reasoner. Distinto dagli altri kind del canale
/// (plan/routing/clarify/...): non e' un meta-step di timeline verso l'utente, e'
/// un contatore tecnico interno. Costante interna (non config di comportamento):
/// regola G riguarda modelli/soglie, non l'identita' di una riga tecnica.
const STALL_BUDGET_KIND: &str = "stall_budget";

/// Adapter [`StallBudgetPort`] -> `nexus_agent_meta_steps` (kind='stall_budget')
/// join `agent_runs` (session_id), sul pool del dominio run.
pub struct PgStallBudgetStore {
    /// Pool Postgres del DOMINIO RUN (progetto): dove vivono `agent_runs` e
    /// `nexus_agent_meta_steps`. Risolto dal call site (separazione DB per-progetto).
    run_db: PgPool,
    /// Run corrente: `run_id` della riga append-only scritta da
    /// [`StallBudgetPort::record_consultation`] (colonna NOT NULL). La LETTURA e'
    /// per SESSIONE (join `agent_runs.session_id`), non per run.
    run_id: Uuid,
}

impl PgStallBudgetStore {
    /// Costruisce lo store sul pool del dominio run (progetto) e sul run corrente
    /// (per l'append). La lettura resta per-sessione.
    pub fn new(run_db: PgPool, run_id: Uuid) -> Self {
        Self { run_db, run_id }
    }
}

#[async_trait]
impl StallBudgetPort for PgStallBudgetStore {
    /// Conta le righe `kind='stall_budget'` dei run della sessione (join
    /// `agent_runs.session_id`). Ogni riga = UNA consultazione effettiva del
    /// meta-reasoner in un qualsiasi run della sessione (cross-run). FAIL-OPEN:
    /// guasto DB -> `Ok(0)` (mai bloccare), MAI un `PortError`. SOLA LETTURA.
    async fn consultations_in_session(&self, session_id: Uuid) -> Result<i64, PortError> {
        let res: Result<i64, sqlx::Error> = sqlx::query_scalar(
            "SELECT COUNT(*) \
             FROM nexus_agent_meta_steps ms \
             JOIN agent_runs ar ON ar.id = ms.run_id \
             WHERE ar.session_id = $1 \
               AND ms.kind = $2",
        )
        .bind(session_id)
        .bind(STALL_BUDGET_KIND)
        .fetch_one(&self.run_db)
        .await;
        match res {
            Ok(count) => Ok(count),
            Err(e) => {
                // Fail-open: un guasto di lettura non deve bloccare il gate.
                tracing::warn!(
                    target: "mcp_core::stall_budget_store",
                    session_id = %session_id,
                    error = %e,
                    "lettura budget stall cross-run fallita (fail-open, conteggio 0)"
                );
                Ok(0)
            }
        }
    }

    /// Appende UNA riga `kind='stall_budget'` per il run corrente (una
    /// consultazione effettiva). Best-effort: errore DB loggato, `Ok(())` ritornato
    /// (il `PortError` resta per un contratto rotto).
    async fn record_consultation(&self, session_id: Uuid) -> Result<(), PortError> {
        let res = sqlx::query(
            "INSERT INTO nexus_agent_meta_steps (run_id, kind, title, payload) \
             VALUES ($1, $2, '', $3)",
        )
        .bind(self.run_id)
        .bind(STALL_BUDGET_KIND)
        .bind(serde_json::json!({ "session_id": session_id }))
        .execute(&self.run_db)
        .await;
        if let Err(e) = res {
            tracing::warn!(
                target: "mcp_core::stall_budget_store",
                run_id = %self.run_id,
                session_id = %session_id,
                error = %e,
                "INSERT budget stall cross-run fallita (best-effort)"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sessione chat reale su cui appendere i run del test: `agent_runs.session_id`
    /// e' vincolato da una FK verso `chat_sessions(id)`.
    async fn seed_sessione(pool: &PgPool) -> Uuid {
        crate::test_support::seed_chat_session(pool, Uuid::new_v4()).await
    }

    /// Registra un run della sessione (per il join) e ne ritorna l'id. Le tabelle
    /// le porta il migrator del set `db/migrations/project`.
    async fn insert_run(pool: &PgPool, session_id: Uuid) -> Uuid {
        crate::test_support::insert_agent_run(pool, session_id, Uuid::new_v4(), "running").await
    }

    /// CROSS-RUN: consultazioni registrate in run DIVERSI della stessa sessione
    /// sommano; run di altre sessioni no.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn conta_cross_run_per_sessione(pool: PgPool) {
        let session = seed_sessione(&pool).await;
        let altra_sessione = seed_sessione(&pool).await;

        // Run 1 e Run 2 della stessa sessione: una consultazione ciascuno.
        let run1 = insert_run(&pool, session).await;
        let run2 = insert_run(&pool, session).await;
        PgStallBudgetStore::new(pool.clone(), run1)
            .record_consultation(session)
            .await
            .expect("ok");
        PgStallBudgetStore::new(pool.clone(), run2)
            .record_consultation(session)
            .await
            .expect("ok");

        // Run di un'ALTRA sessione: non deve contare per `session`.
        let run_alieno = insert_run(&pool, altra_sessione).await;
        PgStallBudgetStore::new(pool.clone(), run_alieno)
            .record_consultation(altra_sessione)
            .await
            .expect("ok");

        let store = PgStallBudgetStore::new(pool.clone(), run2);
        let count = store
            .consultations_in_session(session)
            .await
            .expect("fail-open");
        assert_eq!(count, 2, "2 consultazioni cross-run nella sessione");
        let count_altra = store
            .consultations_in_session(altra_sessione)
            .await
            .expect("fail-open");
        assert_eq!(count_altra, 1, "l'altra sessione conta la propria");
    }

    /// FAIL-OPEN: se la lettura fallisce -> conteggio 0, mai un errore. E' l'UNICO
    /// test del modulo che gira su un DB SENZA schema (nessun migrator): simula il
    /// guasto infrastrutturale che il fail-open deve assorbire.
    #[sqlx::test]
    async fn fail_open_su_query_fallita(pool: PgPool) {
        let store = PgStallBudgetStore::new(pool.clone(), Uuid::new_v4());
        let count = store
            .consultations_in_session(Uuid::new_v4())
            .await
            .expect("fail-open: mai PortError");
        assert_eq!(count, 0, "fail-open: conteggio 0, mai un panico/errore");
    }

    /// Nessuna consultazione registrata -> conteggio 0 (budget mai esaurito,
    /// comportamento invariato).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn nessuna_consultazione_conteggio_zero(pool: PgPool) {
        let session = seed_sessione(&pool).await;
        let store = PgStallBudgetStore::new(pool.clone(), Uuid::new_v4());
        let count = store
            .consultations_in_session(session)
            .await
            .expect("fail-open");
        assert_eq!(count, 0);
    }
}
