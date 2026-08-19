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
//!    ([`PROJECT_MIGRATOR`], ri-esportato da `nexus-migrations-embedded`), coi seeder
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
/// [`nexus_migrations_embedded`] perche' i `#[sqlx::test]` di questo crate possano
/// scriverlo come `crate::test_support::PROJECT_MIGRATOR`.
///
/// Il perche' (regola O: lo strumento deve raggiungere il suo oggetto per la
/// stessa strada della produzione) sta nella doc del crate condiviso, insieme al
/// difetto misurato che l'ha resa necessaria.
pub(crate) use nexus_migrations_embedded::PROJECT_MIGRATOR;

/// Contesto tool minimale, senza infrastruttura: pool DB lazy mai contattato,
/// brain non connesso, root sul path passato.
///
/// Punto unico (regola L) del contesto usato dai test dei tool: prima viveva
/// dentro il `mod tests` di `agent_tools::dispatch`, quindi ogni altro modulo
/// che volesse esercitare un tool per la strada della produzione doveva
/// ricopiarne i quindici campi — e una copia che diverge sul campo sbagliato
/// (`can_write`, `write_scope`, `isolated_subrun`) non fallisce: prova un'altra
/// cosa restando verde.
///
/// ATTENZIONE al costo di una lettura DB su questo pool: `connect_lazy` verso
/// una porta chiusa non fallisce subito, consuma l'`acquire_timeout` (30s). Un
/// test che attraversa un percorso con letture di settings paga 30s ciascuna:
/// vanno scelti percorsi che decidono PRIMA di interrogare il DB.
pub(crate) fn ctx_di_tool_test(root: std::path::PathBuf) -> crate::agent_tools::AgentToolContext {
    use std::sync::Arc;
    let db = sqlx::PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test").expect("pool lazy");
    crate::agent_tools::AgentToolContext {
        core: nexus_agent_tools::ToolContextCore {
            root_path: root,
            user_id: Uuid::nil(),
            is_git_repo: false,
            can_write: true,
            project_id: Uuid::nil(),
            session_id: None,
            db: Arc::new(db.clone()),
            run_db: Arc::new(db.clone()),
            parent_run_id: None,
            run_id: None,
            long_running_patterns: Vec::new(),
            user_role: "admin".to_string(),
            is_nexus_operator: true,
            project_channels: Arc::new(dashmap::DashMap::new()),
            monitor_registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            hooks: Arc::new(nexus_agent_tools::context_core::NoopMutationHooks),
            embedder: Arc::new(nexus_agent_tools::context_core::NoopEmbedder),
            isolated_subrun: false,
            write_scope: Vec::new(),
        },
        playwright_channels: crate::playwright_live::new_channels(),
        neural: crate::orchestrator::NeuralCoreClient::disconnected_for_tests(),
        dependency_status: Arc::new(crate::task_watchdog::DependencyStatus::new()),
        port_registry: crate::port_registry::PortRegistryCache::empty_for_tests(db),
        parent_narration: None,
    }
}

/// Lo stesso contesto, ma su un pool VERO (quello di un `#[sqlx::test]`) e con
/// l'identita' del progetto e del run che la produzione gli darebbe.
///
/// Delega a [`ctx_di_tool_test`] invece di ricopiarne i quindici campi: una copia
/// che diverge sul campo sbagliato non fallisce, prova un'altra cosa restando
/// verde (regola O). Qui si sovrascrivono solo i quattro campi che un test con
/// DB deve poter scegliere.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn ctx_di_tool_test_su_db(
    root: std::path::PathBuf,
    pool: PgPool,
    project_id: Uuid,
    run_id: Option<Uuid>,
) -> crate::agent_tools::AgentToolContext {
    use std::sync::Arc;
    let mut ctx = ctx_di_tool_test(root);
    ctx.core.db = Arc::new(pool.clone());
    ctx.core.run_db = Arc::new(pool.clone());
    ctx.core.project_id = project_id;
    ctx.core.run_id = run_id;
    ctx.port_registry = crate::port_registry::PortRegistryCache::empty_for_tests(pool);
    ctx
}

