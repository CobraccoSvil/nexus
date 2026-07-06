//! Adapter del trait [`nexus_agent_graph::runtime::ports::ToolExecutor`].
//!
//! Implementa `ToolExecutor::execute` delegando:
//! - in [`ExecMode::Real`] al dispatch tool IN-PROCESS di mcp-core
//!   ([`crate::agent_tools::execute_agent_tool`]) costruendo l'`AgentToolContext`
//!   col PUNTO UNICO [`ToolRunnerService::build_ctx`]. ATTENZIONE: mcp-core *e'*
//!   il ToolRunner — NON ci si chiama via gRPC (sarebbe un loop di rete su se'
//!   stessi); si esegue la stessa funzione di dispatch del server gRPC, in
//!   processo. In Real i tool sono REALI (side-effect possibili sul progetto):
//!   questo path serve SOLO al primario / cutover, MAI allo shadow (lo shadow usa
//!   [`ExecMode::Replay`], zero side-effect);
//! - in [`ExecMode::Replay`] (modalita' shadow) RILEGGE il `tool_result` del run
//!   PRIMARIO da `agent_steps` (la fonte scritta da
//!   [`crate::agent_graph_adapter::agent_step_store::PgAgentStepStore`] in F2a),
//!   senza eseguire nulla.
//!
//! CORRELAZIONE REPLAY (nota F2a): `agent_steps` NON persiste il `tool_call_id`
//! (la riga porta `tool_name` + `tool_input` + `tool_result`, ordinati per
//! `step_index`). Quindi il Replay correla per `(tool_name, indice progressivo)`:
//! la N-esima chiamata a un dato tool del primario corrisponde alla N-esima riga
//! `agent_steps` con quel `tool_name` (ordinata per `step_index`). Il contatore
//! per-nome vive nell'adapter (interior mutability), avanzando a ogni `execute`
//! Replay. Se non c'e' la riga corrispondente -> [`PortError::ReplayMissing`].
//! NB: la fonte/allineamento Replay COMPLETI (selezione del primario, riconciliazione
//! ordine cross-iterazione) sono rifiniti in F3; qui c'e' la LETTURA BASE da
//! `agent_steps` per il run primario gia' noto allo shadow.
//!
//! ESITO STRUTTURATO (regola L): `is_error` e `exit_code` sono derivati dal testo
//! del risultato col PUNTO UNICO di mcp-core ([`crate::tool_runner_server::
//! tool_result_is_error`] / [`crate::tool_runner_server::extract_exit_code`]),
//! gli stessi usati dal path gRPC. L'`exit_code` fluisce INVARIATO nel
//! [`ToolOutcome`] (alimenta `routing::signals::tool_result_outcome_after`).
//!
//! GUASTO INFRA vs ERRORE APPLICATIVO (caso "gRPC-down -> degrada a executor",
//! WAVE 2.2: mcp-core NON scala il provider su un guasto infra):
//! - un guasto della COSTRUZIONE del ctx (sessione non risolvibile, DB down =
//!   ToolRunner non operativo) e' propagato come [`PortError::Tool`]: il chiamante
//!   (nodo) lo mappa a degrado, senza scalare provider;
//! - quando invece il tool produce un risultato APPLICATIVO (anche un errore col
//!   marker `\u{274C}`), il [`ToolOutcome`] ha `is_infrastructure=false` (il
//!   ToolRunner ha risposto). Nel dispatch IN-PROCESS attuale non c'e' un piano
//!   "tool eseguito ma con esito infra", quindi `is_infrastructure` resta `false`
//!   sugli `Ok`; il segnale infra viaggia via `PortError`. Il campo resta nel
//!   contratto per i piani Replay/F3 che potranno marcarlo.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{
    ExecMode, PortError, ToolCall, ToolExecutor, ToolOutcome,
};

use crate::agent_tools::execute_agent_tool;
use crate::tool_runner_server::{extract_exit_code, tool_result_is_error, ToolRunnerDeps, ToolRunnerService};

