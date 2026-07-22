//! Helper di TEST condivisi del crate `mcp-core`.
//!
//! L'attributo interno sotto dichiara cio' che `main.rs` gia' impone
//! (`#[cfg(test)] mod test_support;`): questo modulo esiste SOLO nei test. E'
//! anche il segnale che il gate qualita' legge per tenerlo fuori dal conteggio
//! del debito di produzione (`mcp_quality::scan::file_solo_di_test`).
//!
//! Due responsabilita', con la stessa radice (regola O: lo strumento deve
//! raggiungere il suo oggetto per la strada della produzione):
//!
//! 1. lo SCHEMA del dominio run/chat viene dalla migrazione reale
//!    ([`PROJECT_MIGRATOR`], ri-esportato da `nexus-test-schema`), coi seeder
//!    `seed_*` che riempiono i NOT NULL e rispettano le FK vere;
//! 2. le tabelle META non coperte da quel set (a partire da `ai_price_catalog`)
//!    restano fixture esplicite, ma definite UNA volta sola qui.
//!
//! Punto unico (regola L) dello schema di test della tabella `ai_price_catalog`.
//! Prima la definizione era duplicata tra il modulo `#[cfg(test)]` di
//! `orchestrator::model_selection` e quello di
//! `agent_graph_adapter::model_upscale_port`: due `CREATE TABLE` indipendenti che
//! dovevano restare identici a mano. La duplicazione ha gia' causato una
//! regressione (mig 0478: aggiunte le colonne media `supports_image_gen`,
//! `supports_audio_in`, `supports_audio_out`, `supports_video_gen` che il punto
//! unico `select_models_tierchain` ha iniziato a filtrare; solo una delle due
//! copie venne aggiornata, l'altra rimase obsoleta e i suoi `#[sqlx::test]`
//! fallivano con "column does not exist" -> fail-open `Ok(None)` -> panico).
//!
//! Con un solo helper, una nuova colonna del catalog si aggiunge QUI una volta
//! sola e ogni `#[sqlx::test]` del crate resta automaticamente allineato.

#![cfg(test)]

use sqlx::PgPool;
use uuid::Uuid;

/// Migrator del set `db/migrations/project`, ri-esportato dal punto unico
/// [`nexus_test_schema`] perche' i `#[sqlx::test]` di questo crate possano
/// scriverlo come `crate::test_support::PROJECT_MIGRATOR`.
///
/// Il perche' (regola O: lo strumento deve raggiungere il suo oggetto per la
/// stessa strada della produzione) sta nella doc del crate condiviso, insieme al
/// difetto misurato che l'ha resa necessaria.
pub(crate) use nexus_test_schema::PROJECT_MIGRATOR;

/// Semina una sessione chat (`chat_sessions`, mig project 0001) e ne ritorna l'id.
///
/// Serve perche' lo schema reale VINCOLA `agent_runs.session_id` con una FK verso
/// `chat_sessions(id)`: le fixture a mano, prive di FK, accettavano run orfani che
/// in produzione il DB rifiuta.
pub(crate) async fn seed_chat_session(pool: &PgPool, project_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO chat_sessions (id, project_id) VALUES ($1, $2)")
        .bind(id)
        .bind(project_id)
        .execute(pool)
        .await
        .expect("seed chat_sessions");
    id
}

/// Semina un run agentico completo (sessione + riga `agent_runs`) e ne ritorna
/// l'id. Riempie i NOT NULL reali (`session_id`, `project_id`, `user_id`) che le
/// fixture a mano ignoravano: `CREATE TABLE agent_runs (id UUID PRIMARY KEY)`
/// permetteva `INSERT INTO agent_runs (id)`, una riga impossibile in produzione.
pub(crate) async fn seed_agent_run(pool: &PgPool) -> Uuid {
    seed_agent_run_for_project(pool, Uuid::new_v4()).await
}

/// Variante di [`seed_agent_run`] con `project_id` esplicito, per i test che
/// devono correlare run e progetto (eventi, scoping per-progetto).
pub(crate) async fn seed_agent_run_for_project(pool: &PgPool, project_id: Uuid) -> Uuid {
    let session_id = seed_chat_session(pool, project_id).await;
    insert_agent_run(pool, session_id, project_id, "running").await
}

/// Inserisce un run su una sessione GIA' esistente, con lo `status` voluto.
/// Base comune dei seeder: riempie i NOT NULL reali e rispetta la FK
/// `agent_runs.session_id -> chat_sessions(id)`.
pub(crate) async fn insert_agent_run(
    pool: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    status: &str,
) -> Uuid {
    insert_agent_run_as(pool, session_id, project_id, Uuid::new_v4(), status).await
}

/// Variante di [`insert_agent_run`] con `user_id` esplicito, per i test che
/// verificano l'isolamento per utente (le letture filtrano su quella colonna).
pub(crate) async fn insert_agent_run_as(
    pool: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
    status: &str,
) -> Uuid {
    insert_agent_run_with_id(pool, Uuid::new_v4(), session_id, project_id, user_id, status).await
}