/// Il DB dei run del progetto, sostituito da cio' che il test DICHIARA.
///
/// PUNTO UNICO del finto (regola L): la stessa risposta serve ai test di
/// ENTRAMBI i raccoglitori di allocazioni — il GC periodico e quello che gira
/// all'avvio di un servizio — e due finti divergenti darebbero due idee di
/// «run vivo» proprio dove il criterio e' condiviso.
///
/// Serve perche' `agent_runs` vive nel DB del PROGETTO e i `#[sqlx::test]`
/// girano su un META senza directory di routing: la porta di produzione
/// risponderebbe `NonInterrogabile`, che PRESERVA — cioe' il test sarebbe verde
/// per fail-closed e non per il criterio, il falso verde che la regola O vieta.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct RunFinto {
    vivi: Vec<Uuid>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl RunFinto {
    pub(crate) fn nessuno_vivo() -> Self {
        Self { vivi: Vec::new() }
    }
    pub(crate) fn con_vivo(run_id: Uuid) -> Self {
        Self { vivi: vec![run_id] }
    }
}

#[async_trait::async_trait]
impl crate::project_workspace::prenotazione_porta::VitaDelRun for RunFinto {
    async fn interroga(
        &self,
        _project_id: Uuid,
        run_id: Uuid,
    ) -> crate::project_workspace::prenotazione_porta::EsitoInterrogazioneRun {
        use crate::project_workspace::prenotazione_porta::EsitoInterrogazioneRun;
        if self.vivi.contains(&run_id) {
            EsitoInterrogazioneRun::Stato("running".to_string())
        } else {
            EsitoInterrogazioneRun::Stato("completed".to_string())
        }
    }
}

/// Semina un progetto sul DB META, con la catena di FK che lo schema reale
/// pretende: `teams` -> `users` -> `projects`.
///
// `seed_project_meta` NON e' piu' qui: la definizione vive in
// `nexus_test_preconditions` dal 2026-08-05, e si importa da li'
// (`use nexus_test_preconditions::seed_project_meta;`).
//
// Perche' si e' spostata IN BASSO invece di restare: i crate estratti da
// mcp-core — `nexus-prompt` e i prossimi — stanno SOTTO di lui nel grafo, e i
// loro test hanno bisogno dello stesso seeder. Tenerlo qui avrebbe lasciato
// una sola strada, duplicarlo; ed e' precisamente il difetto che il commento
// originale descriveva ("viveva ricopiata in almeno cinque `mod tests`"), con
// una copia in piu' invece che in meno.
//
// Non e' rimasto un `pub use` di cortesia: sarebbe stato un import inutilizzato
// finche' nessun test di QUESTO crate lo chiama, e un `allow(unused)` per
// tenerlo in vita e' debito che nessuno riscuote.

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

/// Semina un messaggio di chat (`chat_messages`, mig project 0001) e ne ritorna
/// l'id.
///
/// Serve per la stessa ragione di [`seed_chat_session`]: lo schema reale VINCOLA
/// `agent_runs.run_message_id` con una FK verso `chat_messages(id)`, quindi un
/// uuid inventato non e' un'ancora valida — un test che ne passasse uno
/// misurerebbe il rifiuto della FK invece di cio' che voleva provare.
pub(crate) async fn seed_chat_message(pool: &PgPool, session_id: Uuid, project_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO chat_messages (id, session_id, project_id, role, content)
         VALUES ($1, $2, $3, 'user', 'messaggio di prova')",
    )
    .bind(id)
    .bind(session_id)
    .bind(project_id)
    .execute(pool)
    .await
    .expect("seed chat_messages");
    id
}

/// Semina una memoria di sessione in `prompt_corrections` - una voce del pannello
/// "Memoria del progetto" - come la scrive `chat_sessions::compact_session_core`,
/// e ne ritorna l'id di riga.
///
/// Le colonne sono quelle della INSERT di produzione, `qdrant_point_id` compreso:
/// e' l'aggancio (UNIQUE) da cui dipende la contabilizzazione del recupero, e un
/// seeder che lo omettesse renderebbe verde un test su una riga irraggiungibile.
/// `retrieved_count` e `last_retrieved_at` NON si passano: restano al DEFAULT
/// dello schema (0 e NULL), che e' il punto di partenza da misurare.
pub(crate) async fn seed_memoria_di_sessione(
    pool: &PgPool,
    project_id: Uuid,
    session_id: Uuid,
    point_id: &str,
    text: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO prompt_corrections
            (id, project_id, session_id, intent, correction_text,
             normalized_hint_hash, qdrant_point_id, active, status, type)
        VALUES ($1, $2, $3, 'session_memory', $4, $5, $6, true, 'saved', 'session_memory')
        "#,
    )
    .bind(id)
    .bind(project_id)
    .bind(session_id)
    .bind(text)
    .bind(format!("session:{session_id}"))
    .bind(point_id)
    .execute(pool)
    .await
    .expect("seed prompt_corrections");
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

