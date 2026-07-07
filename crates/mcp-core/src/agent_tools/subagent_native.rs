//! Orchestrazione NATIVA dei sub-agenti (porting Rust di `/agent/subagent-run`).
//!
//! Verso zero-Python: prima `dispatch_subagent` (in `nexus-agent-tools`) chiamava
//! l'endpoint REST `POST /agent/subagent-run` del brain Python, che eseguiva un
//! sub-run sul grafo agentico LangGraph. Ora il sub-run gira sul GRAFO NATIVO Rust
//! (`crate::native_engine::run_native`), in-process, senza alcuna dipendenza dal
//! brain.
//!
//! ## Perche' qui (gerarchia crate, regola L)
//!
//! Il grafo nativo (`native_engine`) vive in `mcp-core`; `nexus-agent-tools` e' una
//! DEP di `mcp-core` (mcp-core -> nexus-agent-tools, non viceversa), quindi
//! `subagent.rs` NON puo' chiamare `native_engine`. Il tool sub-agent viene percio'
//! INTERCETTATO qui in `mcp-core` (dove `run_native` e' chiamabile), prima di
//! delegare. E' la via piu' pulita data la gerarchia: l'orchestrazione del sub-run
//! sta dove vive il motore.
//!
//! ## Guard replicate dal brain (DB-driven, regola G)
//!
//! - `orchestrator.subagents_enabled`  (false -> tool ritorna disabilitato)
//! - `orchestrator.subagent_kinds_whitelist` (CSV dei kind ammessi)
//! - `orchestrator.subagent_max_depth` (rifiuta se depth supera il cap -> anti-ricorsione)
//! - `orchestrator.subagent_cost_cap_per_run_usd` (hard cap di spesa per parent)
//! - `orchestrator.subagent_default_timeout_s` (timeout se la definition non lo specifica)
//!
//! Le soglie sono lette dal DB (`settings`): niente fallback hardcoded sul
//! comportamento di business; i safe-default coincidono coi default del brain.
//!
//! ## Anti-ricorsione (depth DB-driven)
//!
//! Il proto del ToolRunner porta solo `session_id`, non il `run_id` corrente: il
//! `AgentToolContext` non sa se il run che invoca il tool e' gia' un sub-run. La
//! profondita' corrente e' percio' derivata dalla CATENA in `nexus_subagent_runs`:
//! il depth del nuovo sub-run e' `1 + max(depth)` tra i sub-run con stesso
//! `parent_anchor` ancora `running`. Cosi' un sub-agente che chiama un altro
//! sub-agente incrementa il depth e, al raggiungimento di `max_depth`, il dispatch
//! viene rifiutato: niente loop infinito di sub-agenti che si chiamano.

use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use super::AgentToolContext;
use crate::native_engine::{verdict_keys, NativeDeps, NativeRunInput, NativeRunOutcome};
use nexus_agent_graph::decisions::QuorumPolicy;

/// Marker d'errore (stesso contratto di `subagent.rs`: prefisso U+274C ->
/// `tool_result_is_error` deriva `is_error=true`).
fn err(msg: &str) -> String {
    format!("\u{274C} [dispatch_subagent] {msg}")
}

// Chiavi ricorrenti dei payload di narrazione/tool_result del sub-run: un solo
// literal per chiave (i consumatori — frontend e test — leggono le stesse).
const K_SUB_RUN_ID: &str = "subagent_run_id";
const K_SUB_KIND: &str = "subagent_kind";
const K_SUMMARY: &str = "summary";
const K_TIMEOUT_S: &str = "timeout_s";
const K_TARGET: &str = "target";
const K_IS_ERROR: &str = "is_error";
const K_PROVIDER: &str = "provider";
const K_MODEL: &str = "model";

/// Soglie sub-agent lette dal DB (regola G). I default coincidono coi default del
/// brain (`orchestrator_config` + mig 0153): safe-default se la chiave manca, MAI
/// magic fallback sul comportamento.
struct SubagentSettings {
    enabled: bool,
    whitelist: Vec<String>,
    max_depth: i64,
    cost_cap_usd: f64,
    default_timeout_s: i64,
    /// Narrazione del sub-run sul run PADRE (meta-step avvio/progresso/chiusura,
    /// mig 0535). Kill-switch UX: OFF -> chat muta durante il dispatch (storico).
    narration_enabled: bool,
    /// Heartbeat "al lavoro" nei silenzi del sub-run (secondi, mig 0535).
    /// 0 -> heartbeat disabilitato (restano avvio/progressi/chiusura).
    narration_heartbeat_s: i64,
}

async fn read_subagent_settings(ctx: &AgentToolContext) -> Result<SubagentSettings, String> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN (
            'orchestrator.subagents_enabled',
            'orchestrator.subagent_kinds_whitelist',
            'orchestrator.subagent_max_depth',
            'orchestrator.subagent_cost_cap_per_run_usd',
            'orchestrator.subagent_default_timeout_s',
            'orchestrator.subagent_narration_enabled',
            'orchestrator.subagent_narration_heartbeat_s'
        )",
    )
    .fetch_all(&*ctx.core.db)
    .await
    .map_err(|e| format!("query settings: {e}"))?;

    let mut s = SubagentSettings {
        enabled: false,
        whitelist: Vec::new(),
        max_depth: 2,
        cost_cap_usd: 5.0,
        default_timeout_s: 300,
        narration_enabled: true,
        narration_heartbeat_s: 20,
    };
    for row in rows {
        let k: String = row.get("key");
        let v: String = row.get("value");
        apply_subagent_setting(&mut s, &k, &v);
    }
    Ok(s)
}

/// Flag booleano nel formato accettato dalla tabella `settings`.
fn settings_flag(v: &str) -> bool {
    matches!(v.to_lowercase().as_str(), "true" | "1" | "yes" | "on")
}

/// Applica UNA riga di `settings` alle soglie sub-agent (chiave ignota: no-op).
fn apply_subagent_setting(s: &mut SubagentSettings, key: &str, value: &str) {
    let v = value.trim();
    match key {
        "orchestrator.subagents_enabled" => s.enabled = settings_flag(v),
        "orchestrator.subagent_kinds_whitelist" => {
            s.whitelist = v
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        }
        "orchestrator.subagent_max_depth" => s.max_depth = v.parse().unwrap_or(2),
        "orchestrator.subagent_cost_cap_per_run_usd" => s.cost_cap_usd = v.parse().unwrap_or(5.0),
        "orchestrator.subagent_default_timeout_s" => {
            s.default_timeout_s = v.parse().unwrap_or(300)
        }
        "orchestrator.subagent_narration_enabled" => s.narration_enabled = settings_flag(v),
        "orchestrator.subagent_narration_heartbeat_s" => {
            s.narration_heartbeat_s = v.parse().unwrap_or(20)
        }
        _ => {}
    }
}

/// Definition di un kind di sub-agent (`nexus_subagent_definitions`).
struct SubagentDefinition {
    prompt_key: String,
    tool_whitelist: Vec<String>,
    model_purpose: String,
    timeout_s: i64,
}

async fn fetch_definition(
    ctx: &AgentToolContext,
    kind: &str,
) -> Result<Option<SubagentDefinition>, String> {
    let row = sqlx::query(
        "SELECT prompt_key, tool_whitelist, model_purpose, timeout_s, is_enabled
         FROM nexus_subagent_definitions WHERE kind = $1 LIMIT 1",
    )
    .bind(kind)
    .fetch_optional(&*ctx.core.db)
    .await
    .map_err(|e| format!("query definition: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let is_enabled: bool = row.get::<bool, _>("is_enabled");
    if !is_enabled {
        return Err(format!("kind '{kind}' disabilitato"));
    }
    Ok(Some(SubagentDefinition {
        prompt_key: row.get::<String, _>("prompt_key"),
        tool_whitelist: row
            .try_get::<Vec<String>, _>("tool_whitelist")
            .unwrap_or_default(),
        model_purpose: row
            .try_get::<Option<String>, _>("model_purpose")
            .ok()
            .flatten()
            .unwrap_or_default(),
        timeout_s: row.get::<i32, _>("timeout_s") as i64,
    }))
}

/// Risolve il system_text del sub-agente dal registry prompt (`nexus_prompt_templates`,
/// stesso punto del brain `prompt_registry.get_prompt`). Vuoto -> errore (parita'
/// col brain: prompt mancante -> sub-run fallito).
async fn resolve_system_text(ctx: &AgentToolContext, prompt_key: &str) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = true",
    )
    .bind(prompt_key)
    .fetch_optional(&*ctx.core.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_default()
}

/// Costruisce l'array tools del sub-run filtrando lo schema REALE
/// (`AGENT_TOOLS_JSON`, fonte unica degli schema tool) sulla `tool_whitelist`
/// della definition. Migliora la fedelta' rispetto ai descrittori minimali che il
/// brain costruiva a mano (`_filter_tools_by_whitelist`): il modello vede lo schema
/// vero. Whitelist vuota -> nessun tool (sub-agente puramente conversazionale).
fn build_tools_json(whitelist: &[String]) -> Value {
    if whitelist.is_empty() {
        return json!([]);
    }
    let all: Value = serde_json::from_str(nexus_agent_tools::tool_schema::AGENT_TOOLS_JSON)
        .unwrap_or_else(|_| json!([]));
    let filtered: Vec<Value> = all
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|t| {
                    t.get("name")
                        .and_then(Value::as_str)
                        .map(|n| whitelist.iter().any(|w| w == n))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    json!(filtered)
}

/// Ancora del parent per la catena di sub-run: il run genitore se noto, altrimenti
/// la sessione (il ctx del tool NON porta il run_id corrente; per i sub-run di
/// primo livello l'ancora e' la sessione, come nel codice originale). `Uuid::nil`
/// solo se manca anche la sessione (caso degenere).
fn parent_anchor(ctx: &AgentToolContext) -> Uuid {
    ctx.core
        .parent_run_id
        .or(ctx.core.session_id)
        .unwrap_or_else(Uuid::nil)
}

/// Profondita' corrente DERIVATA dalla catena `nexus_subagent_runs` (anti-ricorsione,
/// punto unico). Il nuovo sub-run avra' `1 + max(depth)` tra i sub-run con lo stesso
/// `parent_anchor` ancora `running`. Nessun sub-run attivo -> il nuovo e' depth 1.
async fn current_chain_depth(pool: &sqlx::PgPool, anchor: Uuid) -> i64 {
    // pool: gia' instradato sul progetto dal chiamante (nexus_subagent_runs migrata).
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(depth)::bigint FROM nexus_subagent_runs \
         WHERE parent_run_id = $1 AND status = 'running'",
    )
    .bind(anchor)
    .fetch_one(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0)
}

/// Costo cumulativo gia' speso dai sub-run su questo `parent_anchor` (hard cap).
async fn cumulative_cost(pool: &sqlx::PgPool, anchor: Uuid) -> f64 {
    // pool: gia' instradato sul progetto dal chiamante (nexus_subagent_runs migrata).
    sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(SUM(cost_usd), 0)::double precision \
         FROM nexus_subagent_runs WHERE parent_run_id = $1",
    )
    .bind(anchor)
    .fetch_one(pool)
    .await
    .unwrap_or(0.0)
}

/// Handler del tool `dispatch_subagent` (singolo sub-run nativo).
pub async fn tool_dispatch_subagent(ctx: &AgentToolContext, input: &Value) -> String {
    let kind = match input.get("kind").and_then(Value::as_str) {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => return err("parametro 'kind' obbligatorio"),
    };
    let task = match input.get("task").and_then(Value::as_str) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => return err("parametro 'task' obbligatorio e non vuoto"),
    };
    let context_blob = input
        .get("context")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expected_format = input
        .get("expected_output_format")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    run_single_subagent(ctx, &kind, &task, &context_blob, &expected_format, None)
        .await
        .to_string()
}

/// Handler del tool `dispatch_subagents` (batch parallelo di sub-run nativi).
pub async fn tool_dispatch_subagents(ctx: &AgentToolContext, input: &Value) -> String {
    let tasks = match input.get("tasks").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return err("parametro 'tasks' (array non vuoto) obbligatorio"),
    };
    let batch_max_tasks = read_batch_max_tasks(ctx).await;
    if tasks.len() as u64 > batch_max_tasks {
        return err(&format!("troppi task in un batch (max {batch_max_tasks})"));
    }
    let configured_max = read_max_parallel_subagents(ctx).await;
    let max_parallel = input
        .get("max_parallel")
        .and_then(|v| v.as_u64())
        .unwrap_or(configured_max)
        .clamp(1, configured_max) as usize;

    let mut parsed: Vec<ParsedTask> = Vec::with_capacity(tasks.len());
    for (i, t) in tasks.iter().enumerate() {
        let kind = t
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let task = t
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if kind.is_empty() || task.trim().is_empty() {
            return err(&format!("task[{i}]: 'kind' e 'task' sono obbligatori"));
        }
        let context_blob = t
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let expected = t
            .get("expected_output_format")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // FASE 2: `write_scope` dichiarato dal task (aree file che il sub-run
        // scrive). Assente/vuoto -> `subtasks_are_disjoint` degradera' a false ->
        // ramo sequenziale (sicuro). Fonte: `Todo.write_scope` in `dispatch_wave`.
        let write_scope = write_scope_of(t);
        parsed.push(ParsedTask {
            kind,
            task,
            context_blob,
            expected,
            write_scope,
        });
    }

    // ── Gating isolamento (regola G + L): il ramo ISOLATO scatta SOLO se
    //    (1) il flag DB `orchestrator.subagent_isolation_enabled` e' ON (cache 60s)
    //        E la root del progetto e' isolabile (probe git fail-closed), E
    //    (2) i `write_scope` dei task sono banalmente disgiunti (funzione pura di
    //        PR1, punto unico). Altrimenti DEGRADA al ramo sequenziale/condiviso
    //        (identico a oggi). Con flag OFF (default) -> sempre sequenziale ->
    //        comportamento BIT-IDENTICO. ─────────────────────────────────────────
    //
    // DETERMINISMO/REPLAY: questo intero handler gira SOLO in `ExecMode::Real`. In
    // Replay/Shadow il `ToolExecutorAdapter` rilegge il tool_result di
    // `dispatch_subagents` da `agent_steps` (`execute_replay`) senza mai chiamare
    // `execute_real`: il ramo isolato (worktree/apply, side-effect Real-only) non
    // viene mai ricreato in shadow -> nessuna divergenza. L'apply e' l'unica fonte
    // del commit e avviene solo qui, in Real.
    let scopes: Vec<Vec<String>> = parsed.iter().map(|p| p.write_scope.clone()).collect();
    // I/O (flag DB + probe git) separato dalla decisione pura: il probe scatta solo
    // se il flag e' ON (compute_isolation_available corto-circuita), poi la
    // disgiunzione e' pura e testabile (should_isolate_batch).
    let isolation_available = compute_isolation_available(ctx).await;
    if should_isolate_batch(isolation_available, &scopes) {
        return run_batch_isolated(ctx, &parsed, max_parallel).await;
    }

    // ── Ramo sequenziale/condiviso (invariato) ────────────────────────────────
    // Esecuzione a ondate concorrenti (cap conservativo). I guard per-sub
    // (enabled/whitelist/depth/cost) sono valutati per ogni sub-run; nel batch
    // il cost cap e' best-effort (race tollerata dato il cap conservativo).
    let mut results: Vec<Value> = Vec::with_capacity(parsed.len());
    for wave in parsed.chunks(max_parallel) {
        let futs = wave.iter().map(|p| {
            run_single_subagent(ctx, &p.kind, &p.task, &p.context_blob, &p.expected, None)
        });
        let wave_res = futures::future::join_all(futs).await;
        results.extend(wave_res);
    }

    let ok = results.iter().filter(|r| r.get("error").is_none()).count();
    let mut out = json!({
        "count": results.len(),
        "ok": ok,
        "failed": results.len() - ok,
        "results": results,
    });
    // Fase C (coordinatore avversario, regola L/M): se il batch e' un PANEL di
    // review (almeno un sub-run ha dichiarato un `review`), compone il verdetto
    // aggregato dai segnali strutturati `outcome.review` — mai dalla prosa. I
    // sub-run non-review non hanno `review` e il panel non viene aggiunto. Le
    // review sono read-only (write_scope vuoto) -> restano nel ramo sequenziale.
    let outcomes: Vec<Value> = results
        .iter()
        .map(|r| r.get("outcome").cloned().unwrap_or(Value::Null))
        .collect();
    if let Some(panel) = nexus_agent_graph::decisions::compose_panel_verdict(
        &outcomes,
        &read_quorum_policy(ctx).await,
    ) {
        out["panel_verdict"] = panel.to_value();
    }
    out.to_string()
}