/// Variante di [`insert_agent_run_as`] con `run_id` IMPOSTO dal chiamante: serve
/// ai sub-run, che in produzione hanno la stessa identita' in `agent_runs` e in
/// `nexus_subagent_runs` (gli `agent_steps` del figlio si correlano su quell'id,
/// vincolato dalla FK verso `agent_runs`).
pub(crate) async fn insert_agent_run_with_id(
    pool: &PgPool,
    run_id: Uuid,
    session_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
    status: &str,
) -> Uuid {
    sqlx::query(
        "INSERT INTO agent_runs (id, session_id, project_id, user_id, status) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind(session_id)
    .bind(project_id)
    .bind(user_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("seed agent_runs");
    run_id
}

/// Semina il piano del run (`nexus_agent_plans`) con i soli NOT NULL richiesti.
///
/// E' il prerequisito dei todo: lo schema reale vincola
/// `nexus_agent_todos.run_id` con una FK verso `nexus_agent_plans(run_id)`, per
/// cui un todo senza piano - che entrambe le fixture a mano accettavano - in
/// produzione non puo' esistere.
pub(crate) async fn seed_plan(pool: &PgPool, run_id: Uuid, project_id: Uuid) {
    sqlx::query(
        "INSERT INTO nexus_agent_plans (run_id, project_id, thread_id, planner_model) \
         VALUES ($1, $2, $3, 'test-planner') \
         ON CONFLICT (run_id) DO NOTHING",
    )
    .bind(run_id)
    .bind(project_id)
    .bind(run_id.to_string())
    .execute(pool)
    .await
    .expect("seed nexus_agent_plans");
}

/// Semina un todo del piano e ne ritorna l'id. Crea il piano se manca (la FK
/// `nexus_agent_todos.run_id -> nexus_agent_plans(run_id)` lo esige) ed eredita
/// da esso il `project_id`, NOT NULL nello schema reale.
pub(crate) async fn seed_todo(pool: &PgPool, run_id: Uuid, seq: i32, status: &str) -> Uuid {
    seed_plan(pool, run_id, Uuid::new_v4()).await;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO nexus_agent_todos (id, run_id, project_id, seq, content, status) \
         SELECT $1, $2, p.project_id, $3, 'todo di test', $4 \
         FROM nexus_agent_plans p WHERE p.run_id = $2",
    )
    .bind(id)
    .bind(run_id)
    .bind(seq)
    .bind(status)
    .execute(pool)
    .await
    .expect("seed nexus_agent_todos");
    id
}

/// Crea la tabella `ai_price_catalog` con lo schema canonico usato dai
/// `#[sqlx::test]` del crate.
///
/// L'insieme delle colonne deve restare allineato a quelle lette dal punto unico
/// `crate::orchestrator::select_models_tierchain` (in particolare i media kind
/// della mig 0478, sempre referenziati nella WHERE per i purpose testuali). Una
/// colonna nuova nel catalog va aggiunta qui e basta: i call site delegano.
///
/// ATTENZIONE: e' uno SPECCHIO dello schema reale, tenuto allineato a mano. Se
/// diverge, i test girano su uno schema che non esiste piu' e il verde non dice
/// nulla. `performance_tier` e' NULLABLE e SENZA default dalla mig 0599 (il
/// `DEFAULT 'medium'` era il fallback magico che rendeva "non lo so" e "e' medium"
/// indistinguibili): questo specchio l'aveva ancora, e un test che seminava un
/// tier NULL falliva per un vincolo che in produzione non esiste piu'.
pub(crate) async fn create_ai_price_catalog_table(pool: &PgPool) {
    sqlx::query(
        "CREATE TABLE ai_price_catalog ( \
             provider TEXT NOT NULL, \
             model TEXT NOT NULL, \
             is_enabled BOOLEAN NOT NULL DEFAULT true, \
             supports_tool_use BOOLEAN NOT NULL DEFAULT true, \
             supports_vision BOOLEAN NOT NULL DEFAULT false, \
             supports_image_gen BOOLEAN NOT NULL DEFAULT false, \
             supports_audio_in BOOLEAN NOT NULL DEFAULT false, \
             supports_audio_out BOOLEAN NOT NULL DEFAULT false, \
             supports_video_gen BOOLEAN NOT NULL DEFAULT false, \
             agentic_thinking_policy TEXT NOT NULL DEFAULT 'none', \
             uses_thinking_mode BOOLEAN NOT NULL DEFAULT false, \
             performance_tier TEXT, \
             capabilities JSONB NOT NULL DEFAULT '[]', \
             context_window INTEGER NOT NULL DEFAULT 8192, \
             input_cost_per_million_tokens DOUBLE PRECISION NOT NULL DEFAULT 0, \
             output_cost_per_million_tokens DOUBLE PRECISION NOT NULL DEFAULT 0, \
             is_featured BOOLEAN NOT NULL DEFAULT false, \
             speed_tier TEXT NOT NULL DEFAULT 'medium', \
             consecutive_failures INT NOT NULL DEFAULT 0, \
             consecutive_tool_failures INT NOT NULL DEFAULT 0, \
             auto_disabled_reason TEXT, \
             updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
             qualification_state TEXT NOT NULL DEFAULT 'unqualified', \
             qualified_capabilities JSONB NOT NULL DEFAULT '[]', \
             qualification_expires_at TIMESTAMPTZ, \
             pricing_state TEXT NOT NULL DEFAULT 'priced' \
         )",
    )
    .execute(pool)
    .await
    .expect("create ai_price_catalog");
}
