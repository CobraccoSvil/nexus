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

pub fn spawn_fanin_worker(state: AppState) {
    tokio::spawn(async move {
        sleep(Duration::from_secs(STARTUP_DELAY_S)).await;
        // Timestamp dell'ultimo passaggio del backstop (scandito piu' di rado del
        // poll rapido della coda: il backstop scansiona TUTTI i progetti, costoso).
        let mut last_backstop = std::time::Instant::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(std::time::Instant::now);
        loop {
            let poll = load_u64(
                &state.db,
                "orchestrator.background_fanin_poll_seconds",
                4,
                2,
            )
            .await;
            if background_fanin_enabled(&state.db).await {
                if let Err(e) = run_one_round(&state).await {
                    tracing::warn!("fanin_worker: round fallito: {e}");
                }
                // BACKSTOP periodico (cadenza DB-driven): recupera i padri
                // awaiting_subagents mai accodati (figlio detached crashato/panicato/
                // timeout DB su mark_run, o restart di mcp-core tra il finalize del
                // figlio e l'enqueue). Scandisce i progetti, quindi va scandito piu'
                // di rado del poll rapido della coda.
                let backstop_every = load_u64(
                    &state.db,
                    "orchestrator.background_fanin_backstop_seconds",
                    60,
                    10,
                )
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
    tracing::info!("fanin_worker: avviato (trigger resume fan-in dei sub-run background, Fase D)");
}

/// Kill-switch DB-driven (regola G): default ON (mig 0542). `false/0/no/off` OFF.
/// PUNTO UNICO (regola L) del flag `orchestrator.background_fanin_enabled`: lo
/// consultano SIA il worker (per drenare la coda) SIA il gate di dispatch
/// (`background_active` in subagent_native) — se OFF il param `background` del tool
/// viene ignorato e il dispatch resta sincrono, così spegnere il flag riporta tutto
/// a sincrono a runtime (60s, senza redeploy) senza lasciare padri appesi.
pub(crate) async fn background_fanin_enabled(db: &PgPool) -> bool {
    bool_setting(db, "orchestrator.background_fanin_enabled", true).await
}

/// Lettura di un flag booleano DB-driven (regola G) con default. PUNTO UNICO
/// (regola L) del parsing bool dei setting del worker: `background_fanin_enabled`
/// e la guardia no-progress lo condividono invece di re-implementare
/// `matches!(..., "0"|"false"|"no"|"off")`. Setting assente/DB down -> `default`.
async fn bool_setting(db: &PgPool, key: &str, default: bool) -> bool {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default)
}

/// Parametri della guardia NO-PROGRESS (mig 0543) passati alla scansione
/// per-progetto del backstop. `Copy` (due campi scalari) per evitare cloni.
#[derive(Debug, Clone, Copy)]
struct NoProgressGuard {
    /// Kill-switch (`orchestrator.subagent_no_progress_check_enabled`): se `false`
    /// il check non viene applicato (resta solo l'orphan_timeout storico).
    enabled: bool,
    /// Eta minima (s) di una sub-run background senza progresso oltre cui e'
    /// marcata `timeout` (`orchestrator.subagent_no_progress_timeout_seconds`).
    timeout_s: i64,
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
/// I suoi figli background DIRETTI sono quelli con `dispatcher_run_id = run_id`
/// (mig project 0010): la correlazione per dispatcher isola i figli diretti dai
/// nipoti annidati (ALTA 1).
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
    // GUARDIA NO-PROGRESS (mig 0543): timeout piu' aggressivo dell'orphan_timeout,
    // applicato SOLO ai figli background SENZA progresso (0 agent_steps E 0
    // iterazioni). Kill-switch DB-driven (regola G) letto col PUNTO UNICO
    // `bool_setting` (stesso helper di `background_fanin_enabled`). Passati a
    // `backstop_project` cosi' la scansione per-progetto applica entrambi i check.
    let no_progress_enabled = bool_setting(
        &state.db,
        "orchestrator.subagent_no_progress_check_enabled",
        true,
    )
    .await;
    let no_progress_timeout_s = load_u64(
        &state.db,
        "orchestrator.subagent_no_progress_timeout_seconds",
        300,
        30,
    )
    .await;
    let no_progress = NoProgressGuard {
        enabled: no_progress_enabled,
        timeout_s: no_progress_timeout_s as i64,
    };
    let mut requeued = 0u64;
    for project_id in crate::project_db_routes::list_all_project_ids(&state.db).await {
        let proj_pool =
            match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
                Ok(pool) => pool,
                Err(e) => {
                    tracing::warn!(project_id = %project_id, error = %e, "fan-in backstop: DB progetto non disponibile, progetto saltato per questo giro");
                    continue;
                }
            };
        requeued +=
            backstop_project(&state.db, &proj_pool, orphan_timeout_s as i64, no_progress).await;
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
async fn backstop_project(
    meta: &PgPool,
    proj: &PgPool,
    orphan_timeout_s: i64,
    no_progress: NoProgressGuard,
) -> u64 {
    // (1a) GUARDIA NO-PROGRESS (mig 0543): marca 'timeout' le sub-run BACKGROUND
    //      impantanate — `running`/`paused` create oltre la soglia (piu' aggressiva
    //      dell'orphan_timeout) E SENZA alcun progresso. Il PROGRESSO e' un segnale
    //      STRUTTURATO (regola M), non prosa: (a) 0 righe in `agent_steps` per quel
    //      run (il figlio ha lo stesso id in agent_runs -> gli step si correlano su
    //      run_id, persistiti INCREMENTALMENTE dal grafo, a differenza di
    //      `iterations`/`tokens_completion` scritti solo al mark_run finale) E
    //      (b) `iterations = 0` (difesa in profondita' sulla stessa riga). Cosi' un
    //      figlio che LAVORA (ha eseguito almeno un tool) non viene mai ucciso,
    //      mentre quello a 0 token/0 iter/0 step per >timeout_s viene abortito e la
    //      COUNT del fan-in scende, liberando il padre. Gated dal kill-switch.
    if no_progress.enabled {
        if let Err(e) = sqlx::query(
            "UPDATE nexus_subagent_runs s SET status = 'timeout', completed_at = NOW() \
             WHERE s.is_background = true AND s.status IN ('running', 'paused') \
               AND s.created_at < NOW() - make_interval(secs => $1) \
               AND COALESCE(s.iterations, 0) = 0 \
               AND NOT EXISTS ( \
                     SELECT 1 FROM agent_steps st WHERE st.run_id = s.id)",
        )
        .bind(no_progress.timeout_s as f64)
        .execute(proj)
        .await
        {
            tracing::warn!(
                target: "mcp_core::fanin_worker",
                error = %e,
                "fan-in backstop: marcatura sub-run no-progress fallita (ritento al prossimo giro)"
            );
            return 0;
        }
    }

    // (1b) Marca 'timeout' le sub-run BACKGROUND rimaste `running` oltre soglia
    //     (figlio detached morto senza mark_run): senza, la COUNT del fan-in non
    //     scenderebbe mai a 0 e il padre resterebbe appeso. Solo i background
    //     (i sincroni non sospendono il padre) e solo oltre il timeout DB-driven.
    //     Complementare al check no-progress: questo colpisce QUALSIASI sub-run
    //     vecchia (anche una che aveva fatto progressi ma e' poi morta), con
    //     timeout piu' lungo.
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

    // (2) Padri `awaiting_subagents` i cui figli background DIRETTI
    //     (dispatcher_run_id = ar.id, cioe' il run che li ha dispatchati; mig
    //     project 0010) sono TUTTI terminali, con ALMENO un figlio background
    //     (altrimenti non e' un fan-in): candidati al re-enqueue. Correlare per
    //     dispatcher_run_id (NON COALESCE(parent_run_id, session_id) = anchor)
    //     isola i figli diretti dai NIPOTI annidati (ALTA 1): senza, un nipote
    //     ancora `running` di un altro figlio bloccherebbe il re-enqueue del padre.
    //     Segnale strutturato (status lifecycle, regola M).
    let candidates: Vec<BackstopParent> = match sqlx::query(
        "SELECT ar.id AS run_id, ar.session_id AS session_id \
         FROM agent_runs ar \
         WHERE ar.status = 'awaiting_subagents' \
           AND EXISTS ( \
                 SELECT 1 FROM nexus_subagent_runs s \
                 WHERE s.dispatcher_run_id = ar.id \
                   AND s.is_background = true) \
           AND NOT EXISTS ( \
                 SELECT 1 FROM nexus_subagent_runs s \
                 WHERE s.dispatcher_run_id = ar.id \
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
async fn backstop_enqueue_one(meta: &PgPool, proj: &PgPool, run_id: Uuid, session_id: Uuid) -> u64 {
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
    // Se il DB del progetto non e' disponibile la riga resta in coda e viene
    // ritentata al prossimo giro (niente fallback al meta-DB).
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
            Ok(pool) => pool,
            Err(e) => {
                tracing::warn!(
                    target: "mcp_core::fanin_worker",
                    parent_run_id = %parent_run_id,
                    project_id = %project_id,
                    error = %e,
                    "fan-in: DB progetto non disponibile, riga saltata per questo giro"
                );
                return;
            }
        };

    // CAS: transizione atomica awaiting_subagents -> running. RETURNING id ->
    // vince UN solo consumer/giro (idempotenza del resume, regola H: e' il CAS a
    // chiudere la race, non uno sleep). updated_at aggiornato per l'osservabilita'.
    let cas = sqlx::query(
        // `completed_at = NULL` (regola H, gemello di chat_agent.rs confirm-CAS): il
        // persist di `awaiting_subagents` scrive `completed_at = NOW()` (stato di
        // riposo), ma il resume ri-porta il run a `running`. Se NON azzerassimo
        // `completed_at`, un `resume_fanin` che fallisce sul lookup infra DOPO questo
        // CAS lascerebbe il run `running` con `completed_at` valorizzato -> invisibile
        // sia al backstop fan-in (cerca `awaiting_subagents`) sia a `reap_stale_runs`
        // (filtra `completed_at IS NULL`) -> hang fino a restart. Azzerandolo, il
        // reaper time-gated recupera il run orfano. Sul resume riuscito il finalize
        // riscrive `completed_at = NOW()`.
        "UPDATE agent_runs SET status = 'running', completed_at = NULL, updated_at = NOW() \
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
        // Il CAS ha portato il parent a `running`. DELETE della riga SUBITO, PRIMA
        // del resume (ALTA 2, regola H race-free): se il resume RI-SOSPENDE il padre
        // su una 2a ondata di figli background, quei figli — che si spawnano DOPO,
        // durante/dopo il resume — faranno un enqueue FRESCO (riga nuova) quando il
        // loro ULTIMO terminera'. Tenere la riga vecchia (vecchio keep_row_after_
        // resume) faceva ri-vincere il CAS al giro dopo con la 2a ondata ANCORA
        // `running` -> resume con risultati PARZIALI. Con delete-al-CAS la riga
        // vecchia sparisce e solo il completamento della 2a ondata crea la riga che
        // riprende il padre: nessun risveglio prematuro, nessun lost-wakeup (la 2a
        // ondata enqueua sempre una riga nuova, mai un no-op su una riga stantia).
        delete_queue_row(&state.db, parent_run_id).await;

        // Resume: ritorna lo status FINALE strutturato (regola M). Se ri-sospende
        // (`AwaitingSubagents`) NON tocchiamo la coda: i figli della 2a ondata
        // accoderanno una riga fresca al loro completamento. Su errore il resume ha
        // gia' marcato il run `failed`: la riga era gia' cancellata, coerente.
        let resume =
            crate::chat_messages::resume_fanin(state, parent_run_id, project_id, session_id).await;
        match &resume {
            Ok(status) => tracing::info!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                status = status.as_str(),
                re_suspended = *status == AgentRunStatus::AwaitingSubagents,
                "fan-in: run padre ripreso (riga di coda gia' cancellata al CAS)"
            ),
            Err(e) => tracing::warn!(
                target: "mcp_core::fanin_worker",
                parent_run_id = %parent_run_id,
                error = %e,
                "fan-in: resume del run padre fallito (run gia' marcato failed dal resume)"
            ),
        }
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
            // `iterations` (mig project 0002) e' il segnale strutturato di progresso
            // letto dal check no-progress; `agent_steps` (sotto) e' l'altro segnale.
            "CREATE TABLE nexus_subagent_runs ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 parent_run_id UUID NOT NULL, \
                 dispatcher_run_id UUID, \
                 is_background BOOLEAN NOT NULL DEFAULT false, \
                 status TEXT NOT NULL, \
                 iterations INTEGER DEFAULT 0, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 completed_at TIMESTAMPTZ )",
        )
        .execute(pool)
        .await
        .expect("create nexus_subagent_runs");
        // agent_steps: il figlio ha lo stesso id in agent_runs, gli step si
        // correlano su run_id (persistiti incrementalmente). Il check no-progress
        // usa NOT EXISTS su questa tabella come segnale primario di progresso.
        sqlx::query(
            "CREATE TABLE agent_steps ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 run_id UUID NOT NULL, \
                 step_index INTEGER NOT NULL )",
        )
        .execute(pool)
        .await
        .expect("create agent_steps");
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

    /// Guardia no-progress DISABILITATA: per i test che esercitano l'orphan_timeout
    /// storico senza il nuovo check (comportamento pre-mig-0543).
    const NO_PROGRESS_OFF: NoProgressGuard = NoProgressGuard {
        enabled: false,
        timeout_s: 300,
    };

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
        // Due figli background, entrambi TERMINALI, ancorati alla session (anchor
        // depth-chain) ma DISPATCHATI dal run `parent` (dispatcher_run_id). La coda
        // e' VUOTA (l'enqueue non e' mai avvenuto).
        for st in ["completed", "timeout"] {
            sqlx::query(
                "INSERT INTO nexus_subagent_runs (parent_run_id, dispatcher_run_id, is_background, status) \
                 VALUES ($1, $2, true, $3)",
            )
            .bind(session)
            .bind(parent)
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
        let requeued = backstop_project(&pool, &pool, 900, NO_PROGRESS_OFF).await;
        assert_eq!(requeued, 1, "il backstop deve accodare il padre orfano");

        // Il padre accodato e' il RUN corrente (agent_runs.id), non l'anchor.
        let queued: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_run_id FROM subagent_fanin_resume_queue")
                .fetch_optional(&pool)
                .await
                .expect("select queued");
        assert_eq!(queued, Some(parent), "in coda ci deve essere il run padre");

        // Idempotente: un secondo passaggio non duplica (la riga esiste gia').
        let requeued2 = backstop_project(&pool, &pool, 900, NO_PROGRESS_OFF).await;
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
        // Ancorato a session (anchor), dispatchato dal run `parent`.
        sqlx::query(
            "INSERT INTO nexus_subagent_runs (parent_run_id, dispatcher_run_id, is_background, status, created_at) \
             VALUES ($1, $2, true, 'running', NOW() - interval '30 minutes')",
        )
        .bind(session)
        .bind(parent)
        .execute(&pool)
        .await
        .expect("insert figlio orfano");

        // Timeout ALTO (1h): il figlio (30 min) e' sotto soglia -> NON marcato, NON
        // accodato (potrebbe ancora vivere).
        let requeued_young = backstop_project(&pool, &pool, 3600, NO_PROGRESS_OFF).await;
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
        let requeued_old = backstop_project(&pool, &pool, 60, NO_PROGRESS_OFF).await;
        assert_eq!(
            requeued_old, 1,
            "figlio orfano oltre timeout -> padre accodato"
        );
        let status: String =
            sqlx::query_scalar("SELECT status FROM nexus_subagent_runs WHERE parent_run_id = $1")
                .bind(session)
                .fetch_one(&pool)
                .await
                .expect("status figlio");
        assert_eq!(status, "timeout", "la sub-run orfana e' marcata timeout");
    }

    /// GUARDIA NO-PROGRESS (mig 0543): un figlio background impantanato (`running`
    /// oltre la soglia no-progress, 0 agent_steps E 0 iterazioni) viene marcato
    /// `timeout` e il padre accodato. Un figlio che HA PROGREDITO (almeno 1
    /// agent_step) NON viene toccato dal check no-progress anche se oltre la stessa
    /// soglia (l'orphan_timeout, piu' lungo, non e' ancora scattato). Con la guardia
    /// DISABILITATA il check non agisce (solo l'orphan_timeout storico).
    #[sqlx::test]
    async fn backstop_no_progress_marca_solo_impantanati(pool: sqlx::PgPool) {
        create_backstop_tables(&pool).await;
        let session = Uuid::new_v4();
        let project = Uuid::new_v4();

        // Padre A: figlio bg IMPANTANATO (running 10 min, 0 step, 0 iter).
        let parent_stuck = Uuid::new_v4();
        let child_stuck = Uuid::new_v4();
        // Padre B: figlio bg che HA PROGREDITO (running 10 min, 1 step).
        let parent_working = Uuid::new_v4();
        let child_working = Uuid::new_v4();
        for p in [parent_stuck, parent_working] {
            sqlx::query(
                "INSERT INTO agent_runs (id, session_id, project_id, parent_run_id, status) \
                 VALUES ($1, $2, $3, NULL, 'awaiting_subagents')",
            )
            .bind(p)
            .bind(session)
            .bind(project)
            .execute(&pool)
            .await
            .expect("insert padre");
        }
        // Figlio impantanato: running da 10 min, iterations 0, nessun agent_step.
        sqlx::query(
            "INSERT INTO nexus_subagent_runs (id, parent_run_id, dispatcher_run_id, is_background, status, iterations, created_at) \
             VALUES ($1, $2, $3, true, 'running', 0, NOW() - interval '10 minutes')",
        )
        .bind(child_stuck)
        .bind(session)
        .bind(parent_stuck)
        .execute(&pool)
        .await
        .expect("insert figlio impantanato");
        // Figlio che lavora: running da 10 min ma con 1 agent_step gia' persistito.
        sqlx::query(
            "INSERT INTO nexus_subagent_runs (id, parent_run_id, dispatcher_run_id, is_background, status, iterations, created_at) \
             VALUES ($1, $2, $3, true, 'running', 0, NOW() - interval '10 minutes')",
        )
        .bind(child_working)
        .bind(session)
        .bind(parent_working)
        .execute(&pool)
        .await
        .expect("insert figlio che lavora");
        sqlx::query("INSERT INTO agent_steps (run_id, step_index) VALUES ($1, 0)")
            .bind(child_working)
            .execute(&pool)
            .await
            .expect("insert agent_step del figlio che lavora");

        // Guardia DISABILITATA: nessun figlio viene toccato dal check no-progress.
        // orphan_timeout ALTO (1h): neanche l'orphan scatta -> 0 accodati.
        let requeued_off = backstop_project(&pool, &pool, 3600, NO_PROGRESS_OFF).await;
        assert_eq!(
            requeued_off, 0,
            "guardia off + orphan alto -> nessun accodato"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM nexus_subagent_runs WHERE status = 'timeout'"
            )
            .fetch_one(&pool)
            .await
            .expect("count timeout off"),
            0,
            "guardia off: nessuna sub-run marcata timeout"
        );

        // Guardia ON con soglia no-progress 300s (orphan_timeout alto 1h, cosi'
        // l'orphan storico NON scatta: isola l'effetto del solo check no-progress).
        let no_progress_on = NoProgressGuard {
            enabled: true,
            timeout_s: 300,
        };
        let requeued_on = backstop_project(&pool, &pool, 3600, no_progress_on).await;
        // Solo il padre col figlio IMPANTANATO viene accodato.
        assert_eq!(
            requeued_on, 1,
            "solo il padre col figlio impantanato viene accodato"
        );
        // Il figlio impantanato e' marcato timeout.
        let stuck_status: String =
            sqlx::query_scalar("SELECT status FROM nexus_subagent_runs WHERE id = $1")
                .bind(child_stuck)
                .fetch_one(&pool)
                .await
                .expect("status impantanato");
        assert_eq!(stuck_status, "timeout", "figlio impantanato -> timeout");
        // Il figlio che LAVORA (1 agent_step) resta running: il check no-progress lo
        // ignora (ha progredito), l'orphan_timeout (1h) non e' ancora scattato.
        let working_status: String =
            sqlx::query_scalar("SELECT status FROM nexus_subagent_runs WHERE id = $1")
                .bind(child_working)
                .fetch_one(&pool)
                .await
                .expect("status che lavora");
        assert_eq!(
            working_status, "running",
            "figlio con progresso NON marcato dal check no-progress"
        );
        // Il padre col figlio che lavora NON e' in coda.
        let working_queued: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM subagent_fanin_resume_queue WHERE parent_run_id = $1",
        )
        .bind(parent_working)
        .fetch_one(&pool)
        .await
        .expect("count working queued");
        assert_eq!(working_queued, 0, "padre col figlio vivo NON accodato");
    }

    /// Replica LOCALE della meccanica di `fanin_enqueue_if_last` (privata in
    /// subagent_native): "se tutti i figli DIRETTI (dispatcher_run_id = dispatcher)
    /// background sono terminali, accoda il dispatcher (idempotente)". Serve ai test
    /// E2E del ciclo per simulare l'enqueue che i finalize dei figli eseguono, senza
    /// dipendere dal modulo privato. Ritorna true se ha accodato/gia' in coda.
    async fn enqueue_if_all_direct_terminal(
        pool: &sqlx::PgPool,
        dispatcher: Uuid,
        project: Uuid,
        session: Uuid,
    ) -> bool {
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_subagent_runs \
             WHERE dispatcher_run_id = $1 AND is_background = true \
               AND status IN ('running', 'paused')",
        )
        .bind(dispatcher)
        .fetch_one(pool)
        .await
        .expect("count remaining");
        if remaining > 0 {
            return false;
        }
        sqlx::query(
            "INSERT INTO subagent_fanin_resume_queue (parent_run_id, project_id, session_id) \
             VALUES ($1, $2, $3) ON CONFLICT (parent_run_id) DO NOTHING",
        )
        .bind(dispatcher)
        .bind(project)
        .bind(session)
        .execute(pool)
        .await
        .expect("enqueue");
        true
    }

    /// Il CAS + DELETE-al-vincere del worker (ALTA 2): estrae la sequenza che
    /// `process_queue_row` esegue quando vince il CAS, cosi' i test E2E possono
    /// riprodurla senza AppState/resume_fanin. Ritorna true se ha vinto il CAS
    /// (parent era `awaiting_subagents`), avendo gia' cancellato la riga di coda.
    async fn cas_and_delete_on_win(pool: &sqlx::PgPool, parent: Uuid) -> bool {
        let won = sqlx::query_scalar::<_, Uuid>(
            "UPDATE agent_runs SET status = 'running' \
             WHERE id = $1 AND status = 'awaiting_subagents' RETURNING id",
        )
        .bind(parent)
        .fetch_optional(pool)
        .await
        .expect("cas")
        .is_some();
        if won {
            // Delete-al-CAS (il fix ALTA 2): la riga sparisce PRIMA del resume.
            sqlx::query("DELETE FROM subagent_fanin_resume_queue WHERE parent_run_id = $1")
                .bind(parent)
                .execute(pool)
                .await
                .expect("delete queue row");
        }
        won
    }

    /// SCENARIO 1 (CICLO COMPLETO): parent `awaiting_subagents` + 1 figlio bg
    /// (dispatcher = parent). Il figlio diventa terminale -> l'enqueue (dispatcher-
    /// based) accoda il PARENT -> il worker vince il CAS -> il fetch dei figli
    /// (dispatcher = parent) ritorna SOLO i figli di quel dispatcher.
    #[sqlx::test]
    async fn fanin_ciclo_completo_dispatcher_based(pool: sqlx::PgPool) {
        create_backstop_tables(&pool).await;
        let parent = Uuid::new_v4();
        let session = Uuid::new_v4();
        let project = Uuid::new_v4();
        let altro_run = Uuid::new_v4(); // un ALTRO run della stessa sessione

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

        // Figlio bg del PARENT (dispatcher=parent), ancora running.
        sqlx::query(
            "INSERT INTO nexus_subagent_runs (parent_run_id, dispatcher_run_id, is_background, status) \
             VALUES ($1, $2, true, 'running')",
        )
        .bind(session) // anchor degenere
        .bind(parent)
        .execute(&pool)
        .await
        .expect("insert figlio del parent");
        // Un figlio di un ALTRO run della stessa sessione (dispatcher=altro_run),
        // running: NON deve influenzare la COUNT/fetch del parent (isolamento).
        sqlx::query(
            "INSERT INTO nexus_subagent_runs (parent_run_id, dispatcher_run_id, is_background, status) \
             VALUES ($1, $2, true, 'running')",
        )
        .bind(session)
        .bind(altro_run)
        .execute(&pool)
        .await
        .expect("insert figlio altro run");

        // Figlio del parent ancora running -> NON accoda.
        assert!(
            !enqueue_if_all_direct_terminal(&pool, parent, project, session).await,
            "figlio del parent running -> no enqueue"
        );

        // Il figlio del parent diventa terminale (finalize). Il figlio dell'altro
        // run resta running: NON deve bloccare il parent (dispatcher diverso).
        sqlx::query(
            "UPDATE nexus_subagent_runs SET status = 'completed' \
             WHERE dispatcher_run_id = $1",
        )
        .bind(parent)
        .execute(&pool)
        .await
        .expect("chiudi figlio parent");
        assert!(
            enqueue_if_all_direct_terminal(&pool, parent, project, session).await,
            "tutti i figli DIRETTI del parent terminali -> accoda (l'altro run non conta)"
        );

        // Coda: contiene il PARENT.
        let queued: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_run_id FROM subagent_fanin_resume_queue")
                .fetch_optional(&pool)
                .await
                .expect("select queued");
        assert_eq!(queued, Some(parent), "in coda il run parent (dispatcher)");

        // Il worker vince il CAS e cancella la riga (delete-al-CAS).
        assert!(
            cas_and_delete_on_win(&pool, parent).await,
            "CAS vinto sul parent"
        );
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subagent_fanin_resume_queue")
            .fetch_one(&pool)
            .await
            .expect("count after cas");
        assert_eq!(after, 0, "riga cancellata al CAS (ALTA 2)");

        // Fetch dei figli del parent (dispatcher=parent): SOLO 1, l'altro run e' escluso.
        let n_figli_parent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_subagent_runs \
             WHERE dispatcher_run_id = $1 AND is_background = true",
        )
        .bind(parent)
        .fetch_one(&pool)
        .await
        .expect("count figli parent");
        assert_eq!(
            n_figli_parent, 1,
            "il fan-in del parent vede SOLO il suo figlio diretto, non quello dell'altro run"
        );
    }

    /// SCENARIO 3 (ALTA 2): il worker vince il CAS e cancella la riga PRIMA del
    /// resume. Il resume ri-sospende il padre (2a ondata) con un figlio ancora
    /// `running`. Verifica che NON esista una riga di coda che riprenderebbe il
    /// padre finche' la 2a ondata non e' terminale (solo il loro completamento crea
    /// una riga FRESCA). Col vecchio keep-row la riga vecchia resterebbe e ri-
    /// vincerebbe il CAS con la 2a ondata `running` -> resume PARZIALE.
    #[sqlx::test]
    async fn fanin_no_ripresa_prematura_seconda_ondata(pool: sqlx::PgPool) {
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

        // 1a ONDATA: un figlio bg del parent, gia' terminale -> enqueue.
        sqlx::query(
            "INSERT INTO nexus_subagent_runs (parent_run_id, dispatcher_run_id, is_background, status) \
             VALUES ($1, $2, true, 'completed')",
        )
        .bind(session)
        .bind(parent)
        .execute(&pool)
        .await
        .expect("insert figlio 1a ondata");
        assert!(enqueue_if_all_direct_terminal(&pool, parent, project, session).await);

        // Il worker vince il CAS e CANCELLA la riga (fix ALTA 2), poi (simulato) il
        // resume ri-sospende il padre e spawna la 2a ondata ANCORA running.
        assert!(cas_and_delete_on_win(&pool, parent).await, "CAS 1a ondata");
        sqlx::query("UPDATE agent_runs SET status = 'awaiting_subagents' WHERE id = $1")
            .bind(parent)
            .execute(&pool)
            .await
            .expect("resume ri-sospende");
        // 2a ondata: figlio del parent ANCORA running (spawnato dopo il resume).
        sqlx::query(
            "INSERT INTO nexus_subagent_runs (parent_run_id, dispatcher_run_id, is_background, status) \
             VALUES ($1, $2, true, 'running')",
        )
        .bind(session)
        .bind(parent)
        .execute(&pool)
        .await
        .expect("insert figlio 2a ondata");

        // INVARIANTE (ALTA 2): la coda e' VUOTA (la riga vecchia e' stata cancellata
        // al CAS e la 2a ondata non ha ancora accodato: il suo figlio e' running).
        // Col vecchio keep-row la riga vecchia sarebbe ancora qui -> ripresa
        // prematura al giro dopo.
        let in_coda: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subagent_fanin_resume_queue")
            .fetch_one(&pool)
            .await
            .expect("count coda");
        assert_eq!(
            in_coda, 0,
            "ALTA 2: nessuna riga di coda mentre la 2a ondata e' running (no ripresa prematura)"
        );
        // Un tentativo di enqueue ora NON accoda (figlio 2a ondata running).
        assert!(
            !enqueue_if_all_direct_terminal(&pool, parent, project, session).await,
            "2a ondata running -> nessun enqueue"
        );
        // Il worker NON riprende il padre: nessuna riga da processare, quindi il
        // padre resta `awaiting_subagents` (non ripreso con risultati parziali).
        let status: String = sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
            .bind(parent)
            .fetch_one(&pool)
            .await
            .expect("status padre");
        assert_eq!(
            status, "awaiting_subagents",
            "padre ancora sospeso: la 2a ondata non e' terminale"
        );

        // La 2a ondata COMPLETA -> enqueue FRESCO -> ora il worker puo' riprendere.
        sqlx::query(
            "UPDATE nexus_subagent_runs SET status = 'completed' \
             WHERE dispatcher_run_id = $1 AND status = 'running'",
        )
        .bind(parent)
        .execute(&pool)
        .await
        .expect("chiudi 2a ondata");
        assert!(
            enqueue_if_all_direct_terminal(&pool, parent, project, session).await,
            "2a ondata terminale -> enqueue fresco"
        );
        let in_coda2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subagent_fanin_resume_queue")
            .fetch_one(&pool)
            .await
            .expect("count coda 2");
        assert_eq!(
            in_coda2, 1,
            "riga fresca creata solo al completamento della 2a ondata"
        );
        assert!(
            cas_and_delete_on_win(&pool, parent).await,
            "ora il CAS vince e riprende il padre con la 2a ondata TERMINALE"
        );
    }
}