/// Adapter [`ToolExecutor`] -> dispatch tool IN-PROCESS (Real) + replay
/// `tool_result` da `agent_steps` (Replay).
pub struct ToolRunnerExecutorAdapter {
    /// Pool Postgres: rilettura Replay dei `tool_result` del primario (sempre
    /// presente; coincide con `deps.db` quando `deps` e' valorizzato).
    db: PgPool,
    /// Dipendenze del ToolRunner concreto (db, neural, channels...): servono SOLO
    /// al path Real (costruzione ctx + esecuzione tool). `None` su un adapter
    /// Replay-only (run shadow read-only / test): in tal caso una chiamata Real
    /// ritorna un infra-error coerente (ToolRunner non operativo su questo
    /// adapter), mai un side-effect.
    deps: Option<ToolRunnerDeps>,
    /// Sessione chat del run corrente: risolve project/root/permessi per il ctx.
    session_id: Uuid,
    /// Run primario di cui RILEGGERE i `tool_result` in Replay (= thread_id).
    /// `None` -> in Replay ogni chiamata e' `ReplayMissing` (nessun primario noto).
    primary_run_id: Option<Uuid>,
    /// Contatore per-tool_name delle chiamate Replay gia' consumate (interior
    /// mutability: `execute` prende `&self`). La N-esima chiamata a un tool legge
    /// la N-esima riga `agent_steps` con quel `tool_name` (ordine `step_index`).
    replay_cursor: Mutex<HashMap<String, i64>>,
    /// Override della root di lavoro per un SUB-RUN ISOLATO (FASE 2 orchestrazione:
    /// git worktree effimero proprio del sub-run). Campo IMMUTABILE del run,
    /// passato a `execute_real` -> `build_ctx_with_root`. `None` (default) -> il
    /// ctx usa la root del progetto risolta dalla sessione: comportamento
    /// invariato. In PR3 NESSUN call site passa `Some` (l'accensione e' PR4); il
    /// canale esiste per essere acceso senza toccare di nuovo l'adapter.
    working_root: Option<PathBuf>,
}

impl ToolRunnerExecutorAdapter {
    /// Costruisce l'adapter per il run corrente (path Real + Replay).
    ///
    /// - `session_id`: sessione chat (risolve il ctx in Real);
    /// - `primary_run_id`: run primario da cui rileggere i tool_result in Replay
    ///   (lo shadow lo riceve dall'orchestratore; in Real non serve);
    /// - `working_root`: override root del sub-run ISOLATO (FASE 2). `None`
    ///   (default per ogni run non isolato) -> ctx sulla root del progetto,
    ///   comportamento invariato. In PR3 tutti i call site passano `None`.
    pub fn new(
        deps: ToolRunnerDeps,
        session_id: Uuid,
        primary_run_id: Option<Uuid>,
        working_root: Option<PathBuf>,
    ) -> Self {
        Self {
            db: deps.db.clone(),
            deps: Some(deps),
            session_id,
            primary_run_id,
            replay_cursor: Mutex::new(HashMap::new()),
            working_root,
        }
    }

    /// Costruttore Replay-only (run shadow read-only): nessun `ToolRunnerDeps`,
    /// quindi nessun path Real possibile (zero side-effect by construction). Lo
    /// shadow non esegue mai tool reali — rilegge solo i tool_result del primario.
    /// Usato anche dai test del Replay (niente `ToolRunnerDeps` da fabbricare).
    ///
    /// Cablato in F4 (run shadow): in F3 il motore nativo costruisce SOLO il path
    /// Real (run primario, `new`); il path shadow read-only non e' ancora
    /// instradato, quindi questo costruttore ha per ora il solo call site nei
    /// test. `allow(dead_code)` mirato fuori dai test finche' F4 non lo cabla.
    #[cfg_attr(not(test), allow(dead_code))] // cablato in F4 (shadow): impl viva
    pub fn from_db_for_replay(db: PgPool, primary_run_id: Option<Uuid>) -> Self {
        Self {
            db,
            deps: None,
            session_id: Uuid::nil(),
            primary_run_id,
            replay_cursor: Mutex::new(HashMap::new()),
            // Replay-only: nessun path Real -> nessun ctx costruito -> l'override
            // root e' irrilevante (execute_real fallisce prima, deps assente).
            working_root: None,
        }
    }

    /// Pool Postgres (scorciatoia leggibile).
    fn db(&self) -> &PgPool {
        &self.db
    }

