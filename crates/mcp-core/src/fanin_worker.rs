//! Worker TRIGGER del fan-in deterministico dei sub-run background (Fase D Slice 3).
//!
//! CONTESTO: quando il padre dispatcha figli con `background=true`, il motore lo
//! SOSPENDE (`awaiting_subagents`, Slice 1+2). Al completamento dell'ultimo figlio
//! background il finalize accoda il parent nella coda durevole META
//! `subagent_fanin_resume_queue` (mig 0542). Questo worker drena la coda e
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

/// PUNTO UNICO (regola L/M) della decisione post-resume: la riga di coda va
/// TENUTA (non cancellata) se e solo se il resume ha RI-SOSPESO il padre su una
/// nuova ondata di figli background (`AwaitingSubagents`). Decisione dal SEGNALE
/// STRUTTURATO `status` (regola M), mai dalla prosa.
///
/// - `Some(AwaitingSubagents)` -> tieni: il padre riprendera' al prossimo giro.
/// - `Some(altro terminale)` -> cancella: run finalizzato, lavoro consumato.
/// - `None` (resume fallito, run gia' marcato failed dal resume) -> cancella: non
///   riprendera' da questa coda.
fn keep_row_after_resume(resume_status: Option<&AgentRunStatus>) -> bool {
    matches!(resume_status, Some(AgentRunStatus::AwaitingSubagents))
}

pub fn spawn_fanin_worker(state: AppState) {
    tokio::spawn(async move {
        sleep(Duration::from_secs(STARTUP_DELAY_S)).await;
        // Timestamp dell'ultimo passaggio del backstop (scandito piu' di rado del
        // poll rapido della coda: il backstop scansiona TUTTI i progetti, costoso).
        let mut last_backstop = std::time::Instant::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(std::time::Instant::now);
        loop {
            let poll = load_u64(&state.db, "orchestrator.background_fanin_poll_seconds", 4, 2).await;
            if is_enabled(&state.db).await {
                if let Err(e) = run_one_round(&state).await {
                    tracing::warn!("fanin_worker: round fallito: {e}");
                }
                // BACKSTOP periodico (cadenza DB-driven): recupera i padri
                // awaiting_subagents mai accodati (figlio detached crashato/panicato/
                // timeout DB su mark_run, o restart di mcp-core tra il finalize del
                // figlio e l'enqueue). Scandisce i progetti, quindi va scandito piu'
                // di rado del poll rapido della coda.
                let backstop_every =
                    load_u64(&state.db, "orchestrator.background_fanin_backstop_seconds", 60, 10)
                        .await;
                if last_backstop.elapsed() >= Duration::from_secs(backstop_every) {
                    if let Err(e) = run_backstop(&state).await {
                        tracing::warn!("fanin_worker: backstop fallito: {e}");
                    }
                    last_backstop = std::time::Instant::now();
                }
            }
            sleep(Duration::from_secs(poll)).await;
        }
    });
    tracing::info!(
        "fanin_worker: avviato (trigger resume fan-in dei sub-run background, Fase D)"
    );
}

/// Kill-switch DB-driven (regola G): default ON (mig 0542). `false/0/no/off` OFF.
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
        // Isolamento del panic (regola F/H): ogni riga e' processata in un task
        // separato, cosi' un panic in `resume_fanin` (o in una sua dipendenza)
        // NON uccide il loop del worker — le altre righe e i giri successivi
        // proseguono. Un `JoinError` (panic o cancellazione) e' loggato ERROR e la
        // riga resta in coda (nessun delete): il prossimo giro la ri-processa.
        let state_cloned = state.clone();
        let parent_run_id = row.parent_run_id;
        let handle = tokio::spawn(async move {
            process_queue_row(&state_cloned, row).await;
        });
        if let Err(join_err) = handle.await {
            tracing::error!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                error = %join_err,
                "fan-in: task di processamento riga PANICATO/cancellato, riga lasciata in coda per il prossimo giro"
            );
        }
    }
    Ok(())
}

