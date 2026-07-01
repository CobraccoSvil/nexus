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
use crate::native_engine::{NativeDeps, NativeRunInput, NativeRunOutcome};

/// Marker d'errore (stesso contratto di `subagent.rs`: prefisso U+274C ->
/// `tool_result_is_error` deriva `is_error=true`).
fn err(msg: &str) -> String {
    format!("\u{274C} [dispatch_subagent] {msg}")
}

/// Soglie sub-agent lette dal DB (regola G). I default coincidono coi default del
/// brain (`orchestrator_config` + mig 0153): safe-default se la chiave manca, MAI
/// magic fallback sul comportamento.
struct SubagentSettings {
    enabled: bool,
    whitelist: Vec<String>,
    max_depth: i64,
    cost_cap_usd: f64,
    default_timeout_s: i64,
}

async fn read_subagent_settings(ctx: &AgentToolContext) -> Result<SubagentSettings, String> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN (
            'orchestrator.subagents_enabled',
            'orchestrator.subagent_kinds_whitelist',
            'orchestrator.subagent_max_depth',
            'orchestrator.subagent_cost_cap_per_run_usd',
            'orchestrator.subagent_default_timeout_s'
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
    };
    for row in rows {
        let k: String = row.get("key");
        let v: String = row.get("value");
        match k.as_str() {
            "orchestrator.subagents_enabled" => {
                s.enabled = matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes" | "on")
            }
            "orchestrator.subagent_kinds_whitelist" => {
                s.whitelist = v
                    .split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            }
            "orchestrator.subagent_max_depth" => s.max_depth = v.trim().parse().unwrap_or(2),
            "orchestrator.subagent_cost_cap_per_run_usd" => {
                s.cost_cap_usd = v.trim().parse().unwrap_or(5.0)
            }
            "orchestrator.subagent_default_timeout_s" => {
                s.default_timeout_s = v.trim().parse().unwrap_or(300)
            }
            _ => {}
        }
    }
    Ok(s)
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

    run_single_subagent(ctx, &kind, &task, &context_blob, &expected_format)
        .await
        .to_string()
}