    /// Esegue REALMENTE il tool in processo (side-effect possibili) e mappa il
    /// risultato testuale nel [`ToolOutcome`] strutturato.
    async fn execute_real(&self, call: &ToolCall) -> Result<ToolOutcome, PortError> {
        // Un adapter Replay-only non puo' eseguire tool reali (deps assente): e'
        // un guasto INFRASTRUTTURALE coerente (ToolRunner non operativo qui),
        // mai un side-effect silenzioso.
        let deps = self.deps.as_ref().ok_or_else(|| {
            PortError::Tool("ToolExecutor Replay-only: esecuzione Real non disponibile".to_string())
        })?;
        // Ctx col PUNTO UNICO del server gRPC (stesso root/permessi/reindex). Un
        // fallimento qui (sessione non risolvibile, DB down) e' INFRASTRUTTURALE:
        // il ToolRunner non e' operativo -> is_infrastructure (degrada a executor,
        // niente scalata provider).
        let svc = ToolRunnerService::new(deps.clone());
        // PUNTO UNICO di costruzione ctx (regola L): con `working_root=None`
        // (default PR3) e' identico a `build_ctx(session_id)` — stessa root del
        // progetto, `isolated_subrun=false`. Con un override (PR4) il ctx punta al
        // worktree effimero del sub-run e sopprime autocommit/reindex.
        let ctx = svc
            .build_ctx_with_root(self.session_id, self.working_root.as_deref())
            .await
            .map_err(|status| PortError::Tool(format!("build_ctx fallita: {status}")))?;

        // Esecuzione IN-PROCESS: la STESSA funzione del dispatch gRPC, non una
        // chiamata di rete a se' stessi (regola: mcp-core E' il ToolRunner).
        let result = execute_agent_tool(&ctx, &call.name, &call.input).await;

        Ok(map_result_to_outcome(&call.id, result))
    }

    /// Rilegge il `tool_result` del primario da `agent_steps` correlando per
    /// `(tool_name, indice progressivo)`. Avanza il cursore per-nome e delega la
    /// query al PUNTO UNICO [`replay_tool_result`] (testabile in isolamento).
    async fn execute_replay(&self, call: &ToolCall) -> Result<ToolOutcome, PortError> {
        let run_id = self
            .primary_run_id
            .ok_or_else(|| PortError::ReplayMissing(format!("{}:nessun-primario", call.name)))?;

        // Offset = quante chiamate a questo tool sono gia' state replayate.
        let offset = {
            let mut cur = self.replay_cursor.lock().await;
            let n = cur.entry(call.name.clone()).or_insert(0);
            let off = *n;
            *n += 1;
            off
        };

        let text = replay_tool_result(self.db(), run_id, &call.name, offset).await?;
        Ok(map_result_to_outcome(&call.id, text))
    }
}

/// PUNTO UNICO della lettura Replay da `agent_steps`: la `offset`-esima riga
/// (ordine `step_index` ASC) con `tool_name` per il run primario. Ritorna il
/// `tool_result` (testo; `NULL` -> stringa vuota: blocco senza risultato
/// registrato, non un errore) oppure [`PortError::ReplayMissing`] se non c'e' una
/// riga corrispondente. Funzione libera cosi' i test la esercitano col solo
/// `&PgPool` (niente `ToolRunnerDeps` da fabbricare).
async fn replay_tool_result(
    db: &PgPool,
    primary_run_id: Uuid,
    tool_name: &str,
    offset: i64,
) -> Result<String, PortError> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT tool_result FROM agent_steps \
         WHERE run_id = $1 AND tool_name = $2 \
         ORDER BY step_index ASC \
         OFFSET $3 LIMIT 1",
    )
    .bind(primary_run_id)
    .bind(tool_name)
    .bind(offset)
    .fetch_optional(db)
    .await
    .map_err(|e| PortError::Tool(format!("replay lettura agent_steps: {e}")))?;

    match row {
        Some((tool_result,)) => Ok(tool_result.unwrap_or_default()),
        None => Err(PortError::ReplayMissing(format!("{tool_name}:#{offset}"))),
    }
}

#[async_trait]
impl ToolExecutor for ToolRunnerExecutorAdapter {
    async fn execute(&self, call: ToolCall, mode: ExecMode) -> Result<ToolOutcome, PortError> {
        match mode {
            ExecMode::Real => self.execute_real(&call).await,
            ExecMode::Replay => self.execute_replay(&call).await,
        }
    }
}