/// Un task del batch, parsato dall'input del tool. `write_scope` alimenta il gating
/// dell'isolamento fisico (FASE 2).
/// PUNTO UNICO (regola L) di estrazione del `write_scope` da un task del batch:
/// lista di prefissi file dichiarati dal task; assente/non-array -> vuoto (il
/// gating di isolamento degrada a sequenziale, ramo sicuro).
fn write_scope_of(t: &Value) -> Vec<String> {
    t.get("write_scope")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

struct ParsedTask {
    kind: String,
    task: String,
    context_blob: String,
    expected: String,
    write_scope: Vec<String>,
}

/// Chiave della setting kill-switch dell'isolamento fisico (regola G, opt-in).
const ISOLATION_ENABLED_SETTING: &str = "orchestrator.subagent_isolation_enabled";

/// TTL della cache del flag isolamento. Allineato ai 60s degli altri letti-da-DB
/// (routing matrix, capability, orchestration): stesso orizzonte di refresh.
const ISOLATION_FLAG_TTL_SECS: u64 = 60;

/// Cache 60s a livello processo del solo flag booleano `subagent_isolation_enabled`
/// (regola L: punto unico cache = `nexus_cache::TtlCache`; regola G: unica fonte
/// DB). Chiave costante: il flag e' globale (non per-progetto).
static ISOLATION_FLAG_CACHE: std::sync::OnceLock<nexus_cache::TtlCache<(), bool>> =
    std::sync::OnceLock::new();

fn isolation_flag_cache() -> &'static nexus_cache::TtlCache<(), bool> {
    ISOLATION_FLAG_CACHE
        .get_or_init(|| nexus_cache::TtlCache::new(std::time::Duration::from_secs(ISOLATION_FLAG_TTL_SECS)))
}

/// Legge il flag `orchestrator.subagent_isolation_enabled` (default false) con
/// cache 60s. DB down o chiave assente -> `false` (fail-safe: nessun isolamento,
/// ramo sequenziale come oggi).
/// PUNTO UNICO (regola L) della lettura del flag: lo condividono il batch tool
/// (`compute_isolation_available`) e il run-init del grafo
/// (`native_engine::compute_run_isolation_available`), cosi' il gate del planner e
/// l'esecuzione reale dell'isolamento vedono lo STESSO valore. Prende `&PgPool`
/// (unico bisogno reale): DB down o chiave assente -> `false` (fail-safe).
pub(crate) async fn isolation_flag_enabled(db: &sqlx::PgPool) -> bool {
    if let Some(v) = isolation_flag_cache().get(&()) {
        return v;
    }
    let enabled = nexus_auth::get_bool_setting(db, ISOLATION_ENABLED_SETTING)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    isolation_flag_cache().insert((), enabled);
    enabled
}

/// Isolamento DISPONIBILE per questo batch (regola G): flag DB ON (cache 60s) AND
/// la root del progetto e' un repo git in cui `git worktree` e' utilizzabile
/// (probe FAIL-CLOSED, punto unico `nexus_tool_kit::worktree::probe_isolatable`).
/// Corto-circuito: se `is_git_repo` (dalla sessione) e' false, niente probe git
/// (nessun I/O sul path caldo su progetti non-git). Flag OFF -> `false` senza
/// alcun probe.
async fn compute_isolation_available(ctx: &AgentToolContext) -> bool {
    if !isolation_flag_enabled(&ctx.core.db).await {
        return false;
    }
    if !ctx.core.is_git_repo {
        return false;
    }
    nexus_tool_kit::worktree::probe_isolatable(&ctx.core.root_path).await
}

/// Decisione PURA del gating isolamento (regola L + M, testabile senza I/O): il
/// ramo isolato scatta SOLO se l'isolamento e' disponibile (flag ON + root
/// isolabile, gia' risolto a monte) E i `write_scope` dei task sono banalmente
/// disgiunti (funzione pura di PR1, punto unico). Con `isolation_available=false`
/// (flag OFF, default) ritorna SEMPRE false -> ramo sequenziale -> bit-identico.
fn should_isolate_batch(isolation_available: bool, scopes: &[Vec<String>]) -> bool {
    isolation_available && nexus_agent_graph::decisions::subtasks_are_disjoint(scopes)
}

/// Esegue il batch di sub-run in ISOLAMENTO FISICO (FASE 2). Precondizione (gia'
/// verificata dal chiamante): flag ON, root isolabile, `write_scope` disgiunti.
///
/// Flusso (regola H, mai toppe):
///  1. GC best-effort dei worktree orfani del progetto (evita accumulo su crash).
///  2. `base = head_commit(root)` una volta per il batch (persistito per replay).
///  3. Per ogni task: crea un worktree effimero e prenota la row sub-run.
///  4. Esegue i sub-run IN PARALLELO (a ondate `max_parallel`), ciascuno con
///     `working_root = Some(worktree)` -> ctx isolato (autocommit/reindex soppressi).
///  5. Dopo che TUTTI sono terminati, APPLY SERIALIZZATO (un worktree alla volta,
///     mai concorrente: index/refs `.git` condivisi) via `apply_worktree_atomic`.
///     - Applied  -> raccoglie i file promossi per il reindex-once.
///     - Conflict -> quel sub-run e' marcato fallito (root intatta), gli altri
///                   proseguono (fail-loud, regola M: esito da `ApplyOutcome`).
///     - NoChanges-> nulla da promuovere.
///  6. Reindex UNA volta sui soli file realmente promossi alla root.
///  7. CLEANUP GARANTITO: `remove_worktree` per OGNI handle in OGNI esito (il
///     cleanup e' esplicito e async — mai Drop sincrono su runtime tokio, finding
///     del design; teardown idempotente e tollerante ai lock Windows).
async fn run_batch_isolated(
    ctx: &AgentToolContext,
    parsed: &[ParsedTask],
    max_parallel: usize,
) -> String {
    let root = ctx.core.root_path.clone();
    let project_id = ctx.core.project_id;
    let proj_pool = crate::project_db_routes::project_data_pool_from(&ctx.core.db, project_id).await;

    // (0) LOCK PER-ROOT CROSS-BATCH (regola H + L, punto unico in worktree.rs):
    //     serializza l'INTERA sezione che tocca l'area `.git`/worktree condivisa del
    //     progetto (GC -> creazione worktree -> ondate -> apply -> cleanup). Due batch
    //     isolati concorrenti sullo STESSO progetto (session distinte, stessa
    //     project_root: la guardia 409 e' per-session, non per-progetto) vengono
    //     serializzati qui. Chiude insieme D1 (merge/commit concorrenti sulla stessa
    //     .git -> index corrotto) e D2 (il GC di un batch che cancella i worktree in
    //     volo di un altro: quando il secondo batch ottiene il lock, il primo ha gia'
    //     applicato e ripulito i suoi worktree). Il guard e' `OwnedMutexGuard<()>`
    //     (Send, `'static`): tenerlo attraverso i `.await` delle ondate join_all e'
    //     corretto. Rilascio naturale a fine funzione (Drop).
    let _root_guard = nexus_tool_kit::worktree::lock_project_root(&root).await;

    // (1) GC orfani: bonifica i worktree residui di batch PRECEDENTI (crash / remove
    //     fallito). Preserva i worktree dei sub-run ancora `running` (batch
    //     concorrente sullo stesso progetto): passiamo come "attivi" i loro run_id
    //     dal DB, cosi' il GC non tocca risorse legittime (regola E). Best-effort.
    let active_run_ids = running_subagent_ids(&proj_pool, project_id).await;
    let removed = nexus_tool_kit::worktree::gc_orphan_worktrees(&root, &active_run_ids).await;
    if removed > 0 {
        tracing::info!(
            target: "mcp_core::subagent_native",
            removed,
            "isolamento: GC ha bonificato worktree orfani prima del batch"
        );
    }

    // (2) base commit del batch (una volta, riusato da ogni worktree e in replay).
    let base_commit = match nexus_tool_kit::worktree::head_commit(&root).await {
        Ok(b) => b,
        Err(e) => {
            // Impossibile risolvere HEAD: degrada al ramo sequenziale (mai fallire
            // l'intero batch per un problema di setup isolamento).
            tracing::warn!(
                target: "mcp_core::subagent_native",
                error = %e,
                "isolamento: head_commit fallito, degrado a sequenziale"
            );
            return run_batch_sequential(ctx, parsed, max_parallel).await;
        }
    };

    // (3) Crea un worktree per ogni task, PRIMA dell'esecuzione. Se la creazione di
    //     uno fallisce, degrada l'intero batch a sequenziale (cleanup di quelli gia'
    //     creati garantito) — mai un batch a isolamento parziale.
    let mut handles: Vec<nexus_tool_kit::worktree::WorktreeHandle> =
        Vec::with_capacity(parsed.len());
    for _ in parsed {
        let run_id = Uuid::new_v4();
        match nexus_tool_kit::worktree::create_ephemeral_worktree(&root, run_id, &base_commit).await
        {
            Ok(h) => handles.push(h),
            Err(e) => {
                tracing::warn!(
                    target: "mcp_core::subagent_native",
                    error = %e,
                    "isolamento: create_ephemeral_worktree fallito, degrado a sequenziale"
                );
                // Cleanup dei worktree gia' creati prima di degradare.
                for h in &handles {
                    let _ = nexus_tool_kit::worktree::remove_worktree(h).await;
                }
                return run_batch_sequential(ctx, parsed, max_parallel).await;
            }
        }
    }

    // (4) Esecuzione PARALLELA a ondate, ogni sub-run sul proprio worktree.
    let mut results: Vec<Value> = Vec::with_capacity(parsed.len());
    for wave in parsed
        .iter()
        .zip(handles.iter())
        .collect::<Vec<_>>()
        .chunks(max_parallel)
    {
        let futs = wave.iter().map(|(p, h)| {
            let slot = IsolationSlot {
                run_id: h.run_id,
                worktree_path: h.path.clone(),
                base_commit: h.base_commit.clone(),
            };
            async move {
                run_single_subagent(
                    ctx,
                    &p.kind,
                    &p.task,
                    &p.context_blob,
                    &p.expected,
                    Some(&slot),
                )
                .await
            }
        });
        let wave_res = futures::future::join_all(futs).await;
        results.extend(wave_res);
    }

    // (5) APPLY SERIALIZZATO (un worktree alla volta, mai concorrente) + raccolta
    //     dei file promossi per il reindex-once. Solo i sub-run RIUSCITI (nessun
    //     campo "error" nel result) vengono applicati; un sub-run fallito lascia il
    //     suo worktree scartato = rollback naturale.
    let mut promoted: Vec<String> = Vec::new();
    for (idx, (result, handle)) in results.iter_mut().zip(handles.iter()).enumerate() {
        if result.get("error").is_some() {
            continue; // sub-run fallito: niente apply (worktree scartato).
        }
        match nexus_tool_kit::worktree::apply_worktree_atomic(&root, handle).await {
            Ok(nexus_tool_kit::worktree::ApplyOutcome::Applied) => {
                promoted.extend(nexus_tool_kit::worktree::promoted_files(handle).await);
            }
            Ok(nexus_tool_kit::worktree::ApplyOutcome::NoChanges) => {}
            Ok(nexus_tool_kit::worktree::ApplyOutcome::Conflict { files }) => {
                // Esito strutturato (regola M): il sub-run e' fallito per conflitto,
                // la root e' intatta (merge --abort), gli altri proseguono.
                tracing::warn!(
                    target: "mcp_core::subagent_native",
                    task_index = idx,
                    conflicted_files = ?files,
                    "isolamento: apply in CONFLITTO, root intatta, sub-run marcato fallito"
                );
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("status".into(), json!("conflict"));
                    obj.insert(
                        "error".into(),
                        json!(format!("apply in conflitto su {} file", files.len())),
                    );
                    obj.insert("conflicted_files".into(), json!(files));
                }
            }
            Err(e) => {
                tracing::error!(
                    target: "mcp_core::subagent_native",
                    task_index = idx,
                    error = %e,
                    "isolamento: apply fallito (errore git), sub-run marcato fallito"
                );
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("status".into(), json!("apply_failed"));
                    obj.insert("error".into(), json!(format!("apply fallito: {e}")));
                }
            }
        }
    }

    // (6) CLEANUP GARANTITO di OGNI worktree, in OGNI esito (successo, conflitto,
    //     errore). Esplicito e async (mai Drop sincrono). Idempotente e tollerante
    //     ai lock Windows (remove --force nel modulo).
    for handle in &handles {
        if let Err(e) = nexus_tool_kit::worktree::remove_worktree(handle).await {
            tracing::warn!(
                target: "mcp_core::subagent_native",
                run_id = %handle.run_id,
                error = %e,
                "isolamento: cleanup worktree fallito (verra' ripreso dal GC al prossimo batch)"
            );
        }
    }

    // (7) Reindex UNA volta sui soli file promossi alla root (dedup dei path: piu'
    //     sub-run potrebbero aver toccato lo stesso file solo se non-disgiunti, che
    //     qui non accade, ma il dedup e' innocuo e barato).
    reindex_promoted_once(ctx, &promoted).await;

    let ok = results.iter().filter(|r| r.get("error").is_none()).count();
    json!({
        "count": results.len(),
        "ok": ok,
        "failed": results.len() - ok,
        "isolated": true,
        "promoted_files": promoted.len(),
        "results": results,
    })
    .to_string()
}

