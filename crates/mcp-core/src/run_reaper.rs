//! Punto unico (regola L) di chiusura dei run agentici orfani — recovery
//! selettivo (mig 0392).
//!
//! Causa radice (regola H): la chiusura dei run 'running' orfani era duplicata in
//! DUE punti con la stessa logica difettosa — il recovery all'avvio (main.rs) e il
//! task_watchdog — entrambi marcavano i run su `created_at` (non distinguendo un
//! run VIVO da uno ORFANO: il loop agentico vive nel brain, separato da mcp-core)
//! e includendo 'awaiting_confirmation' (stato resumibile via checkpoint LangGraph,
//! che venia cosi' distrutto).
//!
//! Fix: il brain batte `agent_runs.updated_at` a ogni iterazione (heartbeat di
//! liveness, sopravvive al restart di mcp-core). Qui — UNICA implementazione —
//! marchiamo 'interrupted' SOLO i run 'running' fermi oltre soglia, lasciando in
//! pace i vivi e gli 'awaiting_confirmation'. I due chiamanti (recovery all'avvio
//! in main.rs, sweep periodico nel task_watchdog) delegano a questa funzione
//! invece di re-implementarla.

use sqlx::PgPool;

/// Messaggio scritto nel `final_answer` del run orfano. Il messaggio VISIBILE in
/// chat e' comunque rigenerato dal frontend a partire dallo status 'interrupted'
/// (run-summary.ts); questo e' il fallback persistito.
const INTERRUPTED_MSG: &str = "L'elaborazione si è interrotta e non è stato \
    possibile completarla. Puoi ripetere la richiesta.";

/// Default della soglia se il setting manca (15 min): allineato allo storico
/// timeout del task_watchdog, generoso per non uccidere run con tool lunghi.
const DEFAULT_STALE_SECONDS: i64 = 900;

/// Legge `agent.run_recovery.stale_after_seconds` dal DB (regola G), con guard
/// minima 30s. Usata da entrambi i chiamanti del reaper.
pub async fn stale_seconds_from_settings(db: &PgPool) -> i64 {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.run_recovery.stale_after_seconds'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse::<i64>().ok())
    .unwrap_or(DEFAULT_STALE_SECONDS)
    .max(30)
}

/// Marca 'interrupted' i run 'running' fermi oltre `stale_seconds` (heartbeat
/// `updated_at`) e inserisce il messaggio assistente per quelli appena reapati.
/// NON tocca 'awaiting_confirmation' (resumibile via checkpoint + /agent/approve).
/// Ritorna gli id dei run reapati, cosi' il chiamante puo' sbloccare gli
/// EventSource ancora in ascolto (emissione `is_final` sul broadcast channel).
/// Elenco dei project_id (tabella globale `projects`, meta-DB). Il reaper itera
/// i progetti per girare la chiusura sul DB di ciascuno (separazione DB): a flag
/// off `project_data_pool_from` ritorna il meta-DB e la prima iterazione reapa
/// tutto, le successive trovano vuoto; a flag on ogni pool e' gia' scoped.
async fn list_project_ids(meta: &PgPool) -> Vec<uuid::Uuid> {
    sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM projects")
        .fetch_all(meta)
        .await
        .unwrap_or_default()
}

pub async fn reap_stale_runs(db: &PgPool, stale_seconds: i64) -> Vec<uuid::Uuid> {
    let mut all_reaped = Vec::new();
    for project_id in list_project_ids(db).await {
        let pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
        let reaped: Vec<uuid::Uuid> = sqlx::query_scalar::<_, uuid::Uuid>(
            r#"
            UPDATE agent_runs
            SET status = 'interrupted',
                final_answer = COALESCE(final_answer, $1),
                completed_at = NOW()
            WHERE status = 'running'
              AND completed_at IS NULL
              AND COALESCE(updated_at, created_at) < NOW() - make_interval(secs => $2)
            RETURNING id
            "#,
        )
        .bind(INTERRUPTED_MSG)
        .bind(stale_seconds as f64)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
        if reaped.is_empty() {
            continue;
        }
        tracing::warn!(
            project_id = %project_id,
            "run_reaper: marcati {} run 'running' orfani (stale>{}s) come 'interrupted'",
            reaped.len(),
            stale_seconds
        );
        all_reaped.extend(finalize_reaped(&pool, reaped).await);
    }
    all_reaped
}