/// Semina un SUB-RUN con l'identita' che ha in produzione: la stessa riga in
/// `agent_runs` (un sub-agente e' un run a se') piu' la riga in
/// `nexus_subagent_runs` che ne dichiara la provenienza. Ritorna l'id del figlio.
///
/// Le due colonne di provenienza sono distinte e vanno seminate come tali:
/// `anchor` finisce in `parent_run_id` (ancora di famiglia per depth-chain e
/// cost-cap, che per i dispatch da tool vale la SESSIONE) e `dispatcher` in
/// `dispatcher_run_id` (il run che ha convocato il figlio, cioe' la parentela
/// run -> run letta da [`crate::run_lineage`]).
pub(crate) async fn seed_subagent_run(
    pool: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
    anchor: Uuid,
    dispatcher: Option<Uuid>,
    kind: &str,
) -> Uuid {
    let child = Uuid::new_v4();
    insert_agent_run_with_id(pool, child, session_id, project_id, user_id, "completed").await;
    sqlx::query(
        "INSERT INTO nexus_subagent_runs \
             (id, parent_run_id, dispatcher_run_id, project_id, kind, task_description, status) \
         VALUES ($1, $2, $3, $4, $5, 'task di test', 'completed')",
    )
    .bind(child)
    .bind(anchor)
    .bind(dispatcher)
    .bind(project_id)
    .bind(kind)
    .execute(pool)
    .await
    .expect("seed nexus_subagent_runs");
    child
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
/// SUPERSTITE VOLUTO (regola O): il grosso dei test che usavano questo specchio
/// e' stato convertito a `nexus_migrations_embedded::META_MIGRATOR` (schema
/// REALE della migrazione, non una copia a mano). Restano solo i test di
/// `agent_tools::subagent_native` che girano su
/// `#[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]`: hanno
/// bisogno di tabelle PROJECT vere (`agent_runs`, `chat_sessions`) sullo STESSO
/// pool dove servono anche `ai_price_catalog`/`nexus_purpose_model` (tabelle
/// META). `db/migrations` e `db/migrations/project` hanno numerazione
/// SOVRAPPOSTA (entrambe partono da 0001): applicare i due migrator sullo stesso
/// DB fa collidere le PK di `_sqlx_migrations`, quindi META_MIGRATOR non e'
/// un'opzione li' — non e' un caso rimasto indietro, e' un limite dei due set di
/// migrazioni che restano scritti per DB separati.
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
/// Schema di test della tabella META `settings`, allineato alla mig
/// `0002_settings.sql` (key/value/category/description/is_secret/updated_at).
///
/// Punto unico (regola L) di una fixture che viveva ricopiata QUINDICI volte in
/// DODICI `mod tests` del crate, e gia' divergente: sei colonne in `ui_flags`,
/// tre in `governance_telemetry` (che dichiara `category`), due altrove, e in
/// `native_engine` un `value TEXT` NULLABLE che in produzione e' `NOT NULL
/// DEFAULT ''`. Una fixture che diverge dallo schema non fallisce: prova
/// un'altra cosa restando verde (regola O).
///
/// Fixture e non migrator, per la stessa ragione documentata sopra per
/// [`create_ai_price_catalog_table`]: i test che ne hanno bisogno girano su un
/// DB vuoto o su `PROJECT_MIGRATOR`, dove `META_MIGRATOR` non e' applicabile
/// (numerazione sovrapposta dei due set). `IF NOT EXISTS` perche' chiamarla su
/// un pool gia' migrato sia un no-op invece di un errore.
///
/// PERCHE' TUTTE E SEI e non le sole colonne lette. Tre servono davvero:
/// `key`/`value` a `get_setting`, `is_secret` a `get_setting_public`,
/// `updated_at` all'upsert di [`seed_setting`]. `category` e `description` non
/// le legge nessun test — misurato, non supposto: la vecchia fixture di
/// `governance_telemetry` dichiarava `category` e nessun suo `INSERT` la
/// popolava. Restano perche' il criterio "solo cio' che si legge" ha un costo
/// nascosto: il giorno in cui un test comincia a leggerne una, la fixture va
/// estesa e il fallimento arriva come "column does not exist" invece che come
/// una riga rifiutata dallo schema vero (e' il modo in cui la mig 0478 ruppe le
/// due copie di `ai_price_catalog`). Sei colonne con DEFAULT non impongono nulla
/// ai chiamanti; una colonna mancante si', il giorno in cui serve.
pub(crate) async fn create_settings_table(pool: &PgPool) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS settings ( \
             key TEXT PRIMARY KEY, \
             value TEXT NOT NULL DEFAULT '', \
             category TEXT NOT NULL DEFAULT 'general', \
             description TEXT NOT NULL DEFAULT '', \
             is_secret BOOLEAN NOT NULL DEFAULT FALSE, \
             updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW() )",
    )
    .execute(pool)
    .await
    .expect("create table settings");
}