/// Ramo sequenziale/condiviso estratto per il degrado dall'isolamento (stessa
/// logica del ramo principale di `tool_dispatch_subagents`, punto unico). `None`
/// come `working_root` -> comportamento invariato.
async fn run_batch_sequential(
    ctx: &AgentToolContext,
    parsed: &[ParsedTask],
    max_parallel: usize,
) -> String {
    let mut results: Vec<Value> = Vec::with_capacity(parsed.len());
    for wave in parsed.chunks(max_parallel) {
        let futs = wave.iter().map(|p| {
            run_single_subagent(ctx, &p.kind, &p.task, &p.context_blob, &p.expected, None)
        });
        let wave_res = futures::future::join_all(futs).await;
        results.extend(wave_res);
    }
    let ok = results.iter().filter(|r| r.get("error").is_none()).count();
    json!({
        "count": results.len(),
        "ok": ok,
        "failed": results.len() - ok,
        "results": results,
    })
    .to_string()
}

/// Reindex UNA volta i file effettivamente promossi alla root dopo l'apply
/// serializzato (i sub-run isolati hanno il reindex per-scrittura SOPPRESSO in PR3).
/// Punto unico di reindex (regola L): `crate::projects::reindex_single_file`.
///
/// `promoted` (da `git diff --name-only base..branch`) INCLUDE anche i file
/// CANCELLATI dal sub-run. Per un file cancellato `reindex_single_file` farebbe
/// `read_to_string` -> Err scartato, lasciando i chunk vettoriali STANTII nell'indice
/// semantico. Per questo, prima di reindicizzare, si controlla l'esistenza del file
/// sul filesystem (segnale strutturato, regola M): se NON esiste, si cancellano i suoi
/// punti dal code index via il punto unico ESISTENTE
/// `vector_memory::delete_code_index_file_points` (regola L, stesso path relativo con
/// separatori '/' che usa `indexing.rs`); se esiste, comportamento invariato.
///
/// Best-effort: un fallimento del reindex/cleanup non e' un errore del batch.
async fn reindex_promoted_once(ctx: &AgentToolContext, promoted: &[String]) {
    if promoted.is_empty() {
        return;
    }
    // Dedup dei path (stabile, senza ordinare per non introdurre non-determinismo).
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let root = &ctx.core.root_path;
    for rel in promoted {
        if !seen.insert(rel.as_str()) {
            continue;
        }
        let target = root.join(rel);
        // File cancellato dal sub-run (promosso ma non piu' sul filesystem): purga i
        // chunk dall'indice invece di tentare un reindex che fallirebbe in read.
        match tokio::fs::try_exists(&target).await {
            Ok(false) => {
                let rel_slash = rel.replace('\\', "/");
                let _ = crate::vector_memory::delete_code_index_file_points(
                    &ctx.core.db,
                    ctx.core.project_id,
                    &rel_slash,
                )
                .await;
            }
            // Esiste (Ok(true)) o esito incerto (Err su try_exists): comportamento
            // invariato, reindex del file promosso.
            _ => {
                let _ = crate::projects::reindex_single_file(
                    &ctx.core.db,
                    &ctx.neural,
                    ctx.core.project_id,
                    root,
                    &target,
                )
                .await;
            }
        }
    }
    tracing::info!(
        target: "mcp_core::subagent_native",
        promoted = seen.len(),
        "isolamento: reindex-once dei file promossi completato"
    );
}

/// Run-id dei sub-run ancora `running` per il progetto (worktree potenzialmente
/// vivi di un batch concorrente). Usati come whitelist del GC filesystem: il GC
/// preserva le dir il cui run_id e' qui, rimuove le altre (regola E: mai toccare
/// risorse legittime di un altro batch). Best-effort: DB down -> lista vuota (il
/// GC diventa piu' aggressivo ma resta filtrato per la root del progetto).
async fn running_subagent_ids(pool: &sqlx::PgPool, project_id: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM nexus_subagent_runs WHERE project_id = $1 AND status = 'running'",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

/// Tetto di sicurezza per l'ampiezza dell'ondata concorrente di sub-agenti.
const MAX_PARALLEL_HARD_CAP: u64 = 8;

/// Legge `orchestrator.max_parallel_subagents` (default 3) come tetto effettivo del
/// parallelismo (PUNTO UNICO condiviso col pannello admin "Agenti Paralleli").
async fn read_max_parallel_subagents(ctx: &AgentToolContext) -> u64 {
    let v: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'orchestrator.max_parallel_subagents'",
    )
    .fetch_optional(&*ctx.core.db)
    .await
    .ok()
    .flatten();
    v.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(3)
        .clamp(1, MAX_PARALLEL_HARD_CAP)
}

/// Backstop assoluto al numero di task di UN batch `dispatch_subagents`: previene
/// che un valore di setting insensato faccia esplodere il numero di sub-run.
const BATCH_MAX_TASKS_HARD_CAP: u64 = 32;

/// Numero massimo di task in UN batch `dispatch_subagents` (regola G: DB-driven,
/// niente literal hardcoded). Default 8, clampato al backstop
/// [`BATCH_MAX_TASKS_HARD_CAP`]. Sostituisce il vecchio `tasks.len() > 8` fisso.
async fn read_batch_max_tasks(ctx: &AgentToolContext) -> u64 {
    let v: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'orchestrator.subagent_batch_max_tasks'",
    )
    .fetch_optional(&*ctx.core.db)
    .await
    .ok()
    .flatten();
    v.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(8)
        .clamp(1, BATCH_MAX_TASKS_HARD_CAP)
}

/// Policy del quorum del panel di review (Fase C, regola G: DB-driven, niente
/// hardcode). Safe-default coincidente con `QuorumPolicy::default` se le chiavi
/// mancano: 1 voto valido conclusivo, veto avversario su high-severity attivo.
async fn read_quorum_policy(ctx: &AgentToolContext) -> QuorumPolicy {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN (
            'orchestrator.review_quorum_min_valid',
            'orchestrator.review_fail_on_high_severity'
        )",
    )
    .fetch_all(&*ctx.core.db)
    .await
    .unwrap_or_default();
    let mut policy = QuorumPolicy::default();
    for row in rows {
        let k: String = row.get("key");
        let v: String = row.get("value");
        match k.as_str() {
            "orchestrator.review_quorum_min_valid" => {
                if let Ok(n) = v.trim().parse::<usize>() {
                    policy.min_valid_verdicts = n.max(1);
                }
            }
            "orchestrator.review_fail_on_high_severity" => {
                policy.fail_on_high_severity = settings_flag(v.trim());
            }
            _ => {}
        }
    }
    policy
}

// ─── Narrazione del sub-run sul run PADRE (ADR 0037) ───────────────────────
//
// Il dispatch sub-agente e' BLOCCANTE e puo' durare minuti: senza narrazione la
// chat del run padre resta muta (nessun meta-step, `updated_at` fermo) e il run
// sembra bloccato mentre il figlio lavora. Qui il tool — che conosce il canale
// SSE del run invocante via `ctx.parent_narration` — emette sul PADRE:
//   1. `subagent_started`  all'avvio (kind + task);
//   2. `subagent_progress` per ogni tool CONCLUSO del figlio (esito dal segnale
//      strutturato `AgentStepStatus`, regola M) + heartbeat nei silenzi;
//   3. `subagent_completed`/`subagent_failed` alla chiusura, col summary.
// Ogni step porta `correlation_id = subagent_run_id`. La composizione
// live+storico delega al PUNTO UNICO `emit_phase_meta_correlated` (regola L).

/// Compositore della narrazione sul run PADRE: sink SSE + store meta-step del
/// run invocante. `say` delega al punto unico `emit_phase_meta_correlated`.
/// Porta anche il PIN provider/model del sub-run (risolto dal model_purpose al
/// dispatch, vuoto = routing di default): ogni meta `subagent_*` lo espone nel
/// payload via [`ParentNarrator::with_pin`], cosi' il nastro frontend attribuisce
/// ai blocchi del figlio la SUA provenienza (non quella del padre).
struct ParentNarrator {
    sink: crate::agent_graph_adapter::event_sink::SseEventSinkAdapter,
    store: crate::agent_graph_adapter::meta_step_store::PgMetaStepStore,
    pin_provider: String,
    pin_model: String,
}

impl ParentNarrator {
    /// Costruisce il narratore per il run invocante. `None` se la narrazione e'
    /// disabilitata (setting) o il ctx non porta il canale (tool invocato fuori
    /// dal grafo nativo): il dispatch degrada al comportamento muto storico.
    /// `provider`/`model`: pin del sub-run quando risolto (vuoto = omesso).
    fn from_ctx(
        ctx: &AgentToolContext,
        proj_pool: &sqlx::PgPool,
        enabled: bool,
        provider: &str,
        model: &str,
    ) -> Option<std::sync::Arc<Self>> {
        if !enabled {
            return None;
        }
        let pn = ctx.parent_narration.as_ref()?;
        Some(std::sync::Arc::new(Self {
            sink: crate::agent_graph_adapter::event_sink::SseEventSinkAdapter::with_persistence(
                pn.step_tx.clone(),
                pn.run_id,
                pn.session_id,
                proj_pool.clone(),
            ),
            store: crate::agent_graph_adapter::meta_step_store::PgMetaStepStore::new(
                proj_pool.clone(),
                pn.run_id,
            ),
            pin_provider: provider.to_string(),
            pin_model: model.to_string(),
        }))
    }

    /// Arricchisce un payload `subagent_*` col pin provider/model del figlio
    /// (punto unico, delega a [`put_model_fields`]): campi presenti SOLO quando
    /// il pin e' risolto, mai inventati (il frontend degrada a '?').
    fn with_pin(&self, mut payload: Value) -> Value {
        put_model_fields(&mut payload, &self.pin_provider, &self.pin_model);
        payload
    }

    /// Emette (live) + persiste (storico) UN meta-step della narrazione,
    /// correlato al sub-run. Best-effort: mai un errore al chiamante. Il tool
    /// gira SOLO in `ExecMode::Real` (in Replay il tool_result e' riletto da
    /// `agent_steps` senza eseguire l'handler), quindi il mode e' fissato.
    async fn say(&self, kind: &str, title: String, payload: Value, subagent_run_id: Uuid) {
        nexus_agent_graph::nodes::emit_phase_meta_correlated(
            &self.sink,
            &self.store,
            nexus_agent_graph::runtime::ports::ExecMode::Real,
            kind,
            Some(subagent_run_id.to_string()),
            title,
            payload,
        )
        .await;
    }

    /// UN evento del sub-run sul ponte: memorizza il target dei ToolUse
    /// `Running` e narra i tool CONCLUSI (`concluded_tool_step`, regola M).
    /// Ritorna true se ha emesso un meta-step (azzera il silenzio heartbeat).
    async fn bridge_step(
        &self,
        targets: &mut std::collections::HashMap<u32, String>,
        ev: &crate::agent_types::AgentStepEvent,
        sub_kind: &str,
        sub_run_id: Uuid,
    ) -> bool {
        if let Some((idx, target)) = running_tool_target(ev) {
            targets.insert(idx, target);
        }
        let Some((is_error, tool, idx)) = concluded_tool_step(ev) else {
            return false;
        };
        let target = targets.remove(&idx);
        let (title, payload) =
            tool_progress_meta(sub_kind, sub_run_id, &tool, target.as_deref(), is_error);
        self.say("subagent_progress", title, self.with_pin(payload), sub_run_id)
            .await;
        true
    }

    /// Heartbeat "al lavoro" sul run padre, SOLO se il periodo e' trascorso in
    /// silenzio (nessun progresso emesso dall'ultimo tick).
    async fn maybe_heartbeat(
        &self,
        emitted_since_tick: bool,
        sub_kind: &str,
        sub_run_id: Uuid,
        started: &std::time::Instant,
    ) {
        if emitted_since_tick {
            return;
        }
        let elapsed = started.elapsed().as_secs();
        self.say(
            "subagent_progress",
            format!("Subagente {sub_kind}: al lavoro da {elapsed}s"),
            self.with_pin(json!({
                "phase": "working",
                "elapsed_s": elapsed,
                K_SUB_RUN_ID: sub_run_id.to_string(),
                K_SUB_KIND: sub_kind,
            })),
            sub_run_id,
        )
        .await;
    }
}

/// Arricchisce il payload di un meta `subagent_*` con provider/model del
/// figlio QUANDO NOTI (risoluzione dal model_purpose al dispatch; stringa
/// vuota = routing di default nel motore -> campo omesso, mai inventato).
/// PURA: il frontend (icona provenienza del nastro) legge questi campi.
fn put_model_fields(payload: &mut Value, provider: &str, model: &str) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    if !provider.trim().is_empty() {
        obj.insert(K_PROVIDER.into(), json!(provider));
    }
    if !model.trim().is_empty() {
        obj.insert(K_MODEL.into(), json!(model));
    }
}

/// Decisione PURA (regola M): uno step del sub-run va inoltrato al padre solo
/// se CONCLUSO — `Completed` (ok) o `Failed` (errore) dal segnale strutturato
/// `AgentStepStatus`, mai dal testo. `Running` e' escluso (doppione per tool);
/// uno step senza nome (ToolResult orfano) non produce narrazione utile.
fn concluded_tool_step(
    ev: &crate::agent_types::AgentStepEvent,
) -> Option<(bool, String, u32)> {
    let step = ev.step.as_ref()?;
    let is_error = match step.status {
        crate::agent_types::AgentStepStatus::Completed => false,
        crate::agent_types::AgentStepStatus::Failed => true,
        _ => return None,
    };
    if step.tool_name.is_empty() {
        return None;
    }
    Some((is_error, step.tool_name.clone(), step.step_index))
}

/// Target leggibile dallo step `Running` del sub-run: il ToolUse porta l'input
/// REALE in `tool_input.input` (il ToolResult no — vedi `SseEventSinkAdapter`),
/// quindi il ponte lo memorizza per `step_index` e lo riusa nel titolo alla
/// conclusione. Estrazione col punto unico del grafo (`tool_target_from_input`).
fn running_tool_target(ev: &crate::agent_types::AgentStepEvent) -> Option<(u32, String)> {
    let step = ev.step.as_ref()?;
    if step.status != crate::agent_types::AgentStepStatus::Running {
        return None;
    }
    let target = step
        .tool_input
        .get("input")
        .and_then(nexus_agent_graph::nodes::tool_target_from_input)?;
    Some((step.step_index, target))
}

/// Compone titolo + payload del meta-step `subagent_progress` per un tool
/// concluso del sub-run (PURA, testabile). Stesso registro della narrazione
/// tool del run corrente ("tool edit_file — src/x.ts" / "errore run_command").
fn tool_progress_meta(
    sub_kind: &str,
    subagent_run_id: Uuid,
    tool: &str,
    target: Option<&str>,
    is_error: bool,
) -> (String, Value) {
    let esito = if is_error { "errore" } else { "tool" };
    let title = match target {
        Some(t) => format!("subagente {sub_kind}: {esito} {tool} — {t}"),
        None => format!("subagente {sub_kind}: {esito} {tool}"),
    };
    let payload = json!({
        "phase": "tool",
        "tool": tool,
        K_TARGET: target,
        K_IS_ERROR: is_error,
        K_SUB_RUN_ID: subagent_run_id.to_string(),
        K_SUB_KIND: sub_kind,
    });
    (title, payload)
}

