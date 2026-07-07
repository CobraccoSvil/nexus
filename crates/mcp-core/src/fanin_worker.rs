//! Worker TRIGGER del fan-in deterministico dei sub-run background (Fase D Slice 3).
//!
//! CONTESTO: quando il padre dispatcha figli con `background=true`, il motore lo
//! SOSPENDE (`awaiting_subagents`, Slice 1+2). Al completamento dell'ultimo figlio
//! background il finalize accoda il parent nella coda durevole META
//! `subagent_fanin_resume_queue` (mig 0541). Questo worker drena la coda e
//! RIPRENDE il parent.
//!
//! Perche' una coda durevole e un CAS invece di un hook in-process:
//! - RACE-FREE (lost-wakeup): se il figlio completa PRIMA che il padre si sospenda
//!   (l'insert nella coda avviene ma il CAS `awaiting_subagents -> running` non
//!   trova ancora quello stato), il worker LASCIA la riga in coda e ritenta al
//!   giro successivo, quando il padre si sara' sospeso. Nessun risveglio perso.
//! - RESTART-SAFE: la coda vive in DB; un restart di mcp-core non perde i risvegli
//!   pendenti (a differenza di un canale in memoria).
//! - IDEMPOTENTE: il CAS (`... WHERE status='awaiting_subagents' RETURNING id`)
//!   fa vincere UN solo worker/giro; l'INSERT in coda e' idempotente (PK
//!   parent_run_id). Un solo `resume_fanin` per parent.
//!
//! INERTE di default: il background e' opt-in (nessuna coda si popola finche' un
//! padre non dispatcha figli background), quindi il flag ON non fa nulla senza
//! lavoro. `orchestrator.background_fanin_enabled='false'` (regola G) spegne del
//! tutto il consumo (le righe restano, ripartono a ON).

use std::time::Duration;

use sqlx::{PgPool, Row};
use tokio::time::sleep;
use uuid::Uuid;

use crate::agent_types::AgentRunStatus;
use crate::AppState;

/// Attesa iniziale: lascia stabilizzare l'avvio prima del primo round (stesso
/// stile di `process_resume`, cosi' i risvegli non partono durante il boot).
const STARTUP_DELAY_S: u64 = 30;

/// Esito del tentativo di consumo di UNA riga della coda: guida la decisione se
/// eliminare la riga o lasciarla per un ritentativo (punto unico testabile).
#[derive(Debug, PartialEq, Eq)]
enum QueueAction {
    /// CAS vinto: il parent era `awaiting_subagents` -> resume + DELETE riga.
    Resume,
    /// Parent gia' TERMINALE (o inesistente): riga STALE -> DELETE senza resume.
    DeleteStale,
    /// RACE (parent ancora `running` senza awaiting, o stato non-terminale
    /// non-awaiting): LASCIA la riga in coda, ritenta al prossimo giro.
    Retry,
}

/// Decide l'azione dallo status del parent DOPO un CAS non vincente (regola M:
/// classificazione da segnale strutturato `status`, mai prosa). Punto unico
/// testabile della logica race/stale del worker.
///
/// - `None` (run inesistente) -> DeleteStale: il parent e' sparito, la riga e'
///   stale.
/// - status TERMINALE (`completed*`/`failed*`/`blocked*`/`cancelled`/...) ->
///   DeleteStale: il padre e' gia' finito per altra via, il risveglio non serve.
/// - status NON-terminale e NON `awaiting_subagents` (tipicamente `running`) ->
///   Retry: RACE lost-wakeup, il figlio ha accodato prima che il padre si
///   sospendesse; ritenta quando lo stato sara' `awaiting_subagents`.
fn action_from_parent_status(status: Option<&str>) -> QueueAction {
    match status {
        None => QueueAction::DeleteStale,
        Some(s) => {
            let st = AgentRunStatus::from_db_str(s);
            if st.is_terminal() {
                QueueAction::DeleteStale
            } else {
                // Non-terminale e non-awaiting (il CAS su awaiting ha gia' fallito
                // in questo ramo): RACE, ritenta.
                QueueAction::Retry
            }
        }
    }
}

pub fn spawn_fanin_worker(state: AppState) {
    tokio::spawn(async move {
        sleep(Duration::from_secs(STARTUP_DELAY_S)).await;
        loop {
            let poll = load_u64(&state.db, "orchestrator.background_fanin_poll_seconds", 4, 2).await;
            if is_enabled(&state.db).await {
                if let Err(e) = run_one_round(&state).await {
                    tracing::warn!("fanin_worker: round fallito: {e}");
                }
            }
            sleep(Duration::from_secs(poll)).await;
        }
    });
    tracing::info!(
        "fanin_worker: avviato (trigger resume fan-in dei sub-run background, Fase D)"
    );
}