/// Scrive una coppia in `settings` e invalida la voce corrispondente della cache
/// di processo. Upsert: serve sia al primo seed sia al cambio di valore a meta'
/// test.
///
/// L'INVALIDAZIONE E' IL PUNTO, e sta qui e non nei chiamanti perche' e' cio'
/// che la produzione fa gia' accanto alla propria scrittura
/// (`nexus_auth::update_setting_value`). `get_setting` legge attraverso
/// `SETTINGS_CACHE`, un `LazyLock` di PROCESSO con TTL 60s chiavato su
/// `(pool_identity, key)`: un test che scrive con una query propria e poi rilegge
/// ottiene il valore precedente per un minuto, cioe' piu' della durata dell'intera
/// suite. Quattro moduli l'avevano gia' scoperto da soli e chiamavano
/// `invalidate_setting_cache` a mano attorno alle proprie scritture
/// (`runtime_health`, `native_engine`, `subagent_native`, `nexus-agent-tools::files`):
/// un rimedio che ogni autore deve RICORDARSI e' un rimedio che prima o poi
/// qualcuno dimentica — ed era gia' dimenticato in `agent_tools::testing`, che
/// azzera `agent.playwright.readiness_timeout_seconds` con un `UPDATE` nudo.
///
/// COSA NON E' (regola O: dichiarare da dove si guarda). Non protegge da una
/// collisione fra test DIVERSI: quella non e' raggiungibile per costruzione,
/// perche' `sqlx::test` non pesca da un pool di nomi riciclati ma deriva il nome
/// del database dal percorso del test — `_sqlx_test_<sha512(test_path)>`, in
/// `sqlx_core::testing::TestSupport::db_name` — quindi due test hanno sempre
/// identita' di pool distinte e non possono leggersi a vicenda i valori. Il caso
/// reale, e l'unico, e' DENTRO un test: una lettura che precede il seed, o un
/// secondo seed della stessa chiave.
pub(crate) async fn seed_setting(pool: &PgPool, key: &str, value: &str) {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ($1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .expect("seed settings");
    nexus_auth::invalidate_setting_cache(pool, key);
}

/// Piu' coppie in un colpo, ognuna con lo stesso contratto di [`seed_setting`]
/// (upsert + invalidazione). L'invalidazione e' per-chiave, quindi deve avvenire
/// per ognuna: un `INSERT` multi-riga ne invaliderebbe zero.
pub(crate) async fn seed_settings(pool: &PgPool, pairs: &[(&str, &str)]) {
    for (key, value) in pairs {
        seed_setting(pool, key, value).await;
    }
}

/// Tabella `settings` piu' una coppia: il caso piu' frequente in una riga sola.
pub(crate) async fn create_settings_table_with(pool: &PgPool, key: &str, value: &str) {
    create_settings_table(pool).await;
    seed_setting(pool, key, value).await;
}

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

// La copia di `create_settings_table_with` che stava qui e' stata assorbita
// dal punto unico piu' sopra (`create_settings_table` + `seed_setting`), che
// porta gia' l'invalidazione della cache per-chiave: due fixture per la stessa
// tabella erano il difetto che questo lavoro chiude.
/// Sostituisce PER INTERO il contenuto di un template, dichiarando al presidio
/// della mig 0744 che la perdita dei blocchi e' voluta. Ritorna le righe toccate.
///
/// Serve perche' dal 19/08/2026 un trigger rifiuta ogni scrittura su
/// `nexus_prompt_templates` che faccia sparire un blocco `<nome>...</nome>` senza
/// che qualcuno lo abbia dichiarato: e' il presidio contro le riscritture
/// integrali del blob, che nelle mig 0437/0438 hanno cancellato 23 blocchi da
/// tre prompt senza far fallire niente.
///
/// Un test che riscrive un template **e'** una perdita voluta — sta provando che
/// la configurazione viene dal DB e non dal binario (regola G), e per farlo deve
/// per forza sostituire il testo che il compositore legge. Quindi non si esenta
/// dal presidio: si dichiara, con la stessa forma che una migrazione dovrebbe
/// usare. La dichiarazione e' DERIVATA dal punto unico `nexus_prompt_blocchi`
/// interrogando la riga che si sta per sovrascrivere, mai da un elenco scritto a
/// mano qui: un elenco letterale invecchierebbe alla prima migrazione che
/// aggiunge un blocco a quel prompt, e il test rosseggerebbe per un motivo che
/// non c'entra nulla con cio' che misura.
///
/// PUNTO UNICO (regola L) della riscrittura integrale nei test: con la
/// dichiarazione ricopiata in ogni modulo, il primo che la dimentica trova un
/// errore del DB e la strada spontanea diventa spegnere il trigger.
pub async fn sostituisci_contenuto_template(pool: &PgPool, key: &str, nuovo: &str) -> u64 {
    let mut tx = pool.begin().await.expect("transazione per la dichiarazione");
    // `set_config(..., is_local = true)` vale per la sola transazione: la
    // dichiarazione non sopravvive a questa riscrittura.
    sqlx::query(
        "SELECT set_config('nexus.blocchi_rimossi', COALESCE((             SELECT array_to_string(nexus_prompt_blocchi(content), ',')                FROM nexus_prompt_templates WHERE key = $1), ''), true)",
    )
    .bind(key)
    .execute(&mut *tx)
    .await
    .expect("dichiarazione dei blocchi rimossi");

    let toccate = sqlx::query("UPDATE nexus_prompt_templates SET content = $2 WHERE key = $1")
        .bind(key)
        .bind(nuovo)
        .execute(&mut *tx)
        .await
        .expect("riscrittura del template")
        .rows_affected();
    tx.commit().await.expect("commit della riscrittura");
    toccate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La fixture e' provata per la strada della produzione (regola O): la
    /// lettura e' `nexus_auth::get_setting`, la stessa che chiama il codice sotto
    /// test, non una `SELECT` scritta qui — che confermerebbe solo che l'`INSERT`
    /// e' andato a segno, cioe' l'unica cosa che non era in dubbio.
    ///
    /// I due assert coprono i due modi in cui la cache di processo morde dentro
    /// un test, e MUTAZIONE per entrambi: togliendo
    /// `invalidate_setting_cache` da [`seed_setting`], il primo rosseggia con
    /// `None` (il "non c'e'" letto prima del seed resta valido per 60s) e il
    /// secondo con `Some("primo")` (il valore precedente sopravvive al nuovo
    /// seed).
    #[sqlx::test]
    async fn il_seed_e_visibile_alla_lettura_della_produzione(pool: PgPool) {
        const K: &str = "test.fixture.settings";
        create_settings_table(&pool).await;

        // Il fatto "chiave assente" viene memorizzato come qualunque altro.
        assert_eq!(nexus_auth::get_setting(&pool, K).await, None);

        seed_setting(&pool, K, "primo").await;
        assert_eq!(
            nexus_auth::get_setting(&pool, K).await.as_deref(),
            Some("primo"),
            "un seed dopo una lettura deve essere visibile: e' il caso che ha \
             costretto quattro moduli a invalidare a mano",
        );

        seed_setting(&pool, K, "secondo").await;
        assert_eq!(
            nexus_auth::get_setting(&pool, K).await.as_deref(),
            Some("secondo"),
            "un secondo seed della stessa chiave deve sostituire il primo",
        );
    }
}