/// Un padre `awaiting_subagents` candidato al backstop, letto da un PROJECT pool.
/// `anchor` = COALESCE(parent_run_id, session_id): la chiave con cui i suoi figli
/// sono registrati in `nexus_subagent_runs` (vedi `parent_anchor` in
/// subagent_native).
#[derive(Debug)]
struct BackstopParent {
    run_id: Uuid,
    session_id: Uuid,
}

/// BACKSTOP (regola H, causa radice del "padre awaiting_subagents PER SEMPRE"): un
/// figlio background detached puo' NON accodare mai il parent — crash/panic prima
/// dell'enqueue, timeout DB su mark_run (riga resta `running` -> COUNT non scende a
/// 0), o restart di mcp-core tra il finalize del figlio e l'enqueue. Il worker
/// principale (trigger da coda) non se ne accorge: la coda e' vuota. Questo
/// backstop recupera quei padri scansionando i PROJECT pool.
///
/// SCELTA (cross-DB): agent_runs/nexus_subagent_runs vivono nei DB-PROGETTO, la
/// coda nel META. Per trovare i padri orfani serve enumerare i progetti — il
/// worker NON puo' gatare la scansione sulla sola coda (che e' proprio VUOTA nel
/// caso da recuperare). Usa il PUNTO UNICO `list_all_project_ids` (regola L, lo
/// stesso di run_reaper) e per ogni progetto: (1) marca terminali le sub-run
/// `running` orfane oltre timeout DB-driven; (2) trova i padri awaiting_subagents
/// i cui figli background sono TUTTI terminali; (3) li accoda nella coda META
/// (INSERT ON CONFLICT DO NOTHING). L'INSERT idempotente + il CAS del worker
/// garantiscono un solo resume anche se il figlio "vero" accoda in parallelo.
async fn run_backstop(state: &AppState) -> Result<(), String> {
    let orphan_timeout_s = load_u64(
        &state.db,
        "orchestrator.background_fanin_orphan_timeout_seconds",
        900,
        60,
    )
    .await;
    let mut requeued = 0u64;
    for project_id in crate::project_db_routes::list_all_project_ids(&state.db).await {
        let proj_pool =
            crate::project_db_routes::project_data_pool_from(&state.db, project_id).await;
        requeued += backstop_project(&state.db, &proj_pool, orphan_timeout_s as i64).await;
    }
    if requeued > 0 {
        tracing::warn!(
            target: "mcp_core::fanin_worker",
            requeued,
            "fan-in backstop: recuperati {requeued} padri awaiting_subagents mai accodati (figlio detached perso o restart)"
        );
    }
    Ok(())
}