/// Bootstrap recovery (regola H, causa radice del "run orfano blocca la sessione"):
/// marca 'interrupted' TUTTI i run 'running' SENZA clausola temporale. Dopo un
/// restart di mcp-core un run 'running' e' per definizione orfano (il task che lo
/// eseguiva — heartbeat `updated_at` incluso — e' morto col processo precedente),
/// indipendentemente da quanto e' recente `updated_at`: la guardia stale del
/// reaper periodico (corretta a runtime, non uccide run lunghi vivi) e' SBAGLIATA
/// al boot e lasciava sopravvivere gli orfani recenti, che poi bloccavano i nuovi
/// run sulla sessione (gate 409 / session_has_active_run su status='running').
/// Esclude 'awaiting_confirmation' (resumibile via checkpoint + /agent/approve).
/// Da chiamare PRIMA del bind del listener HTTP: nessun run del processo corrente
/// puo' ancora esistere, quindi ogni 'running' e' un orfano. Gated da
/// `agent.run_recovery.reap_all_at_boot` (regola G, default true); se false ricade
/// sul reap time-gated (compatibilita').
pub async fn reap_orphaned_runs_at_boot(db: &PgPool) -> Vec<uuid::Uuid> {
    if !reap_all_at_boot_enabled(db).await {
        let stale = stale_seconds_from_settings(db).await;
        return reap_stale_runs(db, stale).await;
    }
    let mut all_reaped = Vec::new();
    for project_id in list_project_ids(db).await {
        let pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
        let reaped: Vec<uuid::Uuid> = sqlx::query_scalar::<_, uuid::Uuid>(
            r#"
            UPDATE agent_runs
            SET status = 'interrupted',
                final_answer = COALESCE(final_answer, $1),
                completed_at = NOW()
            WHERE status = 'running'
              AND completed_at IS NULL
            RETURNING id
            "#,
        )
        .bind(INTERRUPTED_MSG)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
        if reaped.is_empty() {
            continue;
        }
        tracing::warn!(
            project_id = %project_id,
            "run_reaper: bootstrap, marcati {} run 'running' orfani come 'interrupted' (no time-gate)",
            reaped.len()
        );
        all_reaped.extend(finalize_reaped(&pool, reaped).await);
    }
    all_reaped
}

/// Flag DB (regola G): il reaper di bootstrap marca TUTTI i 'running' (true,
/// default) oppure ricade sul time-gating periodico (false). Niente hardcode di
/// comportamento: il default true vale solo se il setting manca.
async fn reap_all_at_boot_enabled(db: &PgPool) -> bool {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.run_recovery.reap_all_at_boot'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
    .unwrap_or(true)
}

/// Corpo comune (regola L) dei due reaper: per i run appena marcati 'interrupted'
/// registra il worklog di sessione e inserisce il messaggio assistente. Ritorna
/// gli id reapati cosi' il chiamante puo' sbloccare gli EventSource in ascolto.
async fn finalize_reaped(db: &PgPool, reaped: Vec<uuid::Uuid>) -> Vec<uuid::Uuid> {
    // Worklog di sessione (mig 0411): anche il lavoro dei run interrotti
    // (crash/stallo) entra nella storia di lavoro — gli agent_steps sono gia'
    // in DB grazie alla persistenza incrementale del brain (M68). Il run
    // successivo sulla stessa sessione vede cosa era gia' stato fatto invece
    // di ripartire da zero. Best-effort per singolo run.
    for rid in &reaped {
        if let Err(e) = crate::session_worklog::ingest_from_db_steps(
            db,
            *rid,
            "interrotto (crash o stallo del servizio)",
        )
        .await
        {
            tracing::warn!(error = %e, run_id = %rid, "session_worklog: ingest al reap fallito");
        }
    }

    // Messaggio assistente SOLO per i run appena reapati (completed_at recente),
    // se hanno un messaggio-richiesta e non hanno gia' un assistente associato.
    let inserted = sqlx::query(
        r#"
        INSERT INTO chat_messages
            (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
        SELECT
            gen_random_uuid(),
            ar.session_id,
            ar.project_id,
            'assistant',
            $1,
            jsonb_build_object(
                'agentRunId', ar.id::text,
                'automationMode', 'agent',
                'interrupted', true
            ),
            ar.run_message_id,
            NOW()
        FROM agent_runs ar
        WHERE ar.status = 'interrupted'
          AND ar.run_message_id IS NOT NULL
          AND ar.completed_at > NOW() - interval '20 seconds'
          AND NOT EXISTS (
              SELECT 1 FROM chat_messages cm
              WHERE cm.request_message_id = ar.run_message_id
                AND cm.role = 'assistant'
          )
        "#,
    )
    .bind(INTERRUPTED_MSG)
    .execute(db)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);

    if inserted > 0 {
        tracing::warn!(
            "run_reaper: inseriti {} messaggi assistente per run orfani senza risposta",
            inserted
        );
    }

    reaped
}
