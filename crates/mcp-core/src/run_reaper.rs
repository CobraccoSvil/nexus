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

/// Guardia comune delle risposte inserite da fuori: non si scrive una seconda
/// assistente per una richiesta che ne ha gia' una.
///
/// Frammento SQL condiviso (stesso pattern di
/// [`crate::agent_types::ACTIVE_RUN_STATUS_SQL`]): la usano ENTRAMBI i percorsi
/// che rispondono al posto di un run che non parlera' piu' — la chiusura degli
/// orfani e quella delle sospensioni scadute. Ricopiarla renderebbe possibile
/// correggerne una e non l'altra, e il difetto sarebbe una doppia risposta in
/// chat sulla stessa richiesta. Presuppone l'alias `ar` su `agent_runs`.
const SENZA_RISPOSTA_ASSISTENTE: &str = "NOT EXISTS ( \
     SELECT 1 FROM chat_messages cm \
      WHERE cm.request_message_id = ar.run_message_id \
        AND cm.role = 'assistant' )";

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
/// chiusura sul DB di ciascuno (separazione DB, sempre attiva da mig 0527): ogni
/// pool e' scoped al progetto; un progetto col DB non disponibile viene saltato
/// con WARN per questo giro (niente fallback al meta-DB).
pub async fn reap_stale_runs(db: &PgPool, stale_seconds: i64) -> Vec<uuid::Uuid> {
    let mut all_reaped = Vec::new();
    for project_id in crate::project_db_routes::list_all_project_ids(db).await {
        let pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
            Ok(pool) => pool,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "run_reaper: DB progetto non disponibile, progetto saltato per questo giro");
                continue;
            }
        };
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

/// Chiude i run la cui SOSPENSIONE e' maturata: era in attesa di una decisione
/// umana che, nella modalita' in cui girava, nessuno poteva prendere.
///
/// CAUSA RADICE (rilievo A4, ADR 0043). Il gate duale sui passi critici sospende
/// in HITL anche in Automatic — e' il punto del requisito — ma li' non c'e'
/// nessun umano. `mark_stale_on_pool` non poteva raccoglierlo per costruzione
/// (filtra `status = 'running'`, e l'esclusione degli `awaiting_confirmation` e'
/// il contratto della mig 0392: sono resumibili via checkpoint), e
/// `ACTIVE_RUN_STATUSES` li conta fra i run che OCCUPANO la sessione. Il run
/// notturno restava appeso per sempre e ingorgava la sessione.
///
/// PERCHE' UNA FUNZIONE SORELLA e non un ramo del reap. Il criterio e' un altro
/// (una scadenza scritta sulla riga, non l'assenza di battito) e soprattutto lo
/// e' il CONTRATTO DI CHIUSURA: `mark_stale_on_pool` marca `interrupted` — "e'
/// morto qualcosa" — e alza il segnale di stop per il task che gira ancora. Qui
/// non e' morto niente e non gira niente: il run si e' fermato dove doveva, e
/// chiude con l'esito STRUTTURATO che descrive il perche' (`blocked_needs_input`
/// + blocker derivato dal kind, ADR 0034). Chiuderlo `interrupted` avrebbe detto
/// una cosa falsa con la faccia di un guasto.
///
/// Nessun `cancellation_requested`: quel campo e' il segnale al task tokio che
/// sta ancora girando (vedi [`mark_stale_on_pool`]), e un run sospeso su
/// checkpoint non ne ha uno. Scriverlo direbbe "qualcuno ha chiesto lo stop" di
/// una chiusura che nessuno ha chiesto.
///
/// Sono toccate SOLO le righe con una scadenza scritta: le sospensioni di
/// Confirm hanno `suspension_expires_at` NULL (l'utente e' al terminale) e
/// restano intatte, come i run sospesi prima della mig project 0016.
pub async fn expire_matured_suspensions(db: &PgPool) -> Vec<uuid::Uuid> {
    let mut tutti = Vec::new();
    for project_id in crate::project_db_routes::list_all_project_ids(db).await {
        let pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
            Ok(pool) => pool,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id, error = %e,
                    "scadenza sospensioni: DB progetto non disponibile, saltato per questo giro"
                );
                continue;
            }
        };
        let maturate = expire_on_pool(&pool).await;
        for (run_id, kind) in maturate {
            let origin = kind
                .as_deref()
                .and_then(nexus_agent_graph::decisions::SuspensionOrigin::from_db_str);
            tracing::warn!(
                project_id = %project_id,
                run_id = %run_id,
                suspension_kind = kind.as_deref().unwrap_or("(illeggibile)"),
                blocker = origin
                    .map(nexus_agent_graph::decisions::SuspensionOrigin::blocker)
                    .unwrap_or("(non dichiarato)"),
                "sospensione scaduta: run chiuso blocked_needs_input"
            );
            // Il lavoro fatto prima della sospensione entra nella storia di
            // sessione, come per i run reapati: il run successivo vede cosa era
            // gia' stato fatto invece di ripartire da zero.
            if let Err(e) = crate::session_worklog::ingest_from_db_steps(
                db,
                &pool,
                run_id,
                "bloccato (sospensione scaduta senza decisione umana)",
            )
            .await
            {
                tracing::warn!(error = %e, run_id = %run_id, "session_worklog: ingest alla scadenza fallito");
            }
            insert_blocked_message(&pool, run_id, origin).await;
            tutti.push(run_id);
        }
    }
    tutti
}