/// Backstop di UN progetto: marca gli orfani, trova i padri recuperabili, accoda.
/// Ritorna il numero di padri accodati (righe INSERT effettive). `meta` = coda;
/// `proj` = pool del progetto (agent_runs + nexus_subagent_runs).
async fn backstop_project(meta: &PgPool, proj: &PgPool, orphan_timeout_s: i64) -> u64 {
    // (1) Marca 'timeout' le sub-run BACKGROUND rimaste `running` oltre soglia
    //     (figlio detached morto senza mark_run): senza, la COUNT del fan-in non
    //     scenderebbe mai a 0 e il padre resterebbe appeso. Solo i background
    //     (i sincroni non sospendono il padre) e solo oltre il timeout DB-driven.
    if let Err(e) = sqlx::query(
        "UPDATE nexus_subagent_runs SET status = 'timeout', completed_at = NOW() \
         WHERE is_background = true AND status IN ('running', 'paused') \
           AND created_at < NOW() - make_interval(secs => $1)",
    )
    .bind(orphan_timeout_s as f64)
    .execute(proj)
    .await
    {
        tracing::warn!(
            target: "mcp_core::fanin_worker",
            error = %e,
            "fan-in backstop: marcatura sub-run orfane fallita (ritento al prossimo giro)"
        );
        return 0;
    }

    // (2) Padri `awaiting_subagents` i cui figli background (parent_run_id = anchor,
    //     anchor = COALESCE(ar.parent_run_id, ar.session_id)) sono TUTTI terminali,
    //     con ALMENO un figlio background (altrimenti non e' un fan-in): candidati
    //     al re-enqueue. Segnale strutturato (status lifecycle, regola M).
    let candidates: Vec<BackstopParent> = match sqlx::query(
        "SELECT ar.id AS run_id, ar.session_id AS session_id \
         FROM agent_runs ar \
         WHERE ar.status = 'awaiting_subagents' \
           AND EXISTS ( \
                 SELECT 1 FROM nexus_subagent_runs s \
                 WHERE s.parent_run_id = COALESCE(ar.parent_run_id, ar.session_id) \
                   AND s.is_background = true) \
           AND NOT EXISTS ( \
                 SELECT 1 FROM nexus_subagent_runs s \
                 WHERE s.parent_run_id = COALESCE(ar.parent_run_id, ar.session_id) \
                   AND s.is_background = true \
                   AND s.status IN ('running', 'paused'))",
    )
    .fetch_all(proj)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|r| BackstopParent {
                run_id: r.get("run_id"),
                session_id: r.get("session_id"),
            })
            .collect(),
        Err(e) => {
            tracing::warn!(
                target: "mcp_core::fanin_worker",
                error = %e,
                "fan-in backstop: query padri orfani fallita (ritento al prossimo giro)"
            );
            return 0;
        }
    };

    // (3) Accoda nella coda META (idempotente). Cross-DB: agent_runs/nexus_subagent_
    //     runs vivono sul PROJECT pool, la coda sul META -> nessun INSERT ... SELECT
    //     unico possibile. Per ogni candidato: leggi project_id dal PROJECT pool,
    //     INSERT ON CONFLICT DO NOTHING sul META. L'idempotenza (PK parent_run_id)
    //     + il CAS del worker garantiscono un solo resume anche se il figlio "vero"
    //     accoda in parallelo lo stesso padre.
    let mut inserted = 0u64;
    for p in candidates {
        inserted += backstop_enqueue_one(meta, proj, p.run_id, p.session_id).await;
    }
    inserted
}