/// Ponte narrazione: consuma gli eventi SSE del SUB-run (prima scartati:
/// `_sub_rx`, la feature era muta) e li traduce in meta-step
/// `subagent_progress` sul run PADRE, con heartbeat nei silenzi (LLM call
/// lunghe senza tool). Termina alla chiusura del canale; il chiamante lo
/// aborta comunque prima del meta-step di chiusura.
fn spawn_narration_bridge(
    narrator: std::sync::Arc<ParentNarrator>,
    mut rx: tokio::sync::broadcast::Receiver<crate::agent_types::AgentStepEvent>,
    sub_run_id: Uuid,
    sub_kind: String,
    heartbeat_s: i64,
) -> tokio::task::JoinHandle<()> {
    use tokio::sync::broadcast::error::RecvError;
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let hb_enabled = heartbeat_s > 0;
        // Con heartbeat disabilitato il branch del tick resta spento (guard
        // `if hb_enabled`): il periodo placeholder non scatta mai.
        let period = if hb_enabled { heartbeat_s as u64 } else { 3600 };
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(period));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Il primo tick di `interval()` e' immediato: consumato subito, il
        // heartbeat parte dal primo periodo pieno.
        tick.tick().await;
        let mut targets: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut emitted_since_tick = false;
        loop {
            tokio::select! {
                recv = rx.recv() => {
                    // Canale chiuso = fine del sub-run. Lagged: il ponte e'
                    // narrazione best-effort, gli eventi persi non lo fermano.
                    if matches!(recv, Err(RecvError::Closed)) { break; }
                    let Ok(ev) = recv else { continue };
                    let said = narrator.bridge_step(&mut targets, &ev, &sub_kind, sub_run_id).await;
                    emitted_since_tick = emitted_since_tick || said;
                }
                _ = tick.tick(), if hb_enabled => {
                    narrator.maybe_heartbeat(emitted_since_tick, &sub_kind, sub_run_id, &started).await;
                    emitted_since_tick = false;
                }
            }
        }
    })
}

/// Slot di isolamento fisico di un sub-run (FASE 2): il worktree effimero
/// pre-creato dal batch e i metadati da persistire su `nexus_subagent_runs` per
/// audit/reconcile. `run_id` E' l'identita' del sub-run (row DB) E del worktree
/// (dir/branch): un solo id per entrambi, cosi' il GC filesystem (dir name =
/// run_id) e la row DB restano allineati.
struct IsolationSlot {
    /// Identita' condivisa row sub-run + worktree.
    run_id: Uuid,
    /// Path del worktree effimero (override root del sub-run).
    worktree_path: std::path::PathBuf,
    /// Commit da cui il worktree e' stato staccato (persistito per replay/reconcile).
    base_commit: String,
}

/// Esegue UNA sub-run sul GRAFO NATIVO. Ritorna sempre un `Value`: il sommario in
/// caso di successo, `{"error": "..."}` su guasto. Replica le guard del brain e
/// mappa l'esito del run nativo al tool_result atteso dal main.
///
/// `isolation`: `None` (default, ramo sequenziale/condiviso) -> il sub-run scrive
/// sulla root del progetto, il `run_id` e' generato dal DB (`DEFAULT`),
/// comportamento invariato. `Some(slot)` (ramo ISOLATO, valorizzato solo dal batch
/// parallelo di `tool_dispatch_subagents`) -> il sub-run usa `slot.run_id` come id,
/// scrive nel worktree `slot.worktree_path` (ctx isolato: autocommit/reindex
/// soppressi, PR3) e persiste `worktree_path`/`base_commit`. L'apply dei
/// cambiamenti alla root e' responsabilita' SERIALIZZATA del chiamante batch.
async fn run_single_subagent(
    ctx: &AgentToolContext,
    kind: &str,
    task: &str,
    context_blob: &str,
    expected_format: &str,
    isolation: Option<&IsolationSlot>,
) -> Value {
    let db = &*ctx.core.db;
    let project_id = ctx.core.project_id;
    let session_id = match ctx.core.session_id {
        Some(s) => s,
        None => return json!({"error": "sub-agent richiede una sessione chat (session_id assente)"}),
    };
    // Routing separazione DB: nexus_subagent_runs e' tabella migrata, vive nel DB
    // del progetto. Risolvo una volta il pool per_progetto e lo riuso per la catena
    // depth/costo, l'INSERT e le mark_run (a flag OFF ritorna il meta-DB).
    let proj_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;

    // ── Guard 1: settings (enabled / whitelist / depth / cost) ────────────────
    let settings = match read_subagent_settings(ctx).await {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("lettura settings fallita: {e}")}),
    };
    if !settings.enabled {
        return json!({"error": "sub-agents disabilitati (orchestrator.subagents_enabled=false)"});
    }
    if !settings.whitelist.iter().any(|w| w == kind) {
        return json!({"error": format!("kind '{kind}' non in whitelist: {:?}", settings.whitelist)});
    }

    // ── Guard 2: definition del kind ──────────────────────────────────────────
    let definition = match fetch_definition(ctx, kind).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return json!({"error": format!("kind '{kind}' non trovato in nexus_subagent_definitions")})
        }
        Err(e) => return json!({"error": e}),
    };

    // ── Guard 3: anti-ricorsione (depth DB-driven dalla catena) ───────────────
    let anchor = parent_anchor(ctx);
    let current_depth = current_chain_depth(&proj_pool, anchor).await + 1;
    if current_depth > settings.max_depth {
        return json!({"error": format!(
            "depth {current_depth} > max {}: annidamento sub-agent eccessivo (anti-ricorsione)",
            settings.max_depth
        )});
    }

    // ── Guard 4: cost cap cumulativo per parent ───────────────────────────────
    let spent = cumulative_cost(&proj_pool, anchor).await;
    if spent >= settings.cost_cap_usd {
        return json!({"error": format!(
            "cost cap raggiunto per parent={anchor} ({spent:.4} >= {:.4})",
            settings.cost_cap_usd
        )});
    }

    // ── Risoluzione system_text + tools + modello worker (DB-driven) ──────────
    let system_text = resolve_system_text(ctx, &definition.prompt_key).await;
    if system_text.trim().is_empty() {
        return json!({"error": format!("prompt '{}' non trovato o vuoto", definition.prompt_key)});
    }
    let tools_json = build_tools_json(&definition.tool_whitelist);

    let (provider, model) =
        resolve_worker_model(db, &proj_pool, kind, &definition, anchor, session_id).await;

    let timeout_s = if definition.timeout_s > 0 {
        definition.timeout_s
    } else {
        settings.default_timeout_s
    };

    // Embedding del context/expected nel task (parita' col brain `run_subagent`).
    let mut initial_msg = task.trim().to_string();
    if !context_blob.trim().is_empty() {
        initial_msg.push_str("\n\n## Contesto aggiuntivo\n");
        initial_msg.push_str(context_blob.trim());
    }
    if !expected_format.trim().is_empty() {
        initial_msg.push_str("\n\n## Formato output atteso\n");
        initial_msg.push_str(expected_format.trim());
    }

    // ── Crea row in nexus_subagent_runs (status='running' subito: la catena depth
    //    si basa sui 'running'; il sub-run e' bloccante, non c'e' fase 'pending'
    //    osservabile). ────────────────────────────────────────────────────────────
    let insert = NewSubagentRun {
        anchor,
        project_id,
        kind,
        task,
        context_blob,
        expected_format,
        depth: current_depth as i32,
    };
    let subagent_run_id: Uuid = match insert_subagent_run(&proj_pool, &insert, isolation).await {
        Ok(id) => id,
        Err(e) => return json!({"error": format!("creazione riga nexus_subagent_runs fallita: {e}")}),
    };

    // ── Riga agent_runs del SUB-run (osservabilita', regola M) ────────────────
    // Il sub-run gira sul grafo nativo con run_id=subagent_run_id ma non aveva
    // alcuna riga in agent_runs: il guard "untracked_run" di PgAgentStepStore
    // scartava in SILENZIO ogni tool_result del figlio (agent_steps vuoto ->
    // errori dei tool illeggibili, incidente 2026-07-06). La riga rende il
    // figlio un run TRACCIATO: step (input+result+esito) persistiti e
    // ispezionabili. `nexus_agent_type='subagent'` e' il discriminatore per le
    // query UI top-level (active-run, timeline, guard 409); `parent_run_id` e'
    // valorizzato solo se l'ancora e' un run tracciato (FK su agent_runs).
    ensure_child_agent_run(
        &proj_pool,
        subagent_run_id,
        session_id,
        project_id,
        ctx.core.user_id,
        anchor,
        &provider,
        &model,
    )
    .await;

    // ── NARRAZIONE sul run PADRE (ADR 0037): avvio ────────────────────────────
    // Il dispatch e' bloccante e puo' durare minuti: senza questi meta-step la
    // chat resta muta e il run padre sembra bloccato mentre il figlio lavora.
    let narrator =
        ParentNarrator::from_ctx(ctx, &proj_pool, settings.narration_enabled, &provider, &model);
    if let Some(n) = &narrator {
        let task_head: String = task.trim().chars().take(160).collect();
        n.say(
            "subagent_started",
            format!("Subagente {kind} avviato — {task_head}"),
            n.with_pin(json!({
                K_SUB_RUN_ID: subagent_run_id.to_string(),
                K_SUB_KIND: kind,
                "task": task_head,
                "depth": current_depth,
                K_TIMEOUT_S: timeout_s,
                "isolated": isolation.is_some(),
            })),
            subagent_run_id,
        )
        .await;
    }

    // ── Esecuzione sul GRAFO NATIVO (in-process, niente brain) ────────────────
    // Il sub-run e' un run a se': run_id = subagent_run_id (= thread del grafo),
    // STESSA session_id del parent (eredita root/permessi/canali). Lo stato porta
    // parent_run_id + subagent_depth -> il grafo applica i guard di annidamento
    // (UnderstandingNode salta il fan-out explore se depth>=1).
    let deps = build_native_deps_for_tool(ctx).await;
    // Canale SSE proprio del sub-run: NON instrada al frontend (l'output utente
    // resta quello del main, che riceve solo il summary). Il receiver alimenta
    // il PONTE narrazione verso il padre (prima era scartato: feature muta);
    // senza narratore il receiver e' droppato e il comportamento e' lo storico.
    let (sub_tx, sub_rx) = tokio::sync::broadcast::channel(256);
    let bridge = narrator.as_ref().map(|n| {
        spawn_narration_bridge(
            n.clone(),
            sub_rx,
            subagent_run_id,
            kind.to_string(),
            settings.narration_heartbeat_s,
        )
    });

    let native_input = NativeRunInput {
        run_id: subagent_run_id,
        session_id,
        provider,
        model,
        system_text,
        initial_msg,
        // Sub-run isolato: NIENTE history del main (parita' col brain `run_subagent`,
        // che parte da messages=[Human(task)]).
        conversation_history: Vec::new(),
        tools_json,
        // Sub-agente auto-approvato e DIRETTO: nessun intent_hint, nessun giudizio
        // del classifier (il task e' gia' descritto). Il RouterNode decide.
        intent_hint: None,
        requires_tools: None,
        agentic_score: None,
        authorizes_changes: None,
        classifier_resolved: false,
        action_oriented_min_score: crate::intent_classifier::DEFAULT_ACTION_ORIENTED_MIN_SCORE,
        // Sub-agente eseguito in autonomia (parita' col brain: behavior_mode
        // "automatico", approved=true).
        automation_mode: "automatic".to_string(),
        step_tx: sub_tx,
        parent_run_id: Some(anchor),
        subagent_depth: Some(current_depth),
        // FASE 2: override root del sub-run. `None` (ramo sequenziale/condiviso) ->
        // scrive sulla root del progetto, comportamento invariato. `Some(worktree)`
        // (ramo ISOLATO, valorizzato dal batch parallelo di
        // `tool_dispatch_subagents`) -> scrive nel worktree effimero, ctx isolato
        // (autocommit/reindex soppressi, PR3). L'apply serializzato e' del batch.
        working_root: isolation.map(|s| s.worktree_path.clone()),
    };

    // Timeout duro sull'esecuzione del sub-run (parita' col brain `asyncio.wait_for`).
    // In OGNI ramo il ponte e' fermato e ATTESO (`stop_bridge`) prima del meta-step
    // di chiusura: senza l'await, abort() e' cooperativo (cancella al prossimo poll)
    // e un progress/heartbeat gia' dentro persist_meta_step potrebbe ottenere un
    // NOW() > del completed -> ordine invertito nella timeline storica (ordinata
    // per created_at). L'await del handle garantisce che nessuna INSERT del ponte
    // segua quella di chiusura (race chiusa alla radice, regola H).
    let run_fut = crate::native_engine::run_native(&deps, &native_input);
    let outcome: anyhow::Result<NativeRunOutcome> =
        match tokio::time::timeout(std::time::Duration::from_secs(timeout_s as u64), run_fut).await {
            Ok(res) => res,
            Err(_) => {
                stop_bridge(bridge).await;
                return finalize_timeout(&proj_pool, narrator.as_deref(), subagent_run_id, kind, timeout_s).await;
            }
        };
    stop_bridge(bridge).await;

    match outcome {
        Ok(o) => {
            finalize_success(&proj_pool, narrator.as_deref(), subagent_run_id, kind, current_depth, &o)
                .await
        }
        Err(e) => finalize_failure(&proj_pool, narrator.as_deref(), subagent_run_id, kind, &e).await,
    }
}