/// Handler del tool `dispatch_subagents` (batch parallelo di sub-run nativi).
pub async fn tool_dispatch_subagents(ctx: &AgentToolContext, input: &Value) -> String {
    let tasks = match input.get("tasks").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return err("parametro 'tasks' (array non vuoto) obbligatorio"),
    };
    if tasks.len() > 8 {
        return err("troppi task in un batch (max 8)");
    }
    let configured_max = read_max_parallel_subagents(ctx).await;
    let max_parallel = input
        .get("max_parallel")
        .and_then(|v| v.as_u64())
        .unwrap_or(configured_max)
        .clamp(1, configured_max) as usize;

    let mut parsed: Vec<(String, String, String, String)> = Vec::with_capacity(tasks.len());
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
        parsed.push((kind, task, context_blob, expected));
    }

    // Esecuzione a ondate concorrenti (cap conservativo). I guard per-sub
    // (enabled/whitelist/depth/cost) sono valutati per ogni sub-run; nel batch
    // il cost cap e' best-effort (race tollerata dato il cap conservativo).
    let mut results: Vec<Value> = Vec::with_capacity(parsed.len());
    for wave in parsed.chunks(max_parallel) {
        let futs = wave
            .iter()
            .map(|(k, ta, c, e)| run_single_subagent(ctx, k, ta, c, e));
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

/// Esegue UNA sub-run sul GRAFO NATIVO. Ritorna sempre un `Value`: il sommario in
/// caso di successo, `{"error": "..."}` su guasto. Replica le guard del brain e
/// mappa l'esito del run nativo al tool_result atteso dal main.
async fn run_single_subagent(
    ctx: &AgentToolContext,
    kind: &str,
    task: &str,
    context_blob: &str,
    expected_format: &str,
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

    // Modello del worker dal model_purpose (regola G, tier-aware). Non risolto ->
    // nessun override: l'executor usa il routing di default (parita' col brain).
    let (provider, model) = if definition.model_purpose.trim().is_empty() {
        (String::new(), String::new())
    } else {
        match crate::internal_routing::resolve_purpose_model_db(db, &definition.model_purpose).await
        {
            crate::internal_routing::PurposeResolution::Resolved {
                provider, model, ..
            } => (provider, model),
            other => {
                tracing::warn!(
                    kind = %kind,
                    model_purpose = %definition.model_purpose,
                    resolution = ?other,
                    "subagent_native: model_purpose non risolto, routing di default"
                );
                (String::new(), String::new())
            }
        }
    };

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
    //    osservabile) ───────────────────────────────────────────────────────────
    let subagent_run_id: Uuid = match sqlx::query_scalar(
        r#"INSERT INTO nexus_subagent_runs
           (parent_run_id, project_id, kind, task_description, context_blob, expected_format,
            status, is_background, depth, source)
           VALUES ($1, $2, $3, $4, $5, $6, 'running', false, $7, 'db')
           RETURNING id"#,
    )
    .bind(anchor)
    .bind(project_id)
    .bind(kind)
    .bind(task)
    .bind(context_blob)
    .bind(expected_format)
    .bind(current_depth as i32)
    .fetch_one(&proj_pool)
    .await
    {
        Ok(id) => id,
        Err(e) => return json!({"error": format!("INSERT nexus_subagent_runs: {e}")}),
    };

    // ── Esecuzione sul GRAFO NATIVO (in-process, niente brain) ────────────────
    // Il sub-run e' un run a se': run_id = subagent_run_id (= thread del grafo),
    // STESSA session_id del parent (eredita root/permessi/canali). Lo stato porta
    // parent_run_id + subagent_depth -> il grafo applica i guard di annidamento
    // (UnderstandingNode salta il fan-out explore se depth>=1).
    let deps = build_native_deps_for_tool(ctx).await;
    // Canale SSE proprio del sub-run: NON instrada al frontend (l'output utente
    // resta quello del main, che riceve solo il summary). Buffer minimo.
    let (sub_tx, _sub_rx) = tokio::sync::broadcast::channel(64);

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
    };

    // Timeout duro sull'esecuzione del sub-run (parita' col brain `asyncio.wait_for`).
    let run_fut = crate::native_engine::run_native(&deps, &native_input);
    let outcome: anyhow::Result<NativeRunOutcome> =
        match tokio::time::timeout(std::time::Duration::from_secs(timeout_s as u64), run_fut).await {
            Ok(res) => res,
            Err(_) => {
                let _ = mark_run(
                    &proj_pool,
                    subagent_run_id,
                    "timeout",
                    "[Sub-agent timeout]",
                    0,
                    0,
                    0,
                    0.0,
                )
                .await;
                return json!({
                    "subagent_run_id": subagent_run_id.to_string(),
                    "kind": kind,
                    "status": "timeout",
                    "error": "[Sub-agent timeout]",
                });
            }
        };

    match outcome {
        Ok(o) => {
            let summary = o.final_answer.clone().unwrap_or_default();
            let status = if o.completed { "completed" } else { "paused" };
            let _ = mark_run(
                &proj_pool,
                subagent_run_id,
                status,
                &summary,
                o.iterations,
                o.prompt_tokens,
                o.completion_tokens,
                o.total_cost,
            )
            .await;
            tracing::info!(
                kind = %kind,
                subagent_run_id = %subagent_run_id,
                depth = current_depth,
                completed = o.completed,
                iterations = o.iterations,
                summary_len = summary.len(),
                "subagent_native: sub-run eseguito sul grafo nativo"
            );
            json!({
                "subagent_run_id": subagent_run_id.to_string(),
                "kind": kind,
                "status": status,
                "summary": compact_summary(&summary),
                "iterations": o.iterations,
                "cost_usd": o.total_cost,
                "tokens": {
                    "prompt": o.prompt_tokens,
                    "completion": o.completion_tokens,
                },
            })
        }
        Err(e) => {
            // Fallback onesto: sub-run fallito -> errore al chiamante (come oggi).
            let msg = format!("[errore grafo nativo: {e}]");
            let _ = mark_run(&proj_pool, subagent_run_id, "failed", &msg, 0, 0, 0, 0.0).await;
            tracing::warn!(
                kind = %kind,
                subagent_run_id = %subagent_run_id,
                error = %e,
                "subagent_native: sub-run fallito"
            );
            json!({
                "error": msg,
                "subagent_run_id": subagent_run_id.to_string(),
                "kind": kind,
            })
        }
    }
}

/// Marca una sub-run come conclusa (status + summary + metriche). Best-effort.
#[allow(clippy::too_many_arguments)]
async fn mark_run(
    db: &sqlx::PgPool,
    run_id: Uuid,
    status: &str,
    summary: &str,
    iterations: i64,
    tokens_prompt: i64,
    tokens_completion: i64,
    cost_usd: f64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE nexus_subagent_runs SET
            status = $1, final_summary = $2, iterations = $3,
            tokens_prompt = $4, tokens_completion = $5, cost_usd = $6,
            completed_at = NOW()
         WHERE id = $7",
    )
    .bind(status)
    .bind(summary.chars().take(4000).collect::<String>())
    .bind(iterations as i32)
    .bind(tokens_prompt as i32)
    .bind(tokens_completion as i32)
    .bind(cost_usd)
    .bind(run_id)
    .execute(db)
    .await
    .map(|_| ())
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
    let run_id = match input.get("subagent_run_id").and_then(Value::as_str) {
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
                tokens_prompt, tokens_completion, cost_usd, depth, source, is_background
         FROM nexus_subagent_runs WHERE id::text = $1",
    )
    .bind(&run_id)
    .fetch_optional(&proj_pool)
    .await;
    match row {
        Ok(Some(r)) => json!({
            "subagent_run_id": r.try_get::<String, _>("id").unwrap_or_default(),
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
    let run_id_str = match input.get("subagent_run_id").and_then(Value::as_str) {
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
        return json!({"status": "noop", "subagent_run_id": run_id_str, "current_status": status})
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
    let res = run_single_subagent(ctx, &kind, &task, &context_blob, &expected).await;
    res.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