/// Accoda UN padre nella coda META (idempotente). `project_id` risolto dal PROJECT
/// pool (agent_runs.project_id) per non dipendere dal valore iterato. Ritorna 1 se
/// ha inserito una riga nuova, 0 altrimenti (gia' presente o errore).
async fn backstop_enqueue_one(
    meta: &PgPool,
    proj: &PgPool,
    run_id: Uuid,
    session_id: Uuid,
) -> u64 {
    // project_id dal PROJECT pool (fonte di verita' della riga run).
    let project_id: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM agent_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(proj)
            .await
            .ok()
            .flatten();
    let Some(project_id) = project_id else {
        return 0;
    };
    match sqlx::query(
        "INSERT INTO subagent_fanin_resume_queue (parent_run_id, project_id, session_id) \
         VALUES ($1, $2, $3) ON CONFLICT (parent_run_id) DO NOTHING",
    )
    .bind(run_id)
    .bind(project_id)
    .bind(session_id)
    .execute(meta)
    .await
    {
        Ok(res) => res.rows_affected(),
        Err(e) => {
            tracing::warn!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %run_id,
                error = %e,
                "fan-in backstop: enqueue del padre orfano fallito (ritento al prossimo giro)"
            );
            0
        }
    }
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
        // Il CAS ha portato il parent a `running`: esegue il resume fan-in. Il
        // resume ritorna lo status FINALE strutturato (regola M). La decisione
        // "cancellare o tenere la riga" e' nel punto unico puro
        // `keep_row_after_resume`: se lo status e' `AwaitingSubagents` il padre si
        // e' RI-SOSPESO (2a ondata di figli background) -> la riga NON va cancellata
        // (i figli della 2a ondata fanno un nuovo enqueue con ON CONFLICT DO NOTHING
        // = no-op sulla riga esistente; se cancellassimo qui, il loro enqueue
        // avrebbe gia' fatto no-op e il risveglio andrebbe perso -> padre appeso).
        // La riga resta e il prossimo giro la ri-processa quando il CAS ritrovera'
        // `awaiting_subagents`.
        let resume = crate::chat_messages::resume_fanin(state, parent_run_id, project_id, session_id).await;
        match &resume {
            Ok(status) => tracing::info!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                status = status.as_str(),
                re_suspended = *status == AgentRunStatus::AwaitingSubagents,
                "fan-in: run padre ripreso"
            ),
            Err(e) => tracing::warn!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                error = %e,
                "fan-in: resume del run padre fallito (run gia' marcato failed dal resume)"
            ),
        }
        if keep_row_after_resume(resume.as_ref().ok()) {
            // Padre ri-sospeso: riga riusata per il prossimo giro fan-in, niente DELETE.
            return;
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

    /// BUG #2 (ALTA): il worker cancellava la riga INCONDIZIONATAMENTE dopo il
    /// resume. Se il resume RI-SOSPENDE il padre (2a ondata background), la riga
    /// deve RESTARE (i figli della 2a ondata fanno enqueue ON CONFLICT DO NOTHING =
    /// no-op sulla riga vecchia). Il punto unico `keep_row_after_resume` decide dal
    /// segnale strutturato: tieni solo se `AwaitingSubagents`.
    #[test]
    fn keep_row_solo_se_ri_sospeso() {
        assert!(
            keep_row_after_resume(Some(&AgentRunStatus::AwaitingSubagents)),
            "resume che ri-sospende -> riga TENUTA (no delete)"
        );
        // Ogni esito terminale -> riga cancellata (lavoro consumato).
        for s in [
            AgentRunStatus::Completed,
            AgentRunStatus::CompletedVerified,
            AgentRunStatus::CompletedUnverified,
            AgentRunStatus::Failed,
            AgentRunStatus::FailedDiagnosed,
            AgentRunStatus::Cancelled,
        ] {
            assert!(
                !keep_row_after_resume(Some(&s)),
                "resume terminale {} -> riga cancellata",
                s.as_str()
            );
        }
        // Resume fallito (None) -> cancella (non riprende da questa coda).
        assert!(!keep_row_after_resume(None), "resume fallito -> cancella");
    }

    /// Crea le tabelle minime per i test del backstop: `agent_runs` +
    /// `nexus_subagent_runs` + la coda. Un solo pool fa da project e da meta (in
    /// prod i primi due sono sul PROJECT pool, la coda sul META): il backstop
    /// riceve i due handle separati, qui coincidono.
    async fn create_backstop_tables(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID PRIMARY KEY, \
                 session_id UUID NOT NULL, \
                 project_id UUID NOT NULL, \
                 parent_run_id UUID, \
                 status TEXT NOT NULL )",
        )
        .execute(pool)
        .await
        .expect("create agent_runs");
        sqlx::query(
            "CREATE TABLE nexus_subagent_runs ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 parent_run_id UUID NOT NULL, \
                 is_background BOOLEAN NOT NULL DEFAULT false, \
                 status TEXT NOT NULL, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 completed_at TIMESTAMPTZ )",
        )
        .execute(pool)
        .await
        .expect("create nexus_subagent_runs");
        sqlx::query(
            "CREATE TABLE subagent_fanin_resume_queue ( \
                 parent_run_id UUID PRIMARY KEY, \
                 project_id UUID NOT NULL, \
                 session_id UUID NOT NULL, \
                 enqueued_at TIMESTAMPTZ NOT NULL DEFAULT NOW() )",
        )
        .execute(pool)
        .await
        .expect("create queue");
    }

    /// BUG #3 (ALTA): un padre `awaiting_subagents` con TUTTI i figli background
    /// terminali ma NESSUNA riga in coda (figlio detached crashato prima
    /// dell'enqueue, o restart) resterebbe appeso per sempre. Il backstop lo
    /// recupera: lo trova (figli tutti terminali) e lo ACCODA. Senza il backstop
    /// la coda resterebbe vuota (il test fallirebbe: 0 righe accodate).
    #[sqlx::test]
    async fn backstop_accoda_padre_orfano(pool: sqlx::PgPool) {
        create_backstop_tables(&pool).await;
        let parent = Uuid::new_v4();
        let session = Uuid::new_v4();
        let project = Uuid::new_v4();
        // Padre di primo livello: anchor = session_id (parent_run_id NULL).
        sqlx::query(
            "INSERT INTO agent_runs (id, session_id, project_id, parent_run_id, status) \
             VALUES ($1, $2, $3, NULL, 'awaiting_subagents')",
        )
        .bind(parent)
        .bind(session)
        .bind(project)
        .execute(&pool)
        .await
        .expect("insert padre");
        // Due figli background, entrambi TERMINALI, ancorati alla session (anchor).
        // La coda e' VUOTA (l'enqueue non e' mai avvenuto).
        for st in ["completed", "timeout"] {
            sqlx::query(
                "INSERT INTO nexus_subagent_runs (parent_run_id, is_background, status) \
                 VALUES ($1, true, $2)",
            )
            .bind(session)
            .bind(st)
            .execute(&pool)
            .await
            .expect("insert figlio");
        }

        // Precondizione: coda vuota.
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subagent_fanin_resume_queue")
            .fetch_one(&pool)
            .await
            .expect("count before");
        assert_eq!(before, 0, "precondizione: coda vuota");

        // Backstop: timeout orfano irrilevante qui (figli gia' terminali).
        let requeued = backstop_project(&pool, &pool, 900).await;
        assert_eq!(requeued, 1, "il backstop deve accodare il padre orfano");

        // Il padre accodato e' il RUN corrente (agent_runs.id), non l'anchor.
        let queued: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_run_id FROM subagent_fanin_resume_queue")
                .fetch_optional(&pool)
                .await
                .expect("select queued");
        assert_eq!(queued, Some(parent), "in coda ci deve essere il run padre");

        // Idempotente: un secondo passaggio non duplica (la riga esiste gia').
        let requeued2 = backstop_project(&pool, &pool, 900).await;
        assert_eq!(requeued2, 0, "backstop idempotente: nessun duplicato");
    }

    /// BUG #3 (parte 2): una sub-run background rimasta `running` orfana (figlio
    /// detached morto senza mark_run) oltre il timeout viene marcata `timeout` dal
    /// backstop, cosi' la COUNT scende a 0 e il padre viene accodato. Con timeout
    /// alto (figlio "giovane") il backstop NON tocca nulla (il figlio potrebbe
    /// ancora vivere).
    #[sqlx::test]
    async fn backstop_marca_orfani_solo_oltre_timeout(pool: sqlx::PgPool) {
        create_backstop_tables(&pool).await;
        let parent = Uuid::new_v4();
        let session = Uuid::new_v4();
        let project = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_runs (id, session_id, project_id, parent_run_id, status) \
             VALUES ($1, $2, $3, NULL, 'awaiting_subagents')",
        )
        .bind(parent)
        .bind(session)
        .bind(project)
        .execute(&pool)
        .await
        .expect("insert padre");
        // Figlio background RUNNING vecchio (created_at 30 min fa): orfano.
        sqlx::query(
            "INSERT INTO nexus_subagent_runs (parent_run_id, is_background, status, created_at) \
             VALUES ($1, true, 'running', NOW() - interval '30 minutes')",
        )
        .bind(session)
        .execute(&pool)
        .await
        .expect("insert figlio orfano");

        // Timeout ALTO (1h): il figlio (30 min) e' sotto soglia -> NON marcato, NON
        // accodato (potrebbe ancora vivere).
        let requeued_young = backstop_project(&pool, &pool, 3600).await;
        assert_eq!(requeued_young, 0, "figlio sotto timeout -> non accodato");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM subagent_fanin_resume_queue")
                .fetch_one(&pool)
                .await
                .expect("count"),
            0
        );

        // Timeout BASSO (60s): il figlio (30 min) e' oltre soglia -> marcato
        // `timeout`, la COUNT scende a 0, il padre viene accodato.
        let requeued_old = backstop_project(&pool, &pool, 60).await;
        assert_eq!(requeued_old, 1, "figlio orfano oltre timeout -> padre accodato");
        let status: String = sqlx::query_scalar(
            "SELECT status FROM nexus_subagent_runs WHERE parent_run_id = $1",
        )
        .bind(session)
        .fetch_one(&pool)
        .await
        .expect("status figlio");
        assert_eq!(status, "timeout", "la sub-run orfana e' marcata timeout");
    }
}