/// Modello del worker dal `model_purpose` della definition (regola G,
/// tier-aware). Non risolto -> nessun override: l'executor usa il routing di
/// default (parita' col brain).
///
/// VINCOLO GIUDICE != WORKER (Fase C2): per un sub-run di `kind == "review"` la
/// risoluzione ESCLUDE il provider del run PADRE (il worker che ha prodotto il
/// lavoro), cosi' la verifica avversaria gira su un provider diverso (indipendenza
/// reale). Se l'esclusione svuota il pool (unico provider capable) si ripiega
/// SENZA esclusione con un WARN: il vincolo e' una preferenza forte, non un hard
/// filter — meglio un review sullo stesso provider che nessun review.
///
/// PIN DEL PROVIDER (propagazione all'utente): per i kind WORKER (NON review) se
/// l'utente ha pinnato un provider sulla chat (`chat_sessions.preferred_provider`)
/// la risoluzione del purpose viene RISTRETTA a quel provider (preferenza-forte
/// tier-aware: tier + capability + tool_use invariati, solo il provider e'
/// vincolato — regola L: unica sorgente del vincolo, NON si propaga il
/// preferred_model, il modello si deriva sempre dal tier via catalog, regola G).
/// Se il provider pinnato non offre un modello capable del tier (o e' in cooldown)
/// si RIPIEGA sulla risoluzione SENZA pin, con un WARN strutturato: il figlio non
/// resta bloccato (preferenza forte, non hard filter). Per `kind == "review"` il
/// pin e' IGNORATO: il vincolo giudice != worker vince (indipendenza avversaria).
///
/// SEPARAZIONE DB: `db` e' il pool META (purpose-resolution via catalog +
/// `parent_provider` da `agent_runs`); `proj_pool` e' il pool del PROGETTO, dove
/// vive `chat_sessions` (il pin). Le due letture NON possono condividere lo stesso
/// pool (incidente separazione DB: `chat_sessions` non e' nel meta).
async fn resolve_worker_model(
    db: &sqlx::PgPool,
    proj_pool: &sqlx::PgPool,
    kind: &str,
    definition: &SubagentDefinition,
    anchor: Uuid,
    session_id: Uuid,
) -> (String, String) {
    if definition.model_purpose.trim().is_empty() {
        return (String::new(), String::new());
    }
    let purpose = &definition.model_purpose;

    // Ramo REVIEW: pin IGNORATO, vincolo giudice != worker invariato. Provider del
    // padre da escludere (astensione da auto-certificazione).
    if kind == "review" {
        let exclude: Vec<String> = parent_provider(db, anchor).await.into_iter().collect();
        return resolve_review_model(db, purpose, kind, &exclude).await;
    }

    // Ramo WORKER: se l'utente ha pinnato un provider sulla chat, prova a risolvere
    // il purpose RISTRETTO a quel provider (preferenza-forte tier-aware).
    if let Some(pinned) = session_pinned_provider(proj_pool, session_id).await {
        match crate::internal_routing::resolve_purpose_model_db_pinned(db, purpose, Some(&pinned))
            .await
        {
            crate::internal_routing::PurposeResolution::Resolved {
                provider, model, ..
            } => return (provider, model),
            // NoCapableModel/NotFound col pin: il provider pinnato non offre un
            // modello capable del tier (o e' in cooldown). FALLBACK senza pin
            // (regola H fail-loud: il figlio non resta bloccato). WARN strutturato
            // (regola M: la decisione e' sull'enum PurposeResolution, il testo e'
            // solo display).
            other => {
                tracing::warn!(
                    kind = %kind,
                    purpose = %purpose,
                    pinned = %pinned,
                    resolution = ?other,
                    "subagent_native: pin provider non risolvibile per il tier del purpose, \
                     fallback senza pin"
                );
            }
        }
    }

    // Nessun pin (o pin degradato): risoluzione worker standard (parita' col
    // comportamento pre-pin).
    match crate::internal_routing::resolve_purpose_model_db(db, purpose).await {
        crate::internal_routing::PurposeResolution::Resolved {
            provider, model, ..
        } => (provider, model),
        other => {
            tracing::warn!(
                kind = %kind,
                model_purpose = %purpose,
                resolution = ?other,
                "subagent_native: model_purpose non risolto, routing di default"
            );
            (String::new(), String::new())
        }
    }
}

/// Risoluzione del modello per un sub-run di `kind == "review"`: ESCLUDE il
/// provider del padre (`exclude`), con fallback SENZA esclusione + WARN se il pool
/// si svuota (unico provider capable). Estratto da `resolve_worker_model` per
/// tenere il ramo review distinto dal ramo pin-worker (leggibilita'); la logica
/// del vincolo giudice != worker e' invariata.
async fn resolve_review_model(
    db: &sqlx::PgPool,
    purpose: &str,
    kind: &str,
    exclude: &[String],
) -> (String, String) {
    match crate::internal_routing::resolve_purpose_model_db_excluding(db, purpose, exclude).await {
        crate::internal_routing::PurposeResolution::Resolved {
            provider, model, ..
        } => (provider, model),
        other if !exclude.is_empty() => {
            // L'esclusione del provider del worker ha svuotato il pool: fallback
            // SENZA esclusione (il review gira comunque, vedi doc). WARN esplicito.
            tracing::warn!(
                kind = %kind, model_purpose = %purpose, excluded = ?exclude,
                resolution = ?other,
                "subagent_native: giudice != worker impossibile (unico provider capable), \
                 review sullo stesso provider del worker"
            );
            match crate::internal_routing::resolve_purpose_model_db(db, purpose).await {
                crate::internal_routing::PurposeResolution::Resolved {
                    provider, model, ..
                } => (provider, model),
                _ => (String::new(), String::new()),
            }
        }
        other => {
            tracing::warn!(
                kind = %kind,
                model_purpose = %purpose,
                resolution = ?other,
                "subagent_native: model_purpose non risolto, routing di default"
            );
            (String::new(), String::new())
        }
    }
}

/// Provider del run PADRE (`agent_runs.provider`) per il vincolo giudice != worker
/// (Fase C2). `None` se l'anchor non e' un run tracciato (sessione senza riga in
/// `agent_runs`) o il provider e' vuoto: in tal caso nessuna esclusione.
async fn parent_provider(db: &sqlx::PgPool, anchor: Uuid) -> Option<String> {
    let provider: Option<String> =
        sqlx::query_scalar("SELECT provider FROM agent_runs WHERE id = $1")
            .bind(anchor)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    provider.filter(|p| !p.trim().is_empty())
}

/// Provider PINNATO dall'utente sulla chat (`chat_sessions.preferred_provider`).
/// DISTINTA da [`parent_provider`]: legge dal pool PROGETTO (`chat_sessions` vive
/// nel DB del progetto, NON nel meta — incidente separazione DB), non da
/// `agent_runs`. `None` se la sessione non ha pin, il valore e' NULL/vuoto, o la
/// query fallisce (fail-open: nessun pin -> routing worker standard). Il pin e'
/// SOLO il provider: il modello si deriva sempre dal tier via catalog (regola G).
async fn session_pinned_provider(proj_pool: &sqlx::PgPool, session_id: Uuid) -> Option<String> {
    let pinned: Option<String> =
        sqlx::query_scalar("SELECT preferred_provider FROM chat_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(proj_pool)
            .await
            .ok()
            .flatten();
    pinned.filter(|p| !p.trim().is_empty())
}

/// Campi comuni della riga `nexus_subagent_runs` (i rami isolato/sequenziale
/// differiscono solo per id/worktree, portati da `IsolationSlot`).
struct NewSubagentRun<'a> {
    anchor: Uuid,
    project_id: Uuid,
    kind: &'a str,
    task: &'a str,
    context_blob: &'a str,
    expected_format: &'a str,
    depth: i32,
}

/// INSERT della riga sub-run, query UNICA per i due rami. Ramo ISOLATO
/// (`slot` presente): id = run_id del worktree (allineamento row DB <->
/// dir/branch) + persistenza worktree_path/base_commit per audit/reconcile.
/// Ramo sequenziale: id generato dal DB (`COALESCE(NULL, gen_random_uuid())` =
/// stesso DEFAULT della tabella, mig 0151) e colonne worktree NULL (parita'
/// con l'INSERT storico che non le elencava, mig project 0005 senza default).
async fn insert_subagent_run(
    pool: &sqlx::PgPool,
    row: &NewSubagentRun<'_>,
    slot: Option<&IsolationSlot>,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO nexus_subagent_runs
           (id, parent_run_id, project_id, kind, task_description, context_blob, expected_format,
            status, is_background, depth, source, worktree_path, base_commit)
           VALUES (COALESCE($1, gen_random_uuid()), $2, $3, $4, $5, $6, $7,
                   'running', false, $8, 'db', $9, $10)
           RETURNING id"#,
    )
    .bind(slot.map(|s| s.run_id))
    .bind(row.anchor)
    .bind(row.project_id)
    .bind(row.kind)
    .bind(row.task)
    .bind(row.context_blob)
    .bind(row.expected_format)
    .bind(row.depth)
    .bind(slot.map(|s| s.worktree_path.to_string_lossy().to_string()))
    .bind(slot.map(|s| s.base_commit.as_str()))
    .fetch_one(pool)
    .await
}

/// Ferma il ponte narrazione e ATTENDE che sia davvero finito (vedi commento
/// sul timeout in `run_single_subagent`: chiude la race abort/INSERT-in-volo).
async fn stop_bridge(bridge: Option<tokio::task::JoinHandle<()>>) {
    if let Some(b) = bridge {
        b.abort();
        let _ = b.await;
    }
}

/// Chiusura del sub-run in TIMEOUT: mark_run + narrazione + tool_result.
async fn finalize_timeout(
    pool: &sqlx::PgPool,
    narrator: Option<&ParentNarrator>,
    sub_run_id: Uuid,
    kind: &str,
    timeout_s: i64,
) -> Value {
    let verdict = terminal_verdict("timed_out", "timeout");
    let _ = mark_run(
        pool,
        sub_run_id,
        SubRunClosure::without_metrics("timeout", "[Sub-agent timeout]", verdict.clone()),
    )
    .await;
    if let Some(n) = narrator {
        n.say(
            "subagent_failed",
            format!("Subagente {kind} in timeout dopo {timeout_s}s"),
            n.with_pin(json!({
                K_SUB_RUN_ID: sub_run_id.to_string(),
                K_SUB_KIND: kind,
                "status": "timeout",
                K_TIMEOUT_S: timeout_s,
            })),
            sub_run_id,
        )
        .await;
    }
    json!({
        K_SUB_RUN_ID: sub_run_id.to_string(),
        "kind": kind,
        "status": "timeout",
        "error": "[Sub-agent timeout]",
        // Verdetto ESITO strutturato (regola M): il coordinatore legge
        // success/verdict qui, mai dalla prosa di `error`.
        "outcome": verdict,
    })
}

/// Narrazione (meta-step live sul run PADRE) della chiusura OK del sub-run.
/// Estratta da `finalize_success` (regola L / lunghezza): no-op se `narrator`
/// e' `None` (kill-switch UX `subagent_narration_enabled`).
async fn narrate_completed(
    narrator: Option<&ParentNarrator>,
    sub_run_id: Uuid,
    kind: &str,
    status: &str,
    summary: &str,
    o: &NativeRunOutcome,
) {
    let Some(n) = narrator else { return };
    let esito = if o.completed { "completato" } else { "in pausa" };
    n.say(
        "subagent_completed",
        format!("Subagente {kind} {esito} ({} iterazioni)", o.iterations),
        n.with_pin(json!({
            K_SUB_RUN_ID: sub_run_id.to_string(),
            K_SUB_KIND: kind,
            "status": status,
            K_SUMMARY: compact_summary(summary),
            "iterations": o.iterations,
            "cost_usd": o.total_cost,
        })),
        sub_run_id,
    )
    .await;
}

/// Chiusura del ramo OK del sub-run: mark_run + log + narrazione + tool_result
/// (il main riceve SOLO questo summary, non l'intera conversazione del figlio).
async fn finalize_success(
    pool: &sqlx::PgPool, narrator: Option<&ParentNarrator>,
    sub_run_id: Uuid, kind: &str, depth: i64, o: &NativeRunOutcome,
) -> Value {
    let summary = o.final_answer.clone().unwrap_or_default();
    // `status` LIFECYCLE del sub-run (completed = arrivato a End, paused = fermato
    // su interrupt HITL). Backward-compat: todo_runner e il batch lo leggono come
    // {completed|paused}; NON va sostituito col verdetto canonico (regredirebbe i
    // completed_unverified). Il VERDETTO del lavoro (segnali strutturati, regola M)
    // viaggia separato nel blocco `outcome`, senza toccare il lifecycle.
    let status = if o.completed { "completed" } else { "paused" };
    let verdict = o.structured_verdict();
    let _ = mark_run(
        pool,
        sub_run_id,
        SubRunClosure {
            status,
            summary: &summary,
            iterations: o.iterations,
            tokens_prompt: o.prompt_tokens,
            tokens_completion: o.completion_tokens,
            cost_usd: o.total_cost,
            verdict: verdict.clone(),
        },
    )
    .await;
    tracing::info!(
        kind = %kind,
        subagent_run_id = %sub_run_id,
        depth,
        completed = o.completed,
        iterations = o.iterations,
        summary_len = summary.len(),
        "subagent_native: sub-run eseguito sul grafo nativo"
    );
    narrate_completed(narrator, sub_run_id, kind, status, &summary, o).await;
    json!({
        K_SUB_RUN_ID: sub_run_id.to_string(),
        "kind": kind,
        "status": status,
        K_SUMMARY: compact_summary(&summary),
        "iterations": o.iterations,
        "cost_usd": o.total_cost,
        "tokens": { "prompt": o.prompt_tokens, "completion": o.completion_tokens },
        // Verdetto ESITO strutturato (regola M): il coordinatore legge
        // success/verdict qui invece di dedurre l'esito dalla prosa di `summary`.
        "outcome": verdict,
    })
}

/// Chiusura del ramo ERRORE del sub-run (fallback onesto: errore al chiamante).
async fn finalize_failure(
    pool: &sqlx::PgPool,
    narrator: Option<&ParentNarrator>,
    sub_run_id: Uuid,
    kind: &str,
    e: &anyhow::Error,
) -> Value {
    let msg = format!("[errore grafo nativo: {e}]");
    let verdict = terminal_verdict("failed", "engine_error");
    let _ = mark_run(
        pool,
        sub_run_id,
        SubRunClosure::without_metrics("failed", &msg, verdict.clone()),
    )
    .await;
    tracing::warn!(
        kind = %kind,
        subagent_run_id = %sub_run_id,
        error = %e,
        "subagent_native: sub-run fallito"
    );
    if let Some(n) = narrator {
        n.say(
            "subagent_failed",
            format!("Subagente {kind} fallito"),
            n.with_pin(json!({
                K_SUB_RUN_ID: sub_run_id.to_string(),
                K_SUB_KIND: kind,
                "status": "failed",
                "error": compact_summary(&msg),
            })),
            sub_run_id,
        )
        .await;
    }
    json!({
        "error": msg,
        K_SUB_RUN_ID: sub_run_id.to_string(),
        "kind": kind,
        // Verdetto ESITO strutturato (regola M): mai dedurre il fallimento
        // dalla prosa di `error`.
        "outcome": verdict,
    })
}

/// Crea la riga `agent_runs` del SUB-run (run TRACCIATO: senza, il guard
/// "untracked_run" di `PgAgentStepStore` scarta in silenzio ogni step del
/// figlio). `nexus_agent_type='subagent'` discrimina i sub-run nelle query UI
/// top-level; `parent_run_id` e' impostato via subquery SOLO se l'ancora e' un
/// run tracciato (FK verso agent_runs: l'ancora puo' essere la sessione).
/// Best-effort ma LOGGATO forte: se fallisce, gli step del figlio non verranno
/// persistiti (la stessa cecita' dell'incidente 2026-07-06).
#[allow(clippy::too_many_arguments)]
async fn ensure_child_agent_run(
    db: &sqlx::PgPool,
    run_id: Uuid,
    session_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
    anchor: Uuid,
    provider: &str,
    model: &str,
) {
    let provider = (!provider.trim().is_empty()).then_some(provider);
    let model = (!model.trim().is_empty()).then_some(model);
    let res = sqlx::query(
        r#"INSERT INTO agent_runs
           (id, session_id, project_id, user_id, status, automation_mode,
            provider, model, nexus_agent_type, parent_run_id)
           VALUES ($1, $2, $3, $4, 'running', 'automatic', $5, $6, 'subagent',
                   (SELECT id FROM agent_runs WHERE id = $7))
           ON CONFLICT (id) DO NOTHING"#,
    )
    .bind(run_id)
    .bind(session_id)
    .bind(project_id)
    .bind(user_id)
    .bind(provider)
    .bind(model)
    .bind(anchor)
    .execute(db)
    .await;
    if let Err(e) = res {
        tracing::warn!(
            target: "mcp_core::subagent_native",
            subagent_run_id = %run_id,
            error = %e,
            "riga agent_runs del sub-run NON creata: gli step del figlio non saranno persistiti"
        );
    }
}