/// L'UPDATE di maturazione su UN pool. Ritorna `(run_id, suspension_kind)`: il
/// kind lo porta la RIGA e non una seconda query, cosi' il messaggio dichiara la
/// causa che era scritta sul run nel momento in cui e' stato chiuso.
///
/// `suspension_expires_at < NOW()` e' l'unico criterio temporale: la scadenza e'
/// stata calcolata a monte dal punto unico `classify_suspension`, che conosceva
/// modalita' e budget residuo. Ricalcolarla qui sarebbe una seconda idea di
/// quando una sospensione muore.
async fn expire_on_pool(pool: &PgPool) -> Vec<(uuid::Uuid, Option<String>)> {
    sqlx::query_as::<_, (uuid::Uuid, Option<String>)>(
        r#"
        UPDATE agent_runs
        SET status = 'blocked_needs_input',
            completed_at = NOW()
        WHERE status = 'awaiting_confirmation'
          AND completed_at IS NULL
          AND suspension_expires_at IS NOT NULL
          AND suspension_expires_at < NOW()
        RETURNING id, suspension_kind
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_else(|e| {
        // Non si ingoia (regola H): l'errore atteso e' "colonna inesistente" su
        // un DB progetto che non ha ancora la mig project 0016, e dura finche'
        // il provisioning non la applica. Senza questa riga la sweep tacerebbe
        // per sempre su quel progetto, e il silenzio sarebbe indistinguibile
        // da "nessuna sospensione da chiudere".
        tracing::warn!(
            error = %e,
            "expire_matured_suspensions: query fallita, sospensioni non raccolte per questo progetto"
        );
        Vec::new()
    })
}

/// Messaggio in chat del run chiuso per scadenza: e' l'unico modo in cui la
/// persona che torna al mattino scopre cosa e' successo.
///
/// Il testo lo compone il punto unico del vocabolario (`nota_scadenza`), non
/// questa funzione. Il `metadata` porta l'esito in CAMPI (regola Q) —
/// `outcome`/`blocker` del vocabolario ADR 0034 — cosi' il frontend non deve
/// rileggere la prosa per sapere che genere di chiusura sia stata.
///
/// `blocker` ASSENTE quando il kind della riga e' illeggibile: un campo mancante
/// dice "non dichiarato", un `safety` inventato direbbe una causa che nessuno
/// ha accertato.
async fn insert_blocked_message(
    pool: &PgPool,
    run_id: uuid::Uuid,
    origin: Option<nexus_agent_graph::decisions::SuspensionOrigin>,
) {
    let testo = nexus_agent_graph::decisions::nota_scadenza(origin);
    let mut metadata = serde_json::json!({
        "agentRunId": run_id.to_string(),
        "automationMode": "agent",
        "outcome": "blocked",
    });
    if let Some(o) = origin {
        metadata["blocker"] = serde_json::Value::String(o.blocker().to_string());
        metadata["suspensionKind"] = serde_json::Value::String(o.as_str().to_string());
    }
    // Stessa guardia del messaggio dei run reapati, dal punto unico
    // `SENZA_RISPOSTA_ASSISTENTE`: niente doppia risposta in chat.
    let inserted = sqlx::query(&format!(
        "INSERT INTO chat_messages \
(id, session_id, project_id, role, content, metadata, request_message_id, created_at) \
SELECT gen_random_uuid(), ar.session_id, ar.project_id, 'assistant', $2, $3, \
ar.run_message_id, NOW() \
FROM agent_runs ar \
WHERE ar.id = $1 \
AND ar.run_message_id IS NOT NULL \
AND {SENZA_RISPOSTA_ASSISTENTE}"
    ))
    .bind(run_id)
    .bind(&testo)
    .bind(&metadata)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    if inserted == 0 {
        tracing::warn!(
            run_id = %run_id,
            "scadenza: nessun messaggio inserito (richiesta senza run_message_id o risposta gia' presente)"
        );
    }
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
        let pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
            Ok(pool) => pool,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "run_reaper boot: DB progetto non disponibile, progetto saltato per questo giro");
                continue;
            }
        };
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
    // Totali del run dai FATTI persistiti (punto unico `run_totals`). Chi muore
    // da fuori non passa dal finalizzatore, quindi la sua riga resta ai valori
    // iniziali: MISURATO su agenda-medica il 06/08/2026, un run `interrupted`
    // con 107 passi e 87 righe di ledger finalizzate (4.899.738 token, $0.0363)
    // si presentava all'utente come "0 tok - $0.000". Nessuna stima: si scrive
    // solo cio' che il ledger e gli step DICONO, e solo dove la riga e' a zero.
    for rid in &reaped {
        crate::run_totals::consolida_run(meta, db, *rid).await;
    }

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
    // se hanno un messaggio-richiesta e non hanno gia' un assistente associato
    // (guardia condivisa `SENZA_RISPOSTA_ASSISTENTE`).
    let inserted = sqlx::query(&format!(
        "INSERT INTO chat_messages \
(id, session_id, project_id, role, content, metadata, request_message_id, created_at) \
SELECT gen_random_uuid(), ar.session_id, ar.project_id, 'assistant', $1, \
jsonb_build_object('agentRunId', ar.id::text, 'automationMode', 'agent', 'interrupted', true), \
ar.run_message_id, NOW() \
FROM agent_runs ar \
WHERE ar.status = 'interrupted' \
AND ar.run_message_id IS NOT NULL \
AND ar.completed_at > NOW() - interval '20 seconds' \
AND {SENZA_RISPOSTA_ASSISTENTE}"
    ))
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

    /// Run del progetto con eta'/heartbeat espressi in secondi nel passato.
    /// Le tabelle le porta il migrator del set `db/migrations/project`; qui si
    /// semina la riga coi NOT NULL reali e le si retrodatano i tempi.
    async fn run_datato(pool: &PgPool, eta_s: i64, heartbeat_s: i64) -> uuid::Uuid {
        let id = crate::test_support::seed_agent_run(pool).await;
        sqlx::query(
            "UPDATE agent_runs SET status = 'running', \
                 created_at = NOW() - make_interval(secs => $2), \
                 updated_at = NOW() - make_interval(secs => $3) \
             WHERE id = $1",
        )
        .bind(id)
        .bind(eta_s as f64)
        .bind(heartbeat_s as f64)
        .execute(pool)
        .await
        .expect("retrodata il run");
        id
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
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_reap_emette_il_segnale_di_stop_non_solo_lo_stato(pool: PgPool) {
        // Un run fermo da 1000s: oltre la soglia di 900s.
        let id = run_datato(&pool, 1000, 1000).await;

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
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn un_run_vivo_non_viene_reapato(pool: PgPool) {
        // Nato 1000s fa ma con l'heartbeat battuto ORA: sta lavorando.
        let id = run_datato(&pool, 1000, 0).await;
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
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_reap_non_sovrascrive_una_cancellazione_utente(pool: PgPool) {
        let id = run_datato(&pool, 1000, 1000).await;
        sqlx::query(
            "UPDATE agent_runs SET cancellation_requested = NOW() - interval '500 seconds', \
                 cancellation_reason = 'utente' WHERE id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .expect("cancellazione utente registrata");
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

    // ── Scadenza delle sospensioni (rilievo A4) ──────────────────────────────

    /// Run SOSPESO con la scadenza espressa in secondi rispetto a ORA
    /// (negativi = gia' maturata). `None` = sospensione senza termine, cioe' il
    /// caso di Confirm, dove l'utente e' al terminale.
    async fn run_sospeso(
        pool: &PgPool,
        kind: &str,
        scadenza_fra_s: Option<i64>,
    ) -> uuid::Uuid {
        let id = crate::test_support::seed_agent_run(pool).await;
        sqlx::query(
            "UPDATE agent_runs SET status = 'awaiting_confirmation', \
                 suspension_kind = $2, \
                 suspension_expires_at = CASE WHEN $3::bigint IS NULL THEN NULL \
                     ELSE NOW() + make_interval(secs => $3::bigint) END \
             WHERE id = $1",
        )
        .bind(id)
        .bind(kind)
        .bind(scadenza_fra_s)
        .execute(pool)
        .await
        .expect("sospensione seminata");
        id
    }

    async fn stato_di(pool: &PgPool, id: uuid::Uuid) -> String {
        sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("riga")
    }

    /// IL TEST CHE CONTA: una sospensione del gate duale che nessuno ha sciolto
    /// matura e chiude il run con l'esito STRUTTURATO, non con un timeout muto.
    ///
    /// E' il difetto A4 per intero: prima nessuna sweep raccoglieva questo run
    /// (il reap filtra `status='running'` per contratto), quindi restava
    /// `awaiting_confirmation` per sempre e ACTIVE_RUN_STATUSES ingorgava la
    /// sessione. Mutazione: rimettere `status='running'` nel WHERE di
    /// `expire_on_pool` -> rosso.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn la_sospensione_maturata_chiude_il_run_bloccato(pool: PgPool) {
        let id = run_sospeso(&pool, "step_gate", Some(-60)).await;

        let maturate = expire_on_pool(&pool).await;

        assert_eq!(maturate.len(), 1, "la sospensione scaduta deve maturare");
        assert_eq!(maturate[0].0, id);
        assert_eq!(
            maturate[0].1.as_deref(),
            Some("step_gate"),
            "il kind viaggia con la riga: e' da li' che si deriva il blocker"
        );
        assert_eq!(
            stato_di(&pool, id).await,
            "blocked_needs_input",
            "l'esito e' quello STRUTTURATO del vocabolario (ADR 0034), non 'interrupted': \
             non e' morto niente, il run si e' fermato dove doveva"
        );
        let (completato, stop_richiesto): (
            Option<chrono::DateTime<chrono::Utc>>,
            Option<chrono::DateTime<chrono::Utc>>,
        ) = sqlx::query_as(
            "SELECT completed_at, cancellation_requested FROM agent_runs WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert!(completato.is_some(), "il run e' chiuso");
        assert!(
            stop_richiesto.is_none(),
            "nessun segnale di stop: quel campo parla al task tokio che gira, e un \
             run sospeso su checkpoint non ne ha uno"
        );
    }

    /// IL DANNO OPPOSTO, ed e' il piu' grave dei due: una sospensione SENZA
    /// termine e' quella di Confirm, dove l'utente e' al terminale. Chiuderla
    /// significherebbe uccidere un run che stava per essere approvato.
    ///
    /// Mutazione: togliere `suspension_expires_at IS NOT NULL` dal WHERE (o
    /// confrontare con `<=` una colonna NULL trattata come passato) -> rosso.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn una_sospensione_senza_termine_non_si_tocca(pool: PgPool) {
        let confirm = run_sospeso(&pool, "human_review", None).await;
        let non_ancora = run_sospeso(&pool, "step_gate", Some(3600)).await;

        assert!(
            expire_on_pool(&pool).await.is_empty(),
            "nessuna delle due e' matura: una non ha termine, l'altra ha un'ora davanti"
        );
        assert_eq!(stato_di(&pool, confirm).await, "awaiting_confirmation");
        assert_eq!(stato_di(&pool, non_ancora).await, "awaiting_confirmation");
    }

    /// La sweep guarda le SOSPENSIONI, non i run in esecuzione: un run che sta
    /// lavorando non viene chiuso nemmeno se porta una scadenza sulla riga
    /// (puo' portarla: e' stato sospeso e poi ha ripreso).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn un_run_ripreso_non_viene_chiuso_dalla_vecchia_scadenza(pool: PgPool) {
        let id = run_sospeso(&pool, "step_gate", Some(-60)).await;
        sqlx::query("UPDATE agent_runs SET status = 'running' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("il run ha ripreso");

        assert!(
            expire_on_pool(&pool).await.is_empty(),
            "il run sta lavorando: la scadenza di una sospensione gia' sciolta non lo chiude"
        );
        assert_eq!(stato_di(&pool, id).await, "running");
    }

    /// Il messaggio che la persona trova al mattino porta l'esito in CAMPI
    /// (regola Q), non solo nella prosa: `outcome` e `blocker` dal vocabolario
    /// ADR 0034, derivato dal kind della riga.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_messaggio_dichiara_l_esito_nei_campi(pool: PgPool) {
        let project_id = uuid::Uuid::new_v4();
        let session_id = crate::test_support::seed_chat_session(&pool, project_id).await;
        let run_id = crate::test_support::insert_agent_run(
            &pool,
            session_id,
            project_id,
            "awaiting_confirmation",
        )
        .await;
        let msg_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO chat_messages (id, session_id, project_id, role, content) \
             VALUES ($1, $2, $3, 'user', 'fai la migrazione')",
        )
        .bind(msg_id)
        .bind(session_id)
        .bind(project_id)
        .execute(&pool)
        .await
        .expect("messaggio utente");
        sqlx::query("UPDATE agent_runs SET run_message_id = $2 WHERE id = $1")
            .bind(run_id)
            .bind(msg_id)
            .execute(&pool)
            .await
            .expect("richiesta collegata");

        insert_blocked_message(
            &pool,
            run_id,
            Some(nexus_agent_graph::decisions::SuspensionOrigin::StepGate),
        )
        .await;

        let (contenuto, metadata): (String, serde_json::Value) = sqlx::query_as(
            "SELECT content, metadata FROM chat_messages \
             WHERE role = 'assistant' AND request_message_id = $1",
        )
        .bind(msg_id)
        .fetch_one(&pool)
        .await
        .expect("la risposta esiste: senza, la persona non scopre mai perche' il run e' fermo");

        assert_eq!(metadata["outcome"], "blocked");
        assert_eq!(
            metadata["blocker"], "safety",
            "il blocker si deriva dal kind, e per il gate duale e' safety"
        );
        assert_eq!(metadata["suspensionKind"], "step_gate");
        assert!(
            contenuto.contains("validatori"),
            "il testo dice la CAUSA, non solo che il tempo e' scaduto"
        );
    }

    /// Kind illeggibile (riga manomessa, o scritta da una versione che non
    /// conosceva questo vocabolario): il run si chiude lo stesso — la scadenza
    /// e' un fatto — ma nessun blocker viene INVENTATO. Il campo assente dice
    /// "non dichiarato" (regola Q).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn kind_illeggibile_non_produce_un_blocker_inventato(pool: PgPool) {
        let project_id = uuid::Uuid::new_v4();
        let session_id = crate::test_support::seed_chat_session(&pool, project_id).await;
        let run_id = crate::test_support::insert_agent_run(
            &pool,
            session_id,
            project_id,
            "awaiting_confirmation",
        )
        .await;
        let msg_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO chat_messages (id, session_id, project_id, role, content) \
             VALUES ($1, $2, $3, 'user', 'vai')",
        )
        .bind(msg_id)
        .bind(session_id)
        .bind(project_id)
        .execute(&pool)
        .await
        .expect("messaggio utente");
        sqlx::query("UPDATE agent_runs SET run_message_id = $2 WHERE id = $1")
            .bind(run_id)
            .bind(msg_id)
            .execute(&pool)
            .await
            .expect("richiesta collegata");

        insert_blocked_message(&pool, run_id, None).await;

        let metadata: serde_json::Value = sqlx::query_scalar(
            "SELECT metadata FROM chat_messages \
             WHERE role = 'assistant' AND request_message_id = $1",
        )
        .bind(msg_id)
        .fetch_one(&pool)
        .await
        .expect("la risposta esiste comunque");
        assert_eq!(metadata["outcome"], "blocked");
        assert!(
            metadata.get("blocker").is_none(),
            "senza un kind leggibile la causa non si dichiara: un safety inventato \
             attribuirebbe al gate duale una chiusura che nessuno ha accertato"
        );
    }
}