/// Kill-switch DB-driven (regola G): default ON (mig 0541). `false/0/no/off` OFF.
async fn is_enabled(db: &PgPool) -> bool {
    crate::settings::get_setting(db, "orchestrator.background_fanin_enabled")
        .await
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

async fn load_u64(db: &PgPool, key: &str, default: u64, min: u64) -> u64 {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .max(min)
}

/// Riga della coda fan-in (META).
struct QueueRow {
    parent_run_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
}

async fn run_one_round(state: &AppState) -> Result<(), String> {
    // Coda nel META (state.db): cross-progetto, letta una volta per round. LIMIT
    // per non monopolizzare un giro se la coda cresce (i restanti al prossimo).
    let rows = sqlx::query(
        "SELECT parent_run_id, project_id, session_id \
         FROM subagent_fanin_resume_queue \
         ORDER BY enqueued_at ASC LIMIT 10",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|r| QueueRow {
        parent_run_id: r.get("parent_run_id"),
        project_id: r.get("project_id"),
        session_id: r.get("session_id"),
    })
    .collect::<Vec<_>>();

    for row in rows {
        process_queue_row(state, row).await;
    }
    Ok(())
}

/// Processa UNA riga della coda: CAS sul PROJECT pool, poi resume/DELETE/retry.
async fn process_queue_row(state: &AppState, row: QueueRow) {
    let QueueRow {
        parent_run_id,
        project_id,
        session_id,
    } = row;

    // Pool del progetto (separazione DB): agent_runs e' migrata per-progetto.
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await;

    // CAS: transizione atomica awaiting_subagents -> running. RETURNING id ->
    // vince UN solo consumer/giro (idempotenza del resume, regola H: e' il CAS a
    // chiudere la race, non uno sleep). updated_at aggiornato per l'osservabilita'.
    let cas = sqlx::query(
        "UPDATE agent_runs SET status = 'running', updated_at = NOW() \
         WHERE id = $1 AND status = 'awaiting_subagents' RETURNING id",
    )
    .bind(parent_run_id)
    .fetch_optional(&proj_pool)
    .await;

    let won = match cas {
        Ok(opt) => opt.is_some(),
        Err(e) => {
            // Errore infra sul CAS: non tocco la coda, ritento al prossimo giro.
            tracing::warn!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                error = %e,
                "fan-in: CAS awaiting_subagents->running fallito, ritento al prossimo giro"
            );
            return;
        }
    };

    if won {
        // Il CAS ha portato il parent a `running`: esegue il resume fan-in. A
        // resume concluso (Ok o Err, il run e' comunque finalizzato) elimina la
        // riga: il lavoro e' consumato.
        match crate::chat_messages::resume_fanin(state, parent_run_id, project_id, session_id).await
        {
            Ok(status) => tracing::info!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                status = status.as_str(),
                "fan-in: run padre ripreso e finalizzato"
            ),
            Err(e) => tracing::warn!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                error = %e,
                "fan-in: resume del run padre fallito (run gia' marcato failed dal resume)"
            ),
        }
        delete_queue_row(&state.db, parent_run_id).await;
        return;
    }

    // CAS non vinto: leggo lo status per decidere stale (DELETE) vs race (retry).
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
        .bind(parent_run_id)
        .fetch_optional(&proj_pool)
        .await
        .ok()
        .flatten();
    match action_from_parent_status(status.as_deref()) {
        QueueAction::DeleteStale => {
            tracing::debug!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                status = status.as_deref().unwrap_or("<assente>"),
                "fan-in: parent gia' terminale/assente, riga di coda rimossa (stale)"
            );
            delete_queue_row(&state.db, parent_run_id).await;
        }
        QueueAction::Retry => {
            // RACE lost-wakeup: il figlio ha accodato prima che il padre si
            // sospendesse. LASCIO la riga: al prossimo giro il padre sara'
            // `awaiting_subagents` e il CAS vincera'. Questo chiude la race alla
            // radice (regola H), niente sleep magici.
            tracing::debug!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                status = status.as_deref().unwrap_or("<assente>"),
                "fan-in: parent non ancora sospeso (race), riga lasciata in coda per il prossimo giro"
            );
        }
        // Resume non e' un esito di questo ramo (il CAS non ha vinto).
        QueueAction::Resume => unreachable!("Resume deriva solo dal CAS vinto"),
    }
}

async fn delete_queue_row(meta: &PgPool, parent_run_id: Uuid) {
    if let Err(e) = sqlx::query("DELETE FROM subagent_fanin_resume_queue WHERE parent_run_id = $1")
        .bind(parent_run_id)
        .execute(meta)
        .await
    {
        tracing::warn!(
            target: "mcp_core::fanin_worker",
            parent_run_id = %parent_run_id,
            error = %e,
            "fan-in: DELETE riga di coda fallito (verra' ri-valutata al prossimo giro)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_none_e_stale() {
        // Run inesistente -> riga stale, DELETE.
        assert_eq!(action_from_parent_status(None), QueueAction::DeleteStale);
    }

    #[test]
    fn action_terminale_e_stale() {
        // Il padre e' gia' finito per altra via: risveglio inutile -> DELETE.
        for s in [
            "completed",
            "completed_verified",
            "completed_unverified",
            "failed",
            "failed_diagnosed",
            "blocked_needs_input",
            "cancelled",
            "loop_aborted",
        ] {
            assert_eq!(
                action_from_parent_status(Some(s)),
                QueueAction::DeleteStale,
                "status terminale {s} deve dare DeleteStale"
            );
        }
    }

    #[test]
    fn action_running_e_race_retry() {
        // RACE lost-wakeup: il figlio ha accodato prima che il padre si sospenda.
        // Il padre e' ancora `running` -> LASCIA in coda (Retry).
        assert_eq!(
            action_from_parent_status(Some("running")),
            QueueAction::Retry
        );
    }

    #[test]
    fn action_awaiting_subagents_e_retry() {
        // Se il CAS ha fallito ma lo status e' ancora awaiting_subagents (raro:
        // un altro consumer l'ha appena preso), NON terminale -> Retry (non lo
        // cancello: sarebbe una perdita di risveglio se il CAS altrui fallisse).
        assert_eq!(
            action_from_parent_status(Some("awaiting_subagents")),
            QueueAction::Retry
        );
    }

    #[test]
    fn action_ignoto_conservativo_retry() {
        // Status ignoto -> from_db_str ricade su Running (non terminale) -> Retry
        // conservativo: mai cancellare un risveglio per uno stato non riconosciuto.
        assert_eq!(
            action_from_parent_status(Some("stato_strano_xyz")),
            QueueAction::Retry
        );
    }
}