/// Esito di chiusura di un sub-run: status + summary + metriche, condiviso
/// dalle due righe da chiudere (nexus_subagent_runs + gemella agent_runs).
struct SubRunClosure<'a> {
    status: &'a str,
    summary: &'a str,
    iterations: i64,
    tokens_prompt: i64,
    tokens_completion: i64,
    cost_usd: f64,
    /// Blocco esito STRUTTURATO del sub-run (regola M / ADR 0034), distinto dallo
    /// `status` lifecycle: persistito su `nexus_subagent_runs.verdict` (mig
    /// project/0009) per il fan-in asincrono (poll) e i coordinatori.
    verdict: Value,
}

impl<'a> SubRunClosure<'a> {
    /// Chiusura senza metriche: il run e' morto prima di produrre usage
    /// (timeout, errore del grafo) — contatori a zero.
    fn without_metrics(status: &'a str, summary: &'a str, verdict: Value) -> Self {
        Self {
            status,
            summary,
            iterations: 0,
            tokens_prompt: 0,
            tokens_completion: 0,
            cost_usd: 0.0,
            verdict,
        }
    }
}

/// Blocco esito strutturato per i rami TERMINALI del sub-run privi di un
/// [`NativeRunOutcome`] (timeout / errore del motore): stessa forma di
/// [`NativeRunOutcome::structured_verdict`] cosi' il coordinatore e il poll
/// leggono SEMPRE lo stesso schema. Le chiavi vengono dal PUNTO UNICO
/// [`verdict_keys`] (regola L): la parita' con `structured_verdict` e' quindi
/// strutturale (stesse costanti), non solo asserita — piu' il test
/// `terminal_verdict_stessa_forma_di_structured_verdict` come rete. `verdict`
/// usa il vocabolario canonico (`AgentRunStatus::as_str`: `timed_out` /
/// `failed`), `success` e' sempre `false`.
fn terminal_verdict(verdict: &str, error_class: &str) -> Value {
    use verdict_keys as k;
    json!({
        k::VERDICT: verdict,
        k::SUCCESS: false,
        k::DECLARED: Value::Null,
        k::REVIEW: Value::Null,
        k::FINAL_GATE_PASSED: Value::Null,
        k::FINAL_GATE_UNVERIFIED: Value::Null,
        k::FINAL_GATE_FAILED_PENDING: false,
        k::FORCED_CLOSE_UNVERIFIED: false,
        k::ERROR_CLASS: error_class,
    })
}

/// Marca una sub-run come conclusa su `nexus_subagent_runs` E chiude la riga
/// gemella `agent_runs` del figlio, in UNA statement atomica (CTE): stessi
/// bind per entrambe. La gemella esiste solo per i figli tracciati (la WHERE
/// id limita l'effetto); stessi status (completed/paused/timeout/failed):
/// `agent_runs.status` e' TEXT libero. Il `verdict` strutturato (regola M) va
/// SOLO sulla riga sub (la gemella eredita l'esito dal finalizzatore del padre).
/// Best-effort per i chiamanti (la finalizzazione non deve fallire per un
/// errore di persistenza) ma MAI muto: l'UPDATE respinto lascerebbe la riga
/// 'running' per sempre (depth chain bloccata, poll che non converge) e senza
/// log sarebbe invisibile — WARN qui, nel punto unico (regola L).
async fn mark_run(
    db: &sqlx::PgPool,
    run_id: Uuid,
    c: SubRunClosure<'_>,
) -> Result<(), sqlx::Error> {
    let compact = c.summary.chars().take(4000).collect::<String>();
    let res = sqlx::query(
        "WITH sub AS (
            UPDATE nexus_subagent_runs SET
                status = $1, final_summary = $2, iterations = $3,
                tokens_prompt = $4, tokens_completion = $5, cost_usd = $6,
                verdict = $8, completed_at = NOW()
             WHERE id = $7
        )
        UPDATE agent_runs SET
            status = $1, final_answer = $2, iteration_count = $3,
            prompt_tokens = $4, completion_tokens = $5,
            total_tokens = $4 + $5, total_cost = $6, completed_at = NOW()
         WHERE id = $7",
    )
    .bind(c.status)
    .bind(&compact)
    .bind(c.iterations as i32)
    .bind(c.tokens_prompt as i32)
    .bind(c.tokens_completion as i32)
    .bind(c.cost_usd)
    .bind(run_id)
    .bind(c.verdict)
    .execute(db)
    .await
    .map(|_| ());
    if let Err(e) = &res {
        tracing::warn!(
            subagent_run_id = %run_id,
            status = c.status,
            error = %e,
            "mark_run: esito del sub-run NON persistito (la riga resta 'running')"
        );
    }
    res
}

const TRUNC_SUFFIX: &str = "...[truncated]";

/// Tronca il summary inviato al main (riceve solo questo, non l'intera conversazione).
fn compact_summary(text: &str) -> String {
    const MAX: usize = 600;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let keep = MAX.saturating_sub(TRUNC_SUFFIX.len());
    let head: String = text.chars().take(keep).collect();
    format!("{head}{TRUNC_SUFFIX}")
}

/// Assembla le `NativeDeps` (ToolRunner in-process + client gateway) dal
/// `AgentToolContext`. Specchio di `build_native_deps` (agent_run.rs) ma a partire
/// dal ctx del tool (che non porta `AppState`): riusa il PUNTO UNICO del cablaggio
/// gateway (`NexusGatewayClient::from_db`, regola L).
async fn build_native_deps_for_tool(ctx: &AgentToolContext) -> NativeDeps {
    let db = (*ctx.core.db).clone();
    let tool_runner_deps = crate::tool_runner_server::ToolRunnerDeps {
        db: db.clone(),
        neural: ctx.neural.clone(),
        playwright_channels: ctx.playwright_channels.clone(),
        dependency_status: ctx.dependency_status.clone(),
        project_channels: ctx.core.project_channels.clone(),
        monitor_registry: ctx.core.monitor_registry.clone(),
        port_registry: ctx.port_registry.clone(),
    };
    let gateway = crate::nexus_gateway::NexusGatewayClient::from_db(&db).await;
    NativeDeps {
        db,
        tool_runner_deps,
        gateway,
    }
}

/// `nexus_subagent_poll` — stato di una sub-run da `nexus_subagent_runs` (DB-only,
/// niente brain). Il main lo usa per i kind background.
pub async fn tool_nexus_subagent_poll(ctx: &AgentToolContext, input: &Value) -> String {
    let run_id = match input.get(K_SUB_RUN_ID).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return "\u{274C} [nexus_subagent_poll] parametro 'subagent_run_id' obbligatorio"
                .to_string()
        }
    };
    // Routing separazione DB: nexus_subagent_runs e' migrata; il sub-run e' nel DB
    // del progetto corrente (stesso project_id che lo ha dispatchato).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&ctx.core.db, ctx.core.project_id).await;
    let row = sqlx::query(
        "SELECT id::text, status, kind, final_summary, artifacts, iterations,
                tokens_prompt, tokens_completion, cost_usd, depth, source, is_background,
                verdict
         FROM nexus_subagent_runs WHERE id::text = $1",
    )
    .bind(&run_id)
    .fetch_optional(&proj_pool)
    .await;
    match row {
        Ok(Some(r)) => json!({
            K_SUB_RUN_ID: r.try_get::<String, _>("id").unwrap_or_default(),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
            "summary": r.try_get::<Option<String>, _>("final_summary").unwrap_or(None),
            "artifacts": r.try_get::<Option<Vec<String>>, _>("artifacts").unwrap_or(None).unwrap_or_default(),
            "iterations": r.try_get::<i32, _>("iterations").unwrap_or(0),
            "tokens": {
                "prompt": r.try_get::<i32, _>("tokens_prompt").unwrap_or(0),
                "completion": r.try_get::<i32, _>("tokens_completion").unwrap_or(0),
            },
            "cost_usd": r.try_get::<f64, _>("cost_usd").unwrap_or(0.0),
            "depth": r.try_get::<i32, _>("depth").unwrap_or(1),
            "source": r.try_get::<String, _>("source").unwrap_or_else(|_| "db".into()),
            "is_background": r.try_get::<bool, _>("is_background").unwrap_or(false),
            // Verdetto ESITO strutturato (mig project/0009): `null` finche' il
            // sub-run non e' finalizzato (fase running). Il coordinatore che fa
            // poll legge success/verdict qui senza dedurre l'esito dalla prosa
            // di `summary` (regola M).
            "outcome": r.try_get::<Option<Value>, _>("verdict").unwrap_or(None),
        })
        .to_string(),
        Ok(None) => format!("\u{274C} [nexus_subagent_poll] sub-agent run '{run_id}' non trovato"),
        Err(e) => format!("\u{274C} [nexus_subagent_poll] query fallita: {e}"),
    }
}