/// Mappa il risultato testuale di un tool (output di `execute_agent_tool` in Real
/// o `tool_result` riletto in Replay) nel [`ToolOutcome`] strutturato.
///
/// `is_error`/`exit_code` derivano dal PUNTO UNICO di mcp-core (stesso codice del
/// path gRPC). `content` resta il testo grezzo (i nodi lo trattano come opaco).
/// `is_infrastructure=false`: un errore applicativo del tool (marker `\u{274C}`)
/// NON e' un guasto infra (il ToolRunner ha risposto). L'infra-error e' segnalato
/// a monte (build_ctx fallita -> `PortError::Tool`, mappato dal chiamante).
fn map_result_to_outcome(tool_call_id: &str, result: String) -> ToolOutcome {
    let is_error = tool_result_is_error(&result);
    let exit_code = extract_exit_code(&result).map(|c| c as i64);
    ToolOutcome {
        tool_call_id: tool_call_id.to_string(),
        content: Value::String(result),
        is_error,
        exit_code,
        // Errore applicativo, non infrastrutturale: il tool ha prodotto un esito.
        is_infrastructure: false,
        error_class: if is_error {
            Some("tool_error".to_string())
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            input: json!({}),
            thought_signature: None,
        }
    }

    // ── map_result_to_outcome (punto unico esito) ─────────────────────────────

    #[test]
    fn esito_successo_con_exit_code_zero() {
        let out = map_result_to_outcome(
            "call_1",
            "hints\nEXIT CODE: 0\nSTDOUT:\nok\nSTDERR:\n".to_string(),
        );
        assert_eq!(out.tool_call_id, "call_1");
        assert!(!out.is_error);
        // exit_code STRUTTURATO estratto e propagato (alimenta tool_result_outcome_after).
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.is_infrastructure);
        assert_eq!(out.error_class, None);
    }

    #[test]
    fn esito_comando_fallito_exit_code_non_zero() {
        let out = map_result_to_outcome("c", "EXIT CODE: 1\nSTDERR:\nboom".to_string());
        // exit_code != 0: errore di comando, propagato strutturato.
        assert_eq!(out.exit_code, Some(1));
        // NB: un exit_code != 0 NON marca is_error (quello e' il marker U+274C):
        // l'esito di comando viaggia in exit_code, l'errore applicativo nel marker.
        assert!(!out.is_error);
    }

    #[test]
    fn esito_errore_applicativo_marker() {
        let out = map_result_to_outcome(
            "c",
            "\u{274C} Tool 'pippo' non esiste".to_string(),
        );
        assert!(out.is_error, "marker U+274C -> is_error");
        // tool non-comando: nessun exit_code.
        assert_eq!(out.exit_code, None);
        assert_eq!(out.error_class.as_deref(), Some("tool_error"));
        // errore APPLICATIVO, non infrastrutturale.
        assert!(!out.is_infrastructure);
    }

    #[test]
    fn esito_tool_non_comando_nessun_exit_code() {
        let out = map_result_to_outcome("c", "contenuto del file letto".to_string());
        assert!(!out.is_error);
        assert_eq!(out.exit_code, None, "tool non-comando -> exit_code None");
    }

    // ── Replay: lettura da agent_steps + correlazione per nome+ordine ─────────

    /// agent_runs minimale + agent_steps (mig 0009), come nel test di F2a.
    async fn create_tables(pool: &PgPool) {
        sqlx::query("CREATE TABLE agent_runs (id UUID PRIMARY KEY DEFAULT gen_random_uuid())")
            .execute(pool)
            .await
            .expect("create agent_runs");
        sqlx::query(
            "CREATE TABLE agent_steps ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 run_id UUID NOT NULL, \
                 step_index INT NOT NULL, \
                 tool_name TEXT NOT NULL, \
                 tool_input JSONB NOT NULL, \
                 tool_result TEXT, \
                 status TEXT NOT NULL DEFAULT 'running', \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_steps");
    }

    async fn insert_step(
        pool: &PgPool,
        run_id: Uuid,
        step_index: i32,
        tool_name: &str,
        tool_result: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO agent_steps \
             (id, run_id, step_index, tool_name, tool_input, tool_result, status) \
             VALUES (gen_random_uuid(), $1, $2, $3, '{}'::jsonb, $4, 'completed')",
        )
        .bind(run_id)
        .bind(step_index)
        .bind(tool_name)
        .bind(tool_result)
        .execute(pool)
        .await
        .expect("insert step");
    }

    async fn insert_run(pool: &PgPool) -> Uuid {
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_runs (id) VALUES ($1)")
            .bind(run)
            .execute(pool)
            .await
            .expect("run");
        run
    }

    // I test Replay esercitano `replay_tool_result` (punto unico della query) +
    // `map_result_to_outcome` (mapping esito) direttamente col solo `&PgPool`:
    // execute_real richiede un `ToolRunnerDeps` reale (ctx di progetto) ed e'
    // coperto a livello E2E in F3 quando run_via_native sara' cablato.

    #[sqlx::test]
    async fn replay_legge_tool_result_del_primario(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool).await;
        insert_step(&pool, run, 1000, "read_file", Some("contenuto A")).await;

        let text = replay_tool_result(&pool, run, "read_file", 0)
            .await
            .expect("replay ok");
        let out = map_result_to_outcome("x", text);
        assert_eq!(out.content, json!("contenuto A"));
        assert!(!out.is_error);
    }

    #[sqlx::test]
    async fn replay_correla_per_nome_e_ordine_progressivo(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool).await;
        // Due chiamate read_file (step 1000, 2000) + una list_files in mezzo (1500).
        insert_step(&pool, run, 1000, "read_file", Some("primo read")).await;
        insert_step(&pool, run, 1500, "list_files", Some("listing")).await;
        insert_step(&pool, run, 2000, "read_file", Some("secondo read")).await;

        // Cursore per-nome: read_file#0 -> step 1000, read_file#1 -> step 2000;
        // list_files#0 -> step 1500 (cursori indipendenti per nome).
        assert_eq!(
            replay_tool_result(&pool, run, "read_file", 0).await.unwrap(),
            "primo read"
        );
        assert_eq!(
            replay_tool_result(&pool, run, "list_files", 0).await.unwrap(),
            "listing"
        );
        assert_eq!(
            replay_tool_result(&pool, run, "read_file", 1).await.unwrap(),
            "secondo read"
        );
    }

    #[sqlx::test]
    async fn replay_missing_se_nessuna_riga(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool).await;
        insert_step(&pool, run, 1000, "read_file", Some("solo uno")).await;

        // offset 0 ok, offset 1 manca -> ReplayMissing.
        replay_tool_result(&pool, run, "read_file", 0).await.expect("ok");
        let err = replay_tool_result(&pool, run, "read_file", 1)
            .await
            .expect_err("deve mancare");
        assert!(matches!(err, PortError::ReplayMissing(_)));
    }

    #[sqlx::test]
    async fn replay_tool_result_null_e_contenuto_vuoto(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool).await;
        // step con tool_result NULL (blocco senza risultato registrato).
        insert_step(&pool, run, 1000, "read_file", None).await;

        let text = replay_tool_result(&pool, run, "read_file", 0)
            .await
            .expect("ok: NULL -> vuoto, non errore");
        let out = map_result_to_outcome("a", text);
        assert_eq!(out.content, json!(""));
        assert!(!out.is_error);
    }

    /// Avanzamento del cursore Replay end-to-end via `execute`: due read_file
    /// consecutive consumano gli step 1000 e 2000 nell'ordine progressivo, e la
    /// terza (senza riga) e' ReplayMissing con offset 2. Esercita la stessa logica
    /// del cursore per-nome usata in produzione. NB: usa la struct senza il path
    /// Real, quindi serve un `db` valido — lo iniettiamo via il costruttore di test
    /// `from_db_for_replay` (solo per i test, evita di fabbricare `ToolRunnerDeps`).
    #[sqlx::test]
    async fn replay_cursore_avanza_per_nome(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool).await;
        insert_step(&pool, run, 1000, "read_file", Some("uno")).await;
        insert_step(&pool, run, 2000, "read_file", Some("due")).await;

        let adapter = ToolRunnerExecutorAdapter::from_db_for_replay(pool.clone(), Some(run));
        let r1 = adapter
            .execute(call("read_file", "a"), ExecMode::Replay)
            .await
            .expect("step 1000");
        assert_eq!(r1.content, json!("uno"));
        let r2 = adapter
            .execute(call("read_file", "b"), ExecMode::Replay)
            .await
            .expect("step 2000 (cursore avanzato)");
        assert_eq!(r2.content, json!("due"));
        // 3a chiamata: cursore a 2, nessuna riga -> ReplayMissing #2.
        let e = adapter
            .execute(call("read_file", "c"), ExecMode::Replay)
            .await
            .expect_err("missing");
        assert!(matches!(&e, PortError::ReplayMissing(m) if m.ends_with("#2")));
    }

    #[sqlx::test]
    async fn real_su_replay_only_e_infra_error_senza_side_effect(pool: PgPool) {
        // Un adapter Replay-only (deps assente) NON deve eseguire tool reali: una
        // chiamata Real ritorna un PortError::Tool (guasto infra coerente), mai un
        // side-effect (zero esecuzioni). Garanzia by-construction per lo shadow.
        let adapter = ToolRunnerExecutorAdapter::from_db_for_replay(pool, None);
        let err = adapter
            .execute(call("run_command", "x"), ExecMode::Real)
            .await
            .expect_err("Real non disponibile su Replay-only");
        assert!(matches!(err, PortError::Tool(_)));
    }
}
