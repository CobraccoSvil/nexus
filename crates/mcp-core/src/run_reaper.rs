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
/// Il reaper itera i progetti (punto unico `list_all_project_ids`) per girare la
/// chiusura sul DB di ciascuno (separazione DB): a flag off
/// `project_data_pool_from` ritorna il meta-DB e la prima iterazione reapa
/// tutto, le successive trovano vuoto; a flag on ogni pool e' gia' scoped.
pub async fn reap_stale_runs(db: &PgPool, stale_seconds: i64) -> Vec<uuid::Uuid> {
    let mut all_reaped = Vec::new();
    for project_id in crate::project_db_routes::list_all_project_ids(db).await {
        let pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
        let reaped = mark_stale_on_pool(&pool, stale_seconds).await;
        if reaped.is_empty() {
            continue;
        }
        tracing::warn!(
            project_id = %project_id,
            "run_reaper: marcati {} run 'running' orfani (stale>{}s) come 'interrupted'",
            reaped.len(),
            stale_seconds
        );
        all_reaped.extend(finalize_reaped(db, &pool, reaped).await);
    }
    all_reaped
}

/// Marca stantii i run di UN pool. Estratta da [`reap_stale_runs`] per separare
/// "su quali pool girare" (che richiede il meta-DB e i progetti) da "cosa fare su
/// un pool" — che e' la logica vera, e cosi' e' testabile con un solo DB.
///
/// Scrive DUE cose, e sono cose diverse:
///   - lo STATO (`status`, `final_answer`, `completed_at`): per chi legge la riga
///     — la UI, i report, chi indaga dopo;
///   - il SEGNALE (`cancellation_requested`): per il TASK che sta ancora girando.
/// Prima scriveva solo il primo, ed e' il difetto: `is_superseded`
/// (`run_control_store.rs:48`) legge ESCLUSIVAMENTE `cancellation_requested`, quindi
/// il reaper chiudeva la riga e sbloccava la UI mentre il task tokio non lo sapeva.
/// Se il blocco si scioglieva, il run riprendeva a bruciare token su una riga gia'
/// chiusa, con `completed_at` valorizzato. Due punti chiudevano un run
/// (`supersede_active_runs` e il reaper) e uno solo lo diceva (regola L).
///
/// `COALESCE` su entrambi i campi: se una cancellazione utente e' gia' registrata,
/// la sua ora e il suo motivo restano — e' arrivata prima, ed e' piu' informativa
/// di "reaped_stale".
async fn mark_stale_on_pool(pool: &PgPool, stale_seconds: i64) -> Vec<uuid::Uuid> {
    sqlx::query_scalar::<_, uuid::Uuid>(
        r#"
        UPDATE agent_runs
        SET status = 'interrupted',
            final_answer = COALESCE(final_answer, $1),
            completed_at = NOW(),
            cancellation_requested = COALESCE(cancellation_requested, NOW()),
            cancellation_reason = COALESCE(cancellation_reason, 'reaped_stale')
        WHERE status = 'running'
          AND completed_at IS NULL
          AND COALESCE(updated_at, created_at) < NOW() - make_interval(secs => $2)
        RETURNING id
        "#,
    )
    .bind(INTERRUPTED_MSG)
    .bind(stale_seconds as f64)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
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
    for project_id in crate::project_db_routes::list_all_project_ids(db).await {
        let pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
        let reaped: Vec<uuid::Uuid> = sqlx::query_scalar::<_, uuid::Uuid>(
            r#"
            UPDATE agent_runs
            SET status = 'interrupted',
                final_answer = COALESCE(final_answer, $1),
                completed_at = NOW(),
                -- Stesso segnale del reap periodico (regola L: chiudere un run e'
                -- UNA cosa, e si dice in UN modo). Al boot il task che eseguiva il
                -- run e' morto col processo precedente, quindi nessuno leggera'
                -- questo campo per QUESTO run — ma la riga resta coerente: chi la
                -- ispeziona dopo trova un motivo, non solo uno stato.
                cancellation_requested = COALESCE(cancellation_requested, NOW()),
                cancellation_reason = COALESCE(cancellation_reason, 'reaped_at_boot')
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
        all_reaped.extend(finalize_reaped(db, &pool, reaped).await);
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
    .map(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
    .unwrap_or(true)
}

/// Corpo comune (regola L) dei due reaper: per i run appena marcati 'interrupted'
/// registra il worklog di sessione e inserisce il messaggio assistente. Ritorna
/// gli id reapati cosi' il chiamante puo' sbloccare gli EventSource in ascolto.
/// `meta` = meta-DB (settings worklog); `db` = pool del DB progetto reapato.
async fn finalize_reaped(meta: &PgPool, db: &PgPool, reaped: Vec<uuid::Uuid>) -> Vec<uuid::Uuid> {
    // Worklog di sessione (mig 0411): anche il lavoro dei run interrotti
    // (crash/stallo) entra nella storia di lavoro — gli agent_steps sono gia'
    // in DB grazie alla persistenza incrementale del brain (M68). Il run
    // successivo sulla stessa sessione vede cosa era gia' stato fatto invece
    // di ripartire da zero. Best-effort per singolo run.
    for rid in &reaped {
        if let Err(e) = crate::session_worklog::ingest_from_db_steps(
            meta,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Schema minimo di `agent_runs` per il reaper: le colonne che tocca e quella
    /// che il TASK legge (`cancellation_requested`).
    async fn crea_agent_runs(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 status TEXT NOT NULL, \
                 final_answer TEXT, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 updated_at TIMESTAMPTZ, \
                 completed_at TIMESTAMPTZ, \
                 cancellation_requested TIMESTAMPTZ, \
                 cancellation_reason TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("tabella agent_runs");
    }

    /// IL TEST CHE CONTA: il reaper deve emettere il SEGNALE DI STOP, non solo
    /// cambiare lo stato.
    ///
    /// Il difetto (misurato il 16/07): il reaper scriveva `status='interrupted'` e
    /// `completed_at`, sbloccando la UI, ma NON `cancellation_requested` — l'unico
    /// campo che `is_superseded` (run_control_store.rs:48) legge. Il task tokio non
    /// sapeva di essere stato chiuso: se il blocco si scioglieva, riprendeva a
    /// bruciare token su una riga gia' chiusa.
    ///
    /// Due punti chiudono un run (`supersede_active_runs` e il reaper) e solo uno
    /// lo diceva: regola L.
    #[sqlx::test]
    async fn il_reap_emette_il_segnale_di_stop_non_solo_lo_stato(pool: PgPool) {
        crea_agent_runs(&pool).await;
        // Un run fermo da 1000s: oltre la soglia di 900s.
        let id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO agent_runs (status, created_at, updated_at) \
             VALUES ('running', NOW() - interval '1000 seconds', \
                     NOW() - interval '1000 seconds') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .expect("run stantio");

        let reaped = mark_stale_on_pool(&pool, 900).await;
        assert_eq!(reaped, vec![id], "il run stantio deve essere reapato");

        let (status, cancel, reason): (String, Option<chrono::DateTime<chrono::Utc>>, Option<String>) =
            sqlx::query_as(
                "SELECT status, cancellation_requested, cancellation_reason \
                   FROM agent_runs WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("riga");
        assert_eq!(status, "interrupted", "lo STATO, per chi legge la riga");
        assert!(
            cancel.is_some(),
            "il SEGNALE, per il TASK che gira ancora: senza questo campo \
             is_superseded risponde false e il run riprende a bruciare token su \
             una riga gia' chiusa"
        );
        assert_eq!(reason.as_deref(), Some("reaped_stale"));
    }

    /// Un run VIVO (heartbeat recente) non si tocca. E' il difetto opposto, e la
    /// mig 0392 nasce proprio da li': un reaper troppo aggressivo uccideva run che
    /// stavano lavorando.
    #[sqlx::test]
    async fn un_run_vivo_non_viene_reapato(pool: PgPool) {
        crea_agent_runs(&pool).await;
        // Nato 1000s fa ma con l'heartbeat battuto ORA: sta lavorando.
        let id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO agent_runs (status, created_at, updated_at) \
             VALUES ('running', NOW() - interval '1000 seconds', NOW()) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .expect("run vivo");
        assert!(
            mark_stale_on_pool(&pool, 900).await.is_empty(),
            "il criterio e' la LIVENESS (updated_at), non l'eta': un run che batte \
             il cuore sta lavorando, anche se e' nato 1000s fa"
        );
        let cancel: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT cancellation_requested FROM agent_runs WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert!(cancel.is_none(), "nessun segnale di stop a un run vivo");
    }

    /// Una cancellazione dell'UTENTE gia' registrata non viene sovrascritta: e'
    /// arrivata prima, e il suo motivo e' piu' informativo di 'reaped_stale'.
    #[sqlx::test]
    async fn il_reap_non_sovrascrive_una_cancellazione_utente(pool: PgPool) {
        crea_agent_runs(&pool).await;
        let id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO agent_runs \
                 (status, created_at, updated_at, cancellation_requested, cancellation_reason) \
             VALUES ('running', NOW() - interval '1000 seconds', NOW() - interval '1000 seconds', \
                     NOW() - interval '500 seconds', 'utente') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .expect("run cancellato dall'utente");
        mark_stale_on_pool(&pool, 900).await;
        let reason: Option<String> =
            sqlx::query_scalar("SELECT cancellation_reason FROM agent_runs WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert_eq!(
            reason.as_deref(),
            Some("utente"),
            "il motivo dell'utente resta: e' arrivato prima ed e' piu' informativo"
        );
    }
}