/// `nexus_subagent_resume` — riprende una sub-run sul GRAFO NATIVO (niente brain).
/// Il run nativo gira da capo dal task della row (i checkpoint del sub-run vivono
/// su `nexus_graph_checkpoints`, ma la ripresa nativa qui ri-esegue il sub-run dal
/// suo `run_id`: la stessa thread del grafo riprende dal checkpoint persistente).
pub async fn tool_nexus_subagent_resume(ctx: &AgentToolContext, input: &Value) -> String {
    let run_id_str = match input.get(K_SUB_RUN_ID).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return "\u{274C} [nexus_subagent_resume] parametro 'subagent_run_id' obbligatorio"
                .to_string()
        }
    };
    let run_id = match Uuid::parse_str(&run_id_str) {
        Ok(u) => u,
        Err(_) => {
            return format!("\u{274C} [nexus_subagent_resume] subagent_run_id non valido: {run_id_str}")
        }
    };

    // Routing separazione DB: nexus_subagent_runs e' migrata; il sub-run da riprendere
    // e' nel DB del progetto corrente (stesso project_id che lo ha dispatchato).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&ctx.core.db, ctx.core.project_id).await;
    let row = sqlx::query(
        "SELECT kind, task_description, context_blob, expected_format, status, depth, parent_run_id
         FROM nexus_subagent_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(&proj_pool)
    .await;
    let row = match row {
        Ok(Some(r)) => r,
        Ok(None) => {
            return format!("\u{274C} [nexus_subagent_resume] sub-agent run '{run_id}' non trovato")
        }
        Err(e) => return format!("\u{274C} [nexus_subagent_resume] query fallita: {e}"),
    };
    let status: String = row.get("status");
    if !matches!(status.as_str(), "paused" | "running" | "timeout") {
        return json!({"status": "noop", K_SUB_RUN_ID: run_id_str, "current_status": status})
            .to_string();
    }
    let kind: String = row.get("kind");
    let task: String = row.get("task_description");
    let context_blob: String = row
        .try_get::<Option<String>, _>("context_blob")
        .ok()
        .flatten()
        .unwrap_or_default();
    let expected: String = row
        .try_get::<Option<String>, _>("expected_format")
        .ok()
        .flatten()
        .unwrap_or_default();

    // Ripresa = ri-esecuzione del sub-run sul kind dato. Riusa lo stesso percorso
    // del dispatch (guard + grafo nativo): semantica onesta, niente notifica brain.
    // Il resume gira SEMPRE sulla root condivisa (nessun isolamento fisico): la
    // ripresa di un singolo sub-run non e' un batch parallelo-che-scrive.
    let res = run_single_subagent(ctx, &kind, &task, &context_blob, &expected, None).await;
    res.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Nomi fixture ricorrenti dei test del ponte narrazione.
    const T_EDIT: &str = "edit_file";
    const T_RUN: &str = "run_command";
    const FIX_PATH: &str = "src/a.rs";

    #[test]
    fn err_ha_marker_errore() {
        let m = err("boom");
        assert!(m.starts_with('\u{274C}'));
        assert!(m.contains("dispatch_subagent"));
    }

    #[test]
    fn build_tools_json_filtra_per_whitelist() {
        let tools = build_tools_json(&["read_file".to_string(), "write_file".to_string()]);
        let arr = tools.as_array().expect("array");
        // Solo i tool in whitelist, con schema REALE (campo input_schema presente).
        assert!(!arr.is_empty(), "lo schema reale deve contenere i tool richiesti");
        for t in arr {
            let name = t.get("name").and_then(Value::as_str).unwrap_or("");
            assert!(
                name == "read_file" || name == "write_file",
                "tool fuori whitelist: {name}"
            );
            assert!(t.get("input_schema").is_some(), "schema reale atteso");
        }
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
    }

    #[test]
    fn build_tools_json_vuoto_se_whitelist_vuota() {
        let tools = build_tools_json(&[]);
        assert_eq!(tools.as_array().map(|a| a.len()), Some(0));
    }

    #[test]
    fn compact_summary_tronca_oltre_soglia() {
        let breve = "ciao";
        assert_eq!(compact_summary(breve), "ciao");
        let lungo = "x".repeat(1000);
        let out = compact_summary(&lungo);
        assert!(out.ends_with(TRUNC_SUFFIX));
        assert!(out.chars().count() <= 600);
    }

    fn sc(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn should_isolate_batch_flag_off_sempre_sequenziale() {
        // Con isolamento NON disponibile (flag OFF, default) il ramo isolato non
        // scatta MAI, qualunque siano gli scope -> bit-identico allo storico.
        let scopes = vec![sc(&["crates/a"]), sc(&["crates/b"])];
        assert!(
            !should_isolate_batch(false, &scopes),
            "flag OFF -> sempre sequenziale"
        );
        // Anche con scope vuoti / sovrapposti resta false (irrilevante a flag OFF).
        assert!(!should_isolate_batch(false, &[sc(&["src/a"]), sc(&["src/a"])]));
        assert!(!should_isolate_batch(false, &[]));
    }

    #[test]
    fn should_isolate_batch_flag_on_solo_se_disgiunti() {
        // Disponibile + scope disgiunti -> ISOLATO.
        assert!(should_isolate_batch(
            true,
            &[sc(&["crates/a"]), sc(&["crates/b"])]
        ));
        // Disponibile ma scope SOVRAPPOSTI -> degrada a sequenziale.
        assert!(!should_isolate_batch(
            true,
            &[sc(&["src/a"]), sc(&["src/a/b"])]
        ));
        // Disponibile ma uno scope VUOTO (nessun write_scope dichiarato: caso di
        // dispatch_wave finche' la colonna DB non e' persistita) -> sequenziale.
        assert!(!should_isolate_batch(true, &[sc(&["src/a"]), sc(&[])]));
        // Disponibile ma scope tocca la denylist (lockfile) -> sequenziale.
        assert!(!should_isolate_batch(
            true,
            &[sc(&["Cargo.lock"]), sc(&["crates/b"])]
        ));
    }

    /// Copertura del ramo DELETE di `reindex_promoted_once` (D3): il branch e'
    /// governato dal segnale strutturato `tokio::fs::try_exists` sul path promosso
    /// (regola M, niente parsing). `reindex_promoted_once` non e' isolabile
    /// (dipende da `AgentToolContext` = DB + neural), quindi si verifica qui il
    /// predicato che seleziona il ramo: un file promosso ma CANCELLATO risulta
    /// `try_exists == Ok(false)` (-> ramo delete via
    /// `vector_memory::delete_code_index_file_points`); un file esistente risulta
    /// `Ok(true)` (-> ramo reindex invariato).
    #[tokio::test]
    async fn reindex_promoted_delete_branch_su_file_inesistente() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path();

        // File promosso ma cancellato: NON esiste sul filesystem -> ramo delete.
        let cancellato = root.join("src").join("rimosso.rs");
        assert!(
            !tokio::fs::try_exists(&cancellato).await.unwrap(),
            "un file promosso ma cancellato deve risultare inesistente -> ramo delete"
        );

        // File promosso ancora presente: esiste -> ramo reindex (invariato).
        std::fs::create_dir_all(root.join("src")).expect("mkdir src");
        let presente = root.join("src").join("vivo.rs");
        std::fs::write(&presente, "// vivo\n").expect("write vivo");
        assert!(
            tokio::fs::try_exists(&presente).await.unwrap(),
            "un file promosso ancora presente deve risultare esistente -> ramo reindex"
        );

        // Normalizzazione path relativo a separatori '/' (come indexing.rs), input di
        // delete_code_index_file_points.
        let rel = "src\\rimosso.rs";
        assert_eq!(rel.replace('\\', "/"), "src/rimosso.rs");
    }

    /// Costruttore di comodo per gli eventi step del sub-run nei test del ponte.
    fn step_event(
        status: crate::agent_types::AgentStepStatus,
        tool: &str,
        idx: u32,
        input: Value,
    ) -> crate::agent_types::AgentStepEvent {
        crate::agent_types::AgentStepEvent {
            run_id: "r".into(),
            step: Some(crate::agent_types::AgentStep {
                run_id: "r".into(),
                step_index: idx,
                tool_name: tool.into(),
                tool_input: input,
                tool_result: None,
                status,
                created_at: String::new(),
            }),
            trace: None,
            is_final: false,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        }
    }

    /// La decisione d'inoltro guarda SOLO il segnale strutturato dello step
    /// (regola M): conclusi si', Running no, orfani (nome vuoto) no.
    #[test]
    fn concluded_tool_step_solo_esiti_conclusi() {
        use crate::agent_types::AgentStepStatus as S;
        let ok = step_event(S::Completed, "read_file", 3, json!({}));
        assert_eq!(
            concluded_tool_step(&ok),
            Some((false, "read_file".to_string(), 3))
        );
        let ko = step_event(S::Failed, T_RUN, 5, json!({}));
        assert_eq!(concluded_tool_step(&ko), Some((true, T_RUN.to_string(), 5)));
        let running = step_event(S::Running, "read_file", 1, json!({}));
        assert_eq!(concluded_tool_step(&running), None, "Running: doppione");
        let orfano = step_event(S::Completed, "", 2, json!({}));
        assert_eq!(concluded_tool_step(&orfano), None, "senza nome: nessuna riga utile");
        let senza_step = crate::agent_types::AgentStepEvent {
            run_id: "r".into(),
            step: None,
            trace: None,
            is_final: false,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        };
        assert_eq!(concluded_tool_step(&senza_step), None);
    }

    /// Il target si estrae dallo step `Running` (il ToolUse porta l'input reale
    /// in `tool_input.input`), col punto unico del grafo (path/command/...).
    #[test]
    fn running_tool_target_dal_tool_use() {
        use crate::agent_types::AgentStepStatus as S;
        let ev = step_event(
            S::Running,
            T_EDIT,
            7,
            json!({"id": "tu_1", "input": {"path": FIX_PATH}}),
        );
        assert_eq!(running_tool_target(&ev), Some((7, FIX_PATH.to_string())));
        // Il Completed non porta l'input reale: nessun target da qui.
        let done = step_event(S::Completed, T_EDIT, 7, json!({"id": "tu_1"}));
        assert_eq!(running_tool_target(&done), None);
    }

    /// Titolo e payload del progresso: stesso registro della narrazione tool del
    /// run corrente, con l'esito dall'is_error strutturato.
    #[test]
    fn tool_progress_meta_compone_titolo_e_payload() {
        let id = Uuid::nil();
        let (title, payload) = tool_progress_meta("coder", id, T_EDIT, Some(FIX_PATH), false);
        assert_eq!(title, "subagente coder: tool edit_file — src/a.rs");
        assert_eq!(payload["phase"], json!("tool"));
        assert_eq!(payload["tool"], json!(T_EDIT));
        assert_eq!(payload[K_TARGET], json!(FIX_PATH));
        assert_eq!(payload[K_IS_ERROR], json!(false));
        assert_eq!(payload[K_SUB_KIND], json!("coder"));
        assert_eq!(payload[K_SUB_RUN_ID], json!(id.to_string()));

        let (title_err, payload_err) = tool_progress_meta("tester", id, T_RUN, None, true);
        assert_eq!(title_err, "subagente tester: errore run_command");
        assert_eq!(payload_err[K_IS_ERROR], json!(true));
        assert_eq!(payload_err[K_TARGET], json!(null));
    }

    /// Il parse dei task del batch estrae `write_scope` dal JSON col punto
    /// unico `write_scope_of` (alimenta il gating di isolamento).
    #[test]
    fn write_scope_parsato_dal_task_json() {
        let t = json!({
            "kind": "coder",
            "task": "x",
            "write_scope": ["crates/a", "docs/a.md"],
        });
        assert_eq!(write_scope_of(&t), sc(&["crates/a", "docs/a.md"]));

        // Task senza write_scope -> vec vuoto -> gating degrada a sequenziale.
        let ws2 = write_scope_of(&json!({ "kind": "coder", "task": "y" }));
        assert!(ws2.is_empty());
        assert!(!should_isolate_batch(true, &[ws2]));
    }

    /// Tabella minima per i test di `insert_subagent_run` (colonne toccate
    /// dall'INSERT; `id` col DEFAULT reale della mig 0151, worktree senza default
    /// come la mig project 0005).
    async fn create_subagent_runs_min(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_subagent_runs ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 parent_run_id UUID NOT NULL, project_id UUID NOT NULL, \
                 kind TEXT NOT NULL, task_description TEXT NOT NULL, \
                 context_blob TEXT, expected_format TEXT, status TEXT NOT NULL, \
                 is_background BOOLEAN NOT NULL DEFAULT false, \
                 depth INTEGER NOT NULL DEFAULT 1, source TEXT NOT NULL DEFAULT 'db', \
                 worktree_path TEXT, base_commit TEXT )",
        )
        .execute(pool)
        .await
        .expect("create nexus_subagent_runs");
    }

    async fn fetch_worktree_cols(pool: &sqlx::PgPool, id: Uuid) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT worktree_path, base_commit FROM nexus_subagent_runs WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("fetch cols")
    }

    /// `insert_subagent_run` (query UNICA con COALESCE) deve produrre lo STESSO
    /// esito dei due INSERT storici, blindando il refactor di de-duplicazione
    /// (regola H): ramo sequenziale -> id generato dal DB (DEFAULT) + colonne
    /// worktree NULL; ramo isolato -> id = run_id del worktree + worktree_path/
    /// base_commit persistiti.
    #[sqlx::test]
    async fn insert_subagent_run_equivalenza_rami(pool: sqlx::PgPool) {
        create_subagent_runs_min(&pool).await;
        let row = NewSubagentRun {
            anchor: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            kind: "coder",
            task: "t",
            context_blob: "",
            expected_format: "",
            depth: 1,
        };

        // Ramo sequenziale: id dal DB (non nil), colonne worktree NULL.
        let seq_id = insert_subagent_run(&pool, &row, None).await.expect("insert seq");
        assert!(!seq_id.is_nil(), "id generato dal DB non nullo");
        assert_eq!(fetch_worktree_cols(&pool, seq_id).await, (None, None));

        // Ramo isolato: id = run_id del worktree, colonne worktree persistite.
        let slot = IsolationSlot {
            run_id: Uuid::new_v4(),
            worktree_path: std::path::PathBuf::from("/tmp/wt"),
            base_commit: "abc123".to_string(),
        };
        let iso_id = insert_subagent_run(&pool, &row, Some(&slot)).await.expect("insert iso");
        assert_eq!(iso_id, slot.run_id, "ramo isolato: id = run_id del worktree");
        assert_eq!(
            fetch_worktree_cols(&pool, iso_id).await,
            (Some(slot.worktree_path.to_string_lossy().to_string()), Some(slot.base_commit)),
        );
    }

    /// Il pin provider/model entra nel payload SOLO quando risolto; vuoto =
    /// campo omesso (il nastro frontend degrada a '?', mai valori inventati).
    #[test]
    fn put_model_fields_solo_quando_risolto() {
        let mut payload = json!({"phase": "tool"});
        put_model_fields(&mut payload, "mistral", "mistral-medium-3");
        assert_eq!(payload[K_PROVIDER], json!("mistral"));
        assert_eq!(payload[K_MODEL], json!("mistral-medium-3"));

        let mut vuoto = json!({"phase": "tool"});
        put_model_fields(&mut vuoto, "", "  ");
        assert!(vuoto.get(K_PROVIDER).is_none(), "pin non risolto: campo omesso");
        assert!(vuoto.get(K_MODEL).is_none());
    }

    /// Schema minimo per i test del run TRACCIATO del figlio (senza FK di
    /// sessione: qui interessa il contratto insert/close, non lo schema pieno).
    async fn create_child_run_tables(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID PRIMARY KEY, \
                 session_id UUID NOT NULL, \
                 project_id UUID NOT NULL, \
                 user_id UUID NOT NULL, \
                 status TEXT NOT NULL DEFAULT 'running', \
                 automation_mode TEXT NOT NULL DEFAULT 'confirm', \
                 provider TEXT, \
                 model TEXT, \
                 nexus_agent_type TEXT, \
                 parent_run_id UUID REFERENCES agent_runs(id) ON DELETE SET NULL, \
                 iteration_count INT NOT NULL DEFAULT 0, \
                 final_answer TEXT, \
                 prompt_tokens INT NOT NULL DEFAULT 0, \
                 completion_tokens INT NOT NULL DEFAULT 0, \
                 total_tokens INT NOT NULL DEFAULT 0, \
                 total_cost DOUBLE PRECISION NOT NULL DEFAULT 0, \
                 completed_at TIMESTAMPTZ, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() )",
        )
        .execute(pool)
        .await
        .expect("create agent_runs");
        sqlx::query(
            "CREATE TABLE nexus_subagent_runs ( \
                 id UUID PRIMARY KEY, \
                 status TEXT NOT NULL DEFAULT 'running', \
                 final_summary TEXT, \
                 iterations INT NOT NULL DEFAULT 0, \
                 tokens_prompt INT NOT NULL DEFAULT 0, \
                 tokens_completion INT NOT NULL DEFAULT 0, \
                 cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0, \
                 verdict JSONB, \
                 completed_at TIMESTAMPTZ )",
        )
        .execute(pool)
        .await
        .expect("create nexus_subagent_runs (chiusura)");
    }

    /// Helper: crea il run TRACCIATO del figlio con ids freschi (punto unico
    /// dei call-site di test, evita blocchi duplicati).
    async fn ensure_child(
        pool: &sqlx::PgPool,
        child: Uuid,
        anchor: Uuid,
        provider: &str,
        model: &str,
    ) {
        ensure_child_agent_run(
            pool,
            child,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            anchor,
            provider,
            model,
        )
        .await;
    }

    /// Il sub-run diventa un run TRACCIATO (riga agent_runs marcata
    /// 'subagent'): senza, il guard untracked_run dello step store scartava in
    /// silenzio ogni tool_result del figlio (incidente 2026-07-06). L'ancora
    /// NON tracciata (sessione) NON deve violare la FK: parent_run_id resta
    /// NULL via subquery.
    #[sqlx::test]
    async fn ensure_child_agent_run_traccia_il_figlio(pool: sqlx::PgPool) {
        create_child_run_tables(&pool).await;
        let child = Uuid::new_v4();
        let anchor_non_tracciato = Uuid::new_v4(); // sessione: NON in agent_runs
        ensure_child(&pool, child, anchor_non_tracciato, "mistral", "mistral-medium-3").await;
        let row: (String, Option<String>, Option<Uuid>, Option<String>) = sqlx::query_as(
            "SELECT status, nexus_agent_type, parent_run_id, model FROM agent_runs WHERE id = $1",
        )
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("riga figlio presente");
        assert_eq!(row.0, "running");
        assert_eq!(row.1.as_deref(), Some("subagent"));
        assert_eq!(row.2, None, "ancora non tracciata -> parent NULL (no FK rotta)");
        assert_eq!(row.3.as_deref(), Some("mistral-medium-3"));

        // Idempotente sul retry (ON CONFLICT DO NOTHING).
        ensure_child(&pool, child, anchor_non_tracciato, "", "").await;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runs WHERE id = $1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 1);
    }

    /// Con l'ancora TRACCIATA (run padre reale) parent_run_id viene valorizzato:
    /// e' il collegamento padre<->figlio per audit e query.
    #[sqlx::test]
    async fn ensure_child_agent_run_collega_il_padre_tracciato(pool: sqlx::PgPool) {
        create_child_run_tables(&pool).await;
        let parent = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_runs (id, session_id, project_id, user_id) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(parent)
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .expect("padre");
        let child = Uuid::new_v4();
        ensure_child(&pool, child, parent, "google", "gemini-2.5-flash").await;
        let linked: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_run_id FROM agent_runs WHERE id = $1")
                .bind(child)
                .fetch_one(&pool)
                .await
                .expect("figlio");
        assert_eq!(linked, Some(parent));
    }

    /// Semina un padre tracciato su `provider` e due modelli 'light' capable
    /// (worker-provider + un alternativo) + il purpose tier-only. Ritorna
    /// (parent_run_id, definition_reviewer).
    async fn seed_review_routing(
        pool: &sqlx::PgPool,
        parent_provider: &str,
        catalog: &[(&str, &str)],
    ) -> (Uuid, SubagentDefinition) {
        create_child_run_tables(pool).await;
        // Il DB di test di questo modulo e' bare (#[sqlx::test] non applica le
        // migrazioni meta): creo le tabelle di routing come fanno gli altri test.
        crate::test_support::create_ai_price_catalog_table(pool).await;
        sqlx::query(
            "CREATE TABLE nexus_purpose_model ( \
                 purpose TEXT PRIMARY KEY, \
                 tier TEXT, \
                 required_capability TEXT, \
                 requires_tool_use BOOLEAN NOT NULL DEFAULT false \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_purpose_model");
        // `chat_sessions` (pin provider) vive nel DB PROGETTO. Nei test il pool e'
        // unico, quindi la creo qui: senza pin i test review/worker-standard
        // leggono NULL -> nessun pin (parita').
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS chat_sessions ( \
                 id UUID PRIMARY KEY, \
                 preferred_provider TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("create chat_sessions");
        let parent = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_runs (id, session_id, project_id, user_id, provider, model) \
             VALUES ($1, $2, $3, $4, $5, 'pm')",
        )
        .bind(parent)
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .bind(parent_provider)
        .execute(pool)
        .await
        .expect("padre");
        for (prov, model) in catalog {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                 (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, \
                  input_cost_per_million_tokens) VALUES ($1, $2, true, 'none', 'light', 1.0)",
            )
            .bind(prov)
            .bind(model)
            .execute(pool)
            .await
            .expect("catalog");
        }
        // Purpose tier-only: requires_tool_use=true -> ramo agentico (select_agentic_model).
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, requires_tool_use) \
             VALUES ('reviewer', 'light', true)",
        )
        .execute(pool)
        .await
        .expect("purpose");
        let def = SubagentDefinition {
            prompt_key: String::new(),
            tool_whitelist: vec![],
            model_purpose: "reviewer".to_string(),
            timeout_s: 0,
        };
        (parent, def)
    }

    /// C2: un sub-run `kind == "review"` risolve il modello ESCLUDENDO il provider
    /// del padre (worker) -> il giudice gira su un provider diverso.
    #[sqlx::test]
    async fn review_esclude_il_provider_del_worker(pool: sqlx::PgPool) {
        let (parent, def) =
            seed_review_routing(&pool, "alpha", &[("alpha", "a1"), ("beta", "b1")]).await;
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "review", &def, parent, Uuid::new_v4()).await;
        assert_eq!(provider, "beta", "il review deve evitare il provider del worker (alpha)");
    }

    /// C2 fallback: se il provider del worker e' l'UNICO capable, il review gira
    /// comunque su quel provider (preferenza forte, non hard filter).
    #[sqlx::test]
    async fn review_fallback_se_unico_provider_capable(pool: sqlx::PgPool) {
        let (parent, def) = seed_review_routing(&pool, "alpha", &[("alpha", "a1")]).await;
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "review", &def, parent, Uuid::new_v4()).await;
        assert_eq!(provider, "alpha", "unico provider capable -> fallback senza esclusione");
    }

    /// C2 parita': un kind NON review non esclude nulla (il provider del padre e'
    /// ammesso, comportamento invariato).
    #[sqlx::test]
    async fn non_review_non_esclude_il_provider_del_padre(pool: sqlx::PgPool) {
        // Solo 'alpha' capable; un worker (kind implement) risolve su 'alpha'
        // senza alcuna esclusione anche se il padre e' 'alpha'.
        let (parent, mut def) = seed_review_routing(&pool, "alpha", &[("alpha", "a1")]).await;
        def.model_purpose = "reviewer".to_string();
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "implement", &def, parent, Uuid::new_v4()).await;
        assert_eq!(provider, "alpha", "kind non-review: nessuna esclusione");
    }

    // ── PROPAGAZIONE PIN PROVIDER AI SUB-AGENTI WORKER ────────────────────────
    //
    // Un catalog reale per il pin: tier 'medium' + capability 'code' + tool_use
    // (il purpose worker 'frontend'). Colonna di catalog: `capabilities` jsonb
    // (la capability 'code' non ha colonna dedicata -> `capabilities @> ["code"]`).

    /// Riga di catalog completa per i test pin: provider/model, tier, capability
    /// 'code' (jsonb), costo (per il cost-first ASC). `enabled=true`.
    async fn seed_catalog_row(
        pool: &sqlx::PgPool,
        provider: &str,
        model: &str,
        tier: &str,
        cost: f64,
    ) {
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
              performance_tier, capabilities, input_cost_per_million_tokens) \
             VALUES ($1, $2, true, true, 'none', $3, '[\"code\"]'::jsonb, $4)",
        )
        .bind(provider)
        .bind(model)
        .bind(tier)
        .bind(cost)
        .execute(pool)
        .await
        .expect("catalog row");
    }

    /// Semina catalog + purpose worker + tabella chat_sessions per i test pin.
    /// `catalog`: (provider, model, tier, cost). Il purpose 'frontend' e' tier
    /// 'medium' + capability 'code' + tool_use (ramo agentico, cost-first). Ritorna
    /// il session_id (per il pin) e la definition worker.
    async fn seed_pin_routing(
        pool: &sqlx::PgPool,
        catalog: &[(&str, &str, &str, f64)],
    ) -> (Uuid, SubagentDefinition) {
        crate::test_support::create_ai_price_catalog_table(pool).await;
        sqlx::query(
            "CREATE TABLE nexus_purpose_model ( \
                 purpose TEXT PRIMARY KEY, \
                 tier TEXT, \
                 required_capability TEXT, \
                 requires_tool_use BOOLEAN NOT NULL DEFAULT false \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_purpose_model");
        sqlx::query(
            "CREATE TABLE chat_sessions ( \
                 id UUID PRIMARY KEY, \
                 preferred_provider TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("create chat_sessions");
        for (prov, model, tier, cost) in catalog {
            seed_catalog_row(pool, prov, model, tier, *cost).await;
        }
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
             VALUES ('frontend', 'medium', 'code', true)",
        )
        .execute(pool)
        .await
        .expect("purpose frontend");
        let def = SubagentDefinition {
            prompt_key: String::new(),
            tool_whitelist: vec![],
            model_purpose: "frontend".to_string(),
            timeout_s: 0,
        };
        (Uuid::new_v4(), def)
    }

    /// Inserisce una sessione con `preferred_provider` = pin.
    async fn seed_session_pin(pool: &sqlx::PgPool, session_id: Uuid, pin: Option<&str>) {
        sqlx::query("INSERT INTO chat_sessions (id, preferred_provider) VALUES ($1, $2)")
            .bind(session_id)
            .bind(pin)
            .execute(pool)
            .await
            .expect("session pin");
    }

    /// TEST 1 — pin propagato felice: pin='deepseek' + purpose worker medium/code/
    /// tool_use -> ritorna il modello DEEPSEEK del tier (non mistral, non il light).
    /// Senza pin vincerebbe mistral (piu' economico): il pin sposta la scelta.
    #[sqlx::test]
    async fn pin_propagato_sceglie_provider_pinnato(pool: sqlx::PgPool) {
        let (session, def) = seed_pin_routing(
            &pool,
            &[
                ("mistral", "mistral-medium", "medium", 1.0),
                ("deepseek", "deepseek-medium", "medium", 2.0),
                ("deepseek", "deepseek-flash", "light", 0.5),
            ],
        )
        .await;
        seed_session_pin(&pool, session, Some("deepseek")).await;
        let (provider, model) =
            resolve_worker_model(&pool, &pool, "implement", &def, Uuid::new_v4(), session).await;
        assert_eq!(provider, "deepseek", "il pin deve vincere il cost-first (mistral piu' economico)");
        assert_eq!(model, "deepseek-medium", "tier medium rispettato (non il light deepseek-flash)");
    }

    /// TEST 2 — pin non-capable -> degrado: il provider pinnato non ha un modello
    /// medium+code+tool_use -> NoCapableModel col pin -> fallback SENZA pin ->
    /// modello purpose normale (mistral). MAI ("","").
    #[sqlx::test]
    async fn pin_non_capable_degrada_al_purpose_normale(pool: sqlx::PgPool) {
        // deepseek ha solo un modello 'light' (tier sbagliato): non capable per
        // il tier 'medium' del purpose. mistral ha il medium capable.
        let (session, def) = seed_pin_routing(
            &pool,
            &[
                ("mistral", "mistral-medium", "medium", 1.0),
                ("deepseek", "deepseek-flash", "light", 0.5),
            ],
        )
        .await;
        seed_session_pin(&pool, session, Some("deepseek")).await;
        let (provider, model) =
            resolve_worker_model(&pool, &pool, "implement", &def, Uuid::new_v4(), session).await;
        assert_eq!(provider, "mistral", "pin non-capable -> fallback senza pin al purpose normale");
        assert_eq!(model, "mistral-medium");
        assert!(!provider.is_empty() && !model.is_empty(), "mai (\"\",\"\") se il purpose e' risolvibile");
    }

    /// TEST 3 — pin in cooldown -> degrado: il provider pinnato e' capable ma in
    /// cooldown (escluso dalla query) -> query pinnata vuota -> fallback senza pin
    /// (il figlio non resta bloccato).
    #[sqlx::test]
    async fn pin_in_cooldown_degrada_senza_bloccare(pool: sqlx::PgPool) {
        // Provider con nomi DEDICATI: il cooldown e' uno snapshot GLOBALE in-memory
        // condiviso tra i test paralleli, quindi non riuso 'deepseek'/'mistral'
        // (evita interferenze cross-test). Il pin va sul provider pinnato-in-cooldown.
        let pinned = "pincd-provider";
        let fallback = "fbcd-provider";
        let (session, def) = seed_pin_routing(
            &pool,
            &[
                (fallback, "fb-medium", "medium", 1.0),
                (pinned, "pin-medium", "medium", 2.0),
            ],
        )
        .await;
        seed_session_pin(&pool, session, Some(pinned)).await;
        // Cooldown in-memory sul provider pinnato: la WHERE `apply_cooldown` lo esclude.
        crate::provider_cooldown::put_provider_in_cooldown(pinned, Some(300));
        let (provider, model) =
            resolve_worker_model(&pool, &pool, "implement", &def, Uuid::new_v4(), session).await;
        // Rimuovo il cooldown per non contaminare altri test (stato globale).
        crate::provider_cooldown::remove_cooldown(pinned);
        assert_eq!(provider, fallback, "pin in cooldown -> fallback senza pin");
        assert_eq!(model, "fb-medium");
    }

    /// TEST 5 — Auto (nessun pin) bit-identico: preferred_provider NULL -> stesso
    /// (provider, model) del comportamento pre-pin (il cost-first sceglie mistral).
    #[sqlx::test]
    async fn nessun_pin_bit_identico_al_comportamento_attuale(pool: sqlx::PgPool) {
        let (session, def) = seed_pin_routing(
            &pool,
            &[
                ("mistral", "mistral-medium", "medium", 1.0),
                ("deepseek", "deepseek-medium", "medium", 2.0),
            ],
        )
        .await;
        seed_session_pin(&pool, session, None).await;
        let (provider, model) =
            resolve_worker_model(&pool, &pool, "implement", &def, Uuid::new_v4(), session).await;
        // Senza pin, il cost-first (ASC) sceglie il piu' economico: mistral.
        assert_eq!(provider, "mistral");
        assert_eq!(model, "mistral-medium");
    }

    /// TEST 4 — review ignora il pin: kind='review', pin = provider del padre ->
    /// il modello risolto NON e' del provider padre (l'esclusione giudice != worker
    /// vince sul pin). Nota: qui il purpose e' tier-only agentico su due provider.
    #[sqlx::test]
    async fn review_ignora_il_pin_esclusione_vince(pool: sqlx::PgPool) {
        let (parent, def) =
            seed_review_routing(&pool, "alpha", &[("alpha", "a1"), ("beta", "b1")]).await;
        // La sessione ha pinnato ALPHA (lo stesso del padre). Se il pin fosse
        // rispettato il review girerebbe su alpha; l'esclusione deve prevalere.
        let session = Uuid::new_v4();
        seed_session_pin(&pool, session, Some("alpha")).await;
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "review", &def, parent, session).await;
        assert_eq!(provider, "beta", "review: il pin e' ignorato, l'esclusione del padre vince");
    }

    /// `mark_run` chiude ENTRAMBE le righe del figlio (nexus_subagent_runs +
    /// gemella agent_runs) nella stessa statement, con status/metriche coerenti.
    #[sqlx::test]
    async fn mark_run_chiude_anche_la_riga_agent_runs(pool: sqlx::PgPool) {
        create_child_run_tables(&pool).await;
        let child = Uuid::new_v4();
        sqlx::query("INSERT INTO nexus_subagent_runs (id) VALUES ($1)")
            .bind(child)
            .execute(&pool)
            .await
            .expect("sub-run");
        ensure_child(&pool, child, Uuid::new_v4(), "mistral", "mistral-medium-3").await;
        mark_run(
            &pool,
            child,
            SubRunClosure {
                status: "completed",
                summary: "fatto",
                iterations: 7,
                tokens_prompt: 1000,
                tokens_completion: 200,
                cost_usd: 0.05,
                verdict: json!({"verdict": "completed", "success": true}),
            },
        )
        .await
        .expect("mark ok");
        let sub: (String, Option<String>, i32, Option<Value>) = sqlx::query_as(
            "SELECT status, final_summary, iterations, verdict \
             FROM nexus_subagent_runs WHERE id = $1",
        )
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("sub");
        assert_eq!(sub.0, "completed");
        assert_eq!(sub.1.as_deref(), Some("fatto"));
        assert_eq!(sub.2, 7);
        // Il verdetto strutturato (regola M) e' persistito SOLO sulla riga sub:
        // e' il canale del fan-in asincrono (poll) e dei coordinatori.
        let verdict = sub.3.expect("verdict persistito");
        assert_eq!(verdict["verdict"], json!("completed"));
        assert_eq!(verdict["success"], json!(true));
        let run: (String, Option<String>, i32, i32, bool) = sqlx::query_as(
            "SELECT status, final_answer, iteration_count, total_tokens, \
             completed_at IS NOT NULL FROM agent_runs WHERE id = $1",
        )
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("run gemello");
        assert_eq!(run.0, "completed");
        assert_eq!(run.1.as_deref(), Some("fatto"));
        assert_eq!(run.2, 7);
        assert_eq!(run.3, 1200, "total = prompt + completion");
        assert!(run.4, "completed_at valorizzato");
    }

    /// GUARD di forma (regola L): il blocco esito dei rami TERMINALI
    /// (`terminal_verdict`, senza `NativeRunOutcome`) e quello del ramo normale
    /// (`NativeRunOutcome::structured_verdict`) devono avere le STESSE chiavi —
    /// il coordinatore e il poll leggono un unico schema. Un campo aggiunto a
    /// uno solo dei due (com'era successo con `review` in Fase B) e' un drift
    /// silenzioso: questo test lo rende un errore di build.
    #[test]
    fn terminal_verdict_stessa_forma_di_structured_verdict() {
        use std::collections::BTreeSet;
        let outcome = crate::native_engine::NativeRunOutcome {
            completed: true,
            final_answer: None,
            stop_reason: None,
            provider_used: None,
            model_used: None,
            resume_at: None,
            iterations: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            total_cost: 0.0,
            user_intent: None,
            reasoning: None,
            messages_json: None,
            declared_outcome: None,
            review_verdict: None,
            error_class: None,
            forced_close_unverified: false,
            final_gate_passed: None,
            final_gate_unverified: None,
            final_gate_failed_pending: false,
        };
        let keys = |v: &Value| -> BTreeSet<String> {
            v.as_object()
                .expect("blocco esito e' un oggetto")
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(
            keys(&terminal_verdict("failed", "engine_error")),
            keys(&outcome.structured_verdict()),
            "terminal_verdict e structured_verdict hanno chiavi divergenti"
        );
    }
}
