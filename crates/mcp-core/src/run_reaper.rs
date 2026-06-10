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
pub async fn reap_stale_runs(db: &PgPool, stale_seconds: i64) -> Vec<uuid::Uuid> {
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
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if reaped.is_empty() {
        return reaped;
    }

    tracing::warn!(
        "run_reaper: marcati {} run 'running' orfani (stale>{}s) come 'interrupted'",
        reaped.len(),
        stale_seconds
    );

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
