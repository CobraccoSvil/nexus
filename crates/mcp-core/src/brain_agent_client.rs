//! Client HTTP verso il brain (Python/LangGraph) per l'agent orchestration.
//!
//! Quando `USE_BRAIN_ORCHESTRATOR=1`, `spawn_agent_run` in `chat_messages.rs`
//! invoca `run_via_brain` al posto di `AgentLoop::run`. Il brain gestisce
//! il loop tool_use, rimbalzando sui tool via il ToolRunner gRPC esposto da
//! questo stesso mcp-core (closed loop).
//!
//! Traduce gli eventi SSE del brain (`assistant_delta`, `tool_use`,
//! `tool_result`, `end_turn`, `error`) in `AgentStepEvent` sullo stesso
//! broadcast channel usato dalla UI — la web-ide non vede differenze.

use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::agent_tools::AGENT_TOOLS_JSON;
use crate::agent_types::{
    AgentMetaStep, AgentRunResult, AgentRunStatus, AgentStep, AgentStepEvent, AgentStepStatus,
};

/// URL REST del brain (FastAPI). Default allineato al port di `--rest` del brain.
/// Gerarchia: env var BRAIN_REST_URL (override emergenza) > hardcoded.
/// Nota: il valore canonico e' in DB (settings.brain_rest_url), ma questa
/// funzione non ha accesso al PgPool. I call-site con accesso al DB dovrebbero
/// preferire `settings::get_setting(db, "brain_rest_url")`.
fn brain_rest_url() -> String {
    std::env::var("BRAIN_REST_URL")
        .or_else(|_| std::env::var("NEURAL_CORE_REST_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string())
}

/// Costruisce il JSON tools da inviare al brain applicando la discovery mode.
///
/// Logica:
/// - Legge `mcp_tool_search_hard_limit` dal DB (default 20).
/// - Conta i tool MCP esterni abilitati e accessibili a user/project.
/// - Se count < soglia: include le definizioni dei tool MCP nel payload
///   (il brain li vede direttamente senza dover cercare).
/// - Se count >= soglia: invia solo AGENT_TOOLS_JSON (che contiene già
///   `nexus_mcp_tool_search` + `nexus_mcp_tool_call`). Il brain usa la
///   ricerca semantica per scoprire i tool a runtime — discovery mode.
/// Fallback hardcoded dei tool essenziali per modelli o-series (o1/o3/o4-mini).
/// Questi modelli non supportano `tool_choice` e con 40+ tool tendono a
/// "narrare" invece di fare tool call. La soluzione e' passare solo i tool
/// fondamentali + `nexus_mcp_tool_search`/`nexus_mcp_tool_call` per la
/// discovery on-demand dei tool non inclusi.
///
/// La sorgente autoritativa e' `settings.automation.o_series_essential_tools`
/// (CSV). Letta da `load_o_series_essential_tools()`. Questo fallback copre
/// il caso DB down.
const O_SERIES_ESSENTIAL_TOOLS_FALLBACK: &[&str] = &[
    // Core lettura
    "read_file",
    "read_file_lines",
    "list_files",
    "search_in_files",
    // Core scrittura
    "write_file",
    "edit_file",
    "run_command",
    "fs_mkdir",
    "delete_file",
    // Git essenziale
    "git_status",
    "git_commit",
    // Test
    "run_tests",
    // Discovery (il modello scopre tool aggiuntivi a runtime)
    "nexus_mcp_tool_search",
    "nexus_mcp_tool_call",
    // Ricerca semantica
    "search_codebase_semantic",
    // Generazione documenti professionali (.docx). Audit 27/05/2026: senza
    // questo, il pannello DOCUMENTI invocava nexus_doc_generate ma l'agente
    // non lo trovava nei 15 tool del safety-net o-series e rispondeva
    // a parole "procedo con la generazione" senza generare nulla.
    "nexus_doc_generate",
    "nexus_doc_update",
    "nexus_doc_list",
    "nexus_doc_status",
];

/// Ritorna `true` se il modello e' della serie reasoning OpenAI (o1/o3/o4-mini).
/// Questi modelli non supportano il parametro `tool_choice` e hanno bisogno
/// di un set ridotto di tool + istruzioni esplicite nel system prompt.
///
/// Esposta come `is_o_series_model_pub` per uso in `chat_messages.rs`.
fn is_o_series_model(model: &str) -> bool {
    let m = model.to_lowercase();
    ["o1", "o1-mini", "o1-preview", "o3", "o3-mini", "o4-mini"]
        .iter()
        .any(|prefix| m == *prefix || m.starts_with(&format!("{}-", prefix)))
}

/// Wrapper pubblico di `is_o_series_model` per uso in altri moduli (es. chat_messages).
pub fn is_o_series_model_pub(model: &str) -> bool {
    is_o_series_model(model)
}

/// Legge la whitelist tool essenziali per o-series da `settings`.
/// Ritorna sempre una lista non vuota: in caso di DB down o chiave assente,
/// usa il fallback hardcoded `O_SERIES_ESSENTIAL_TOOLS_FALLBACK`.
async fn load_o_series_essential_tools(db: &PgPool) -> Vec<String> {
    let csv: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'automation.o_series_essential_tools'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    match csv {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => O_SERIES_ESSENTIAL_TOOLS_FALLBACK
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// M16 — True se il setting `agent.tools.discovery_first_enabled` e' attivo.
/// Default false (opt-in). DB-driven (regola G), niente fallback hardcoded ON.
async fn is_discovery_first_enabled(db: &PgPool) -> bool {
    let v: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'agent.tools.discovery_first_enabled' LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    v.map(|s| {
        !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no" | ""
        )
    })
    .unwrap_or(false)
}

/// M16 — Whitelist dei tool del primo turno discovery-only (CSV in
/// `agent.tools.discovery_first_whitelist`). Fallback ai 2 tool di discovery.
async fn load_discovery_tools(db: &PgPool) -> Vec<String> {
    let csv: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'agent.tools.discovery_first_whitelist' LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    match csv {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => vec![
            "nexus_mcp_tool_search".to_string(),
            "nexus_mcp_tool_call".to_string(),
        ],
    }
}

/// Filtra il JSON tools lasciando solo quelli il cui `name` e' nella whitelist.
fn filter_tools_by_whitelist(tools: Value, whitelist: &[String]) -> Value {
    let arr = match tools.as_array() {
        Some(a) => a,
        None => return Value::Array(Vec::new()),
    };
    let filtered: Vec<Value> = arr
        .iter()
        .filter(|t| {
            let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
            whitelist.iter().any(|w| w == name)
        })
        .cloned()
        .collect();
    tracing::info!(
        "filter_tools_by_whitelist: {} tool totali -> {} filtrati (whitelist={} voci)",
        arr.len(),
        filtered.len(),
        whitelist.len(),
    );
    Value::Array(filtered)
}

/// Fallback hardcoded della whitelist read-only per modalita' `study`.
/// USATA SOLO se `settings.automation.study_mode_readonly_tools` non e'
/// leggibile dal DB (DB down all'avvio). Mantenuta minima: l'admin deve
/// poter ampliare la lista via UI senza redeploy.
///
/// La sorgente autoritativa e' `settings.automation.study_mode_readonly_tools`
/// (mig 0132): CSV di nomi tool. Letta da `load_study_mode_readonly_tools()`.
const STUDY_MODE_READONLY_TOOLS_FALLBACK: &[&str] = &[
    "read_file",
    "read_file_lines",
    "list_files",
    "search_in_files",
    "search_codebase_semantic",
    "get_project_structure",
    "git_status",
    "git_log",
    "git_diff",
    "nexus_mcp_tool_search",
];

/// Legge la whitelist read-only per study mode da `settings`.
/// Ritorna sempre una lista non vuota: in caso di DB down o chiave assente,
/// usa il fallback hardcoded `STUDY_MODE_READONLY_TOOLS_FALLBACK`.
async fn load_study_mode_readonly_tools(db: &PgPool) -> Vec<String> {
    let csv: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'automation.study_mode_readonly_tools'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    match csv {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => {
            tracing::warn!(
                "load_study_mode_readonly_tools: settings.automation.study_mode_readonly_tools assente, uso fallback ({} tool)",
                STUDY_MODE_READONLY_TOOLS_FALLBACK.len(),
            );
            STUDY_MODE_READONLY_TOOLS_FALLBACK
                .iter()
                .map(|s| s.to_string())
                .collect()
        }
    }
}

/// Filtra il JSON di tool in base al `automation_mode`.
///
/// - `Automatic`: nessun filtro (l'utente vuole massima autonomia)
/// - `Confirm`: nessun filtro lato Rust — il gating avviene a livello di
///   HITL nel brain Python (`AwaitingConfirmation` prima di edit/write)
/// - `Study`: solo tool read-only (filtro difensivo basato sulla whitelist
///   passata in `readonly_tools_whitelist` — letta da DB dal caller).
fn filter_tools_by_automation_mode(
    tools: Value,
    mode: &crate::orchestrator::AutomationMode,
    readonly_tools_whitelist: &[String],
) -> Value {
    use crate::orchestrator::AutomationMode;
    match mode {
        AutomationMode::Automatic | AutomationMode::Confirm => tools,
        AutomationMode::Study => {
            // In study, l'agente puo' SOLO leggere/analizzare. Filtriamo
            // qualunque tool non esplicitamente in whitelist.
            let arr = match tools.as_array() {
                Some(a) => a,
                None => return Value::Array(Vec::new()),
            };
            let filtered: Vec<Value> = arr
                .iter()
                .filter(|t| {
                    let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    readonly_tools_whitelist.iter().any(|w| w == name)
                })
                .cloned()
                .collect();
            tracing::info!(
                "automation_mode=study: filtrati {} tool → {} read-only esposti (whitelist={} tool)",
                arr.len(),
                filtered.len(),
                readonly_tools_whitelist.len(),
            );
            Value::Array(filtered)
        }
    }
}

pub async fn build_tools_json_for_agent(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    automation_mode: &crate::orchestrator::AutomationMode,
    provider: &str,
    model: &str,
) -> Value {
    // Legge soglia dal DB
    let hard_limit: i64 = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'mcp_tool_search_hard_limit'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse().ok())
    .unwrap_or(20);

    // Conta tool MCP esterni abilitati e accessibili
    let mcp_tool_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
           FROM mcp_servers s
           JOIN mcp_server_tools t ON t.server_id = s.id
           WHERE s.enabled = true
             AND s.transport != 'builtin'
             AND (
               s.scope = 'global'
               OR (s.scope = 'user'    AND s.user_id    = $1)
               OR (s.scope = 'project' AND s.project_id = $2)
             )"#,
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or(0_i64);

    let base_tools: Value = serde_json::from_str(AGENT_TOOLS_JSON).unwrap_or_else(|_| json!([]));

    let full_tools = if mcp_tool_count < hard_limit && mcp_tool_count > 0 {
        // Catalogo piccolo: include le definizioni MCP direttamente
        tracing::debug!(
            "build_tools_json: {} tool MCP < soglia {}, includo definizioni dirette",
            mcp_tool_count,
            hard_limit
        );
        let mcp_tools =
            crate::mcp_connectors::load_mcp_tools_for_agent(db, user_id, Some(project_id)).await;
        if mcp_tools.is_empty() {
            base_tools
        } else {
            let mut all = base_tools.as_array().cloned().unwrap_or_default();
            all.extend(mcp_tools);
            json!(all)
        }
    } else {
        // Discovery mode (catalogo vuoto o >= soglia): solo AGENT_TOOLS_JSON
        // nexus_mcp_tool_search è già incluso — il brain lo usa per scoprire i tool
        if mcp_tool_count >= hard_limit {
            tracing::debug!(
                "build_tools_json: discovery mode ({} tool MCP >= soglia {})",
                mcp_tool_count,
                hard_limit
            );
        }
        base_tools
    };

    // Gating finale per automation_mode: in `study` filtriamo a solo
    // read-only. `confirm` e `automatic` passano la lista intera.
    // La whitelist e' letta da `settings.automation.study_mode_readonly_tools`
    // (mig 0132) — niente lista hardcoded nel codice (regola G CLAUDE.md).
    let readonly_whitelist = load_study_mode_readonly_tools(db).await;
    let after_mode =
        filter_tools_by_automation_mode(full_tools, automation_mode, &readonly_whitelist);

    // M16 — Progressive tool disclosure: se discovery-first e' attivo, il set
    // INIZIALE passato al brain contiene SOLO i 2 tool di discovery
    // (nexus_mcp_tool_search/call). Il brain (state.py + nodes.py) intercetta i
    // risultati di search e inietta i tool scoperti come native per il turno
    // successivo. Lista minima -> meno MALFORMED su Gemini, prompt piu' snello.
    // Priorita' sopra il filtro o-series. tool_choice resta 'auto'.
    if is_discovery_first_enabled(db).await {
        let discovery = load_discovery_tools(db).await;
        tracing::info!(
            "build_tools_json: discovery-first attivo (model='{}') — primo turno {} tool di discovery",
            model, discovery.len()
        );
        return filter_tools_by_whitelist(after_mode, &discovery);
    }

    // Riduzione tool per modelli o-series (o1/o3/o4-mini): questi modelli non
    // supportano `tool_choice` e con 40+ tool tendono a narrare senza fare
    // tool call. Passiamo solo i tool essenziali + nexus_mcp_tool_search per
    // la discovery on-demand dei tool rimanenti.
    if is_o_series_model(model) {
        let essential = load_o_series_essential_tools(db).await;
        tracing::info!(
            "build_tools_json: modello o-series '{}' rilevato — riduzione a {} tool essenziali + discovery",
            model, essential.len()
        );
        return filter_tools_by_whitelist(after_mode, &essential);
    }

    // ── ADR 0016 Fase A.2: tool discovery on-demand per TUTTI i modelli ────
    // Se `agent.tools.discovery_enabled` e' true, il set inline contiene SOLO
    // i tool core (`agent.tools.inline_core_whitelist`). I rimanenti (~66) sono
    // raggiungibili via nexus_mcp_tool_search / nexus_mcp_tool_call (gia' inline).
    // Risparmio ~14k token/turno di tool definitions sui 19k totali.
    if is_a2_discovery_enabled(db).await {
        let core = load_inline_core_whitelist(db).await;
        if !core.is_empty() {
            tracing::info!(
                "build_tools_json: A.2 discovery on-demand attivo (model='{}') — {} tool core inline",
                model, core.len()
            );
            return filter_tools_by_whitelist(after_mode, &core);
        }
    }

    after_mode
}

/// Settings `agent.tools.discovery_enabled` (default false: opt-in).
async fn is_a2_discovery_enabled(db: &PgPool) -> bool {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.tools.discovery_enabled'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|v| v.trim().eq_ignore_ascii_case("true"))
    .unwrap_or(false)
}

/// Whitelist tool core inline (Fase A.2). CSV in
/// `agent.tools.inline_core_whitelist`. Lista vuota -> disabilita la riduzione.
async fn load_inline_core_whitelist(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.tools.inline_core_whitelist'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|csv| {
        csv.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// Riconosce gli errori del provider AI che indicano "credito esaurito" / "quota
/// superata" / "rate limit prolungato": condizioni non recuperabili in pochi minuti.
/// In questi casi il provider va messo in cooldown lungo (ore) per evitare di
/// continuare a chiamarlo a vuoto, e Nexus deve scalare automaticamente al
/// provider successivo nella gerarchia.
/// Severita' del cooldown da applicare al provider in base al tipo di errore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CooldownKind {
    /// 6 ore: errori non recuperabili in pochi minuti (billing/quota esaurita).
    Long,
    /// 60 secondi: errori transient (5xx, rate limit short-window).
    Short,
}

/// Classifica un errore del provider e suggerisce il tipo di cooldown.
/// Ritorna `(error_class_normalizzato, kind, human_reason)`.
/// La priorita': prima legge `error_class` esplicito (dal brain Python via
/// `brain/providers/error_handler.py`), poi cade sul pattern matching su msg
/// (per messaggi italiani/altri provider).
fn classify_provider_error(
    error_class: Option<&str>,
    msg: &str,
) -> Option<(&'static str, CooldownKind, &'static str)> {
    // Marker testuali di quota/credito esaurito. Un HTTP 429 e' AMBIGUO: puo'
    // essere un rate-limit transitorio (finestra di richieste, cooldown breve)
    // oppure quota/credito esaurito (cooldown lungo come credit_balance_too_low).
    // La distinzione si fa SOLO sul contenuto del messaggio: se compare uno di
    // questi marker e' billing/quota, altrimenti rate-limit transitorio.
    let lower = msg.to_lowercase();
    let has_billing_marker = (lower.contains("credit balance") && lower.contains("too low"))
        || lower.contains("insufficient_quota")
        || lower.contains("insufficient quota")
        || lower.contains("exceeded your current quota")
        || lower.contains("billing_hard_limit_reached")
        || lower.contains("billing hard limit")
        || lower.contains("plans & billing")
        || lower.contains("upgrade or purchase credits")
        || lower.contains("upgrade or purchase")
        || lower.contains("billing required")
        || lower.contains("payment required")
        || lower.contains("account is not active")
        || lower.contains("no credits")
        || lower.contains("quota ai esaurita")
        || lower.contains("credito insufficiente");

    const BILLING_REASON: &str = "Quota AI esaurita o credito insufficiente";

    // Step 1: error_class esplicito propagato dal brain.
    match error_class {
        Some("billing_error")
        | Some("billing_required")
        | Some("quota_exceeded")
        | Some("credit_balance_too_low")
        | Some("insufficient_quota") => {
            return Some(("billing_error", CooldownKind::Long, BILLING_REASON));
        }
        Some("rate_limit") => {
            // Anche con error_class=rate_limit esplicito, un 429 puo' in realta'
            // essere quota/credito esaurito: il brain (o un altro provider) puo'
            // aver mappato il 429 a rate_limit senza esaminare il messaggio.
            // Se il testo contiene marker billing, promuovi a cooldown lungo,
            // altrimenti resta rate-limit transitorio (cooldown breve).
            if has_billing_marker {
                return Some(("billing_error", CooldownKind::Long, BILLING_REASON));
            }
            return Some(("rate_limit", CooldownKind::Short, "Rate limit raggiunto"));
        }
        Some("overloaded")
        | Some("provider_error")
        | Some("server_error")
        | Some("service_unavailable")
        | Some("bad_gateway")
        | Some("internal_server_error") => {
            return Some((
                "provider_error",
                CooldownKind::Short,
                "Provider sovraccarico o errore temporaneo",
            ));
        }
        _ => {}
    }

    // Step 2: pattern matching sul testo (it/en).
    if has_billing_marker {
        return Some(("billing_error", CooldownKind::Long, BILLING_REASON));
    }
    if lower.contains("rate limit")
        || lower.contains("limite di richieste")
        || lower.contains("too many requests")
        || lower.contains("429")
    {
        return Some(("rate_limit", CooldownKind::Short, "Rate limit raggiunto"));
    }
    if lower.contains("overloaded")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("internal server error")
        || lower.contains("gateway timeout")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
    {
        return Some((
            "provider_error",
            CooldownKind::Short,
            "Provider sovraccarico o errore temporaneo",
        ));
    }
    None
}

/// Esegue un turno agente tramite `POST brain/agent/run/stream` e
/// ri-emette gli eventi SSE sul broadcast channel del run.
///
/// Ritorna un `AgentRunResult` compatibile con quello prodotto da
/// `AgentLoop::run`, in modo che il chiamante (`spawn_agent_run`)
/// possa persistere su DB l'esito senza differenze.
/// Esegue il run agente via brain (SSE streaming).
///
/// `sse_max_silence_secs`: soglia silenzio SSE in secondi. Letta dal caller
/// via `settings.routing.sse_heartbeat_max_silence_secs` (mig 0132).
/// Tipico: 120s. Range pratico [60, 600]. Il brain emette ping ogni 30s.
///
/// `emit_final_event`: se true, al termine del tentativo emette
/// `is_final: true` sul broadcast (default per caller singolo-shot tipo
/// resend). I caller con retry loop (spawn_agent_run) lo passano `false`
/// per evitare che il frontend chiuda lo stream SSE dopo il primo tentativo
/// fallito; quei caller emettono is_final manualmente dopo il break.
pub async fn run_via_brain(
    run_id: Uuid,
    session_id: Uuid,
    provider: String,
    model: String,
    system_text: String,
    initial_msg: String,
    step_tx: broadcast::Sender<AgentStepEvent>,
    conversation_history: Vec<serde_json::Value>,
    tools_json: Value,
    sse_max_silence_secs: u64,
    emit_final_event: bool,
    automation_mode: String,
    // Intent gia' RISOLTO a monte (oggi: risposta dell'utente a una
    // disambiguazione, resolve_disambiguation_reply). Quando presente il
    // router_node del brain lo usa al posto di ri-classificare il prompt:
    // la lettera secca "A" verrebbe ri-marcata 'chat' (prompt_len=1) perdendo
    // l'intent scelto dall'utente. None = classificazione brain normale.
    intent_hint: Option<String>,
    // Pool DB: serve per marcare `generation_ended_at` all'istante dell'end_turn
    // (mig 0388), cosi' la finestra reflection/learner post-end_turn non blocca
    // la sessione col guard anti-run-concorrente (409). Solo questo UPDATE leggero.
    db: PgPool,
) -> AgentRunResult {
    let run_id_str = run_id.to_string();
    let url = format!(
        "{}/agent/run/stream",
        brain_rest_url().trim_end_matches('/')
    );

    let mut body = json!({
        "thread_id": run_id_str,
        "prompt": initial_msg,
        "behavior_mode": "bilanciata",
        "tools_json": tools_json,
        "system_text": system_text,
        "session_id": session_id.to_string(),
        "provider_override": provider,
        "model_override": model,
        "conversation_history": conversation_history,
        "run_id": run_id_str,
        "automation_mode": automation_mode,
    });
    if let Some(hint) = intent_hint.as_deref() {
        body["intent_hint"] = json!(hint);
    }

    // Solo connect_timeout: il timeout sulla connessione TCP iniziale.
    // Il precedente timeout monolitico di 1200s sull'intera request impediva
    // run legittimamente lunghi (build Rust, Playwright su suite grandi, ecc.).
    // Il controllo di attivita' e' ora gestito dal timeout per-silence nel
    // loop SSE: se il brain non emette eventi (inclusi ping heartbeat) per
    // MAX_SILENCE_SECS secondi, il loop esce considerando il brain bloccato.
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return fail_result(
                &run_id_str,
                &provider,
                &model,
                format!("reqwest build: {e}"),
            )
        }
    };

    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("brain /agent/run/stream POST fallito: {e}");
            return fail_result(&run_id_str, &provider, &model, format!("POST brain: {e}"));
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        tracing::error!("brain /agent/run/stream status={} body={}", status, text);
        return fail_result(
            &run_id_str,
            &provider,
            &model,
            format!("brain status {}: {}", status, text),
        );
    }

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut final_answer = String::new();
    let mut last_text_segment = String::new();
    let mut steps: Vec<AgentStep> = Vec::new();
    let mut iteration: u32 = 0;
    let mut ended = false;
    let mut last_error: Option<String> = None;
    let mut last_error_class: Option<String> = None;
    let mut last_stop_reason: Option<String> = None;
    let mut acc_prompt_tokens: u32 = 0;
    let mut acc_completion_tokens: u32 = 0;
    let mut acc_total_tokens: u32 = 0;
    let mut acc_total_cost: f64 = 0.0;
    // Token prompt dell'ultima iterazione (per context ratio UI, non billing)
    let mut last_prompt_tokens: Option<u32> = None;
    // B5: metadata routing propagati dal brain Python nell'evento end_turn
    let mut nexus_task_type: Option<String> = None;
    let mut nexus_agent_type: Option<String> = None;
    // Macchina a stati di terminazione (mig 0386): segnale autoritativo che il run
    // e' stato chiuso da un abort anti-loop senza verifica. Propagato dal brain
    // nell'evento end_turn; sopravvive alla riscrittura di stop_reason a "end_turn"
    // operata dal final_gate sul ramo forced_close.
    let mut forced_close_unverified = false;
    // Macchina a stati (mig 0386): verifica E2E (final_gate) superata -> esito
    // canonico CompletedVerified. Propagato dal brain nell'evento end_turn.
    let mut final_gate_passed = false;
    // WAVE 3.2: esito DICHIARATO dal modello via task_complete (outcome + summary),
    // propagato nell'end_turn. `blocked`/`needs_input` mappano su BlockedNeedsInput;
    // il summary diventa la risposta se il modello non ha prodotto testo.
    let mut declared_outcome: Option<String> = None;
    let mut declared_summary: Option<String> = None;
    // Provider/model EFFETTIVI dell'ultima iterazione, propagati dal brain
    // nell'evento end_turn quando avviene un cascade fallback sticky intra-run
    // (es. deepseek -> google/gemini-2.5-pro). Default al provider/model iniziale
    // della routing decision; sovrascritti se il brain segnala il fallback.
    // Il risultato finale usa QUESTI valori cosi' il messaggio assistant mostra
    // il modello reale che ha prodotto la risposta, non quello iniziale.
    let mut effective_provider = provider.clone();
    let mut effective_model = model.clone();

    // Timeout per-silence: se il brain non emette alcun chunk SSE (inclusi i
    // ping heartbeat ogni ~30s) per `sse_max_silence_secs` secondi, il run
    // viene considerato bloccato e il loop esce. Valore letto dal caller via
    // `settings.routing.sse_heartbeat_max_silence_secs` (mig 0132).
    // I ping del brain resettano il timer implicitamente: ogni chunk ricevuto
    // riavvia il wait_for.
    let max_silence = Duration::from_secs(sse_max_silence_secs);
    'sse_loop: loop {
        let chunk_opt = tokio::time::timeout(max_silence, stream.next()).await;
        let bytes = match chunk_opt {
            Err(_elapsed) => {
                tracing::warn!(
                    "brain SSE silenzioso per {}s: run interrotto (brain bloccato?)",
                    sse_max_silence_secs
                );
                last_error = Some(format!(
                    "brain SSE silenzioso per {}s senza eventi",
                    sse_max_silence_secs
                ));
                break;
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                tracing::error!("brain SSE stream chunk error: {e}");
                last_error = Some(format!("SSE chunk: {e}"));
                break;
            }
            Ok(Some(Ok(b))) => b,
        };
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        // Ogni evento SSE termina con `\n\n`.
        while let Some(pos) = buffer.find("\n\n") {
            let raw_event: String = buffer.drain(..pos + 2).collect();
            let payload = parse_sse_data(&raw_event);
            let Some(payload) = payload else { continue };
            let evt: Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("brain SSE JSON invalido: {e} payload={}", payload);
                    continue;
                }
            };
            let kind = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "assistant_delta" => {
                    let text = evt
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        // ── Detection errori provider che arrivano come content
                        // Il brain Python a volte converte gli errori del provider
                        // SDK in stringhe `[Error: ...]` propagate come assistant
                        // content invece di essere lanciate come exception. Senza
                        // questo check il cooldown non scatta e il LED resta verde
                        // anche se il provider e' fuori uso.
                        // (vedi /api/gateway/providers che riporta healthy:true
                        // ma le richieste falliscono con quota_exceeded).
                        let trimmed = text.trim_start();
                        if trimmed.starts_with("[Error:") || trimmed.starts_with("[error:") {
                            if let Some((err_class, kind, human_reason)) =
                                classify_provider_error(None, &text)
                            {
                                let provider_key = provider.split('/').next().unwrap_or(&provider);
                                match kind {
                                    CooldownKind::Long => {
                                        crate::provider_cooldown::put_provider_in_long_cooldown(
                                            provider_key,
                                            human_reason,
                                        );
                                        tracing::warn!(
                                            "Provider '{}' COOLDOWN LUNGO ({}): {}",
                                            provider,
                                            err_class,
                                            human_reason
                                        );
                                    }
                                    CooldownKind::Short => {
                                        // Durata DB-driven (regola G): slow_cooldown_s, non 60 letterale.
                                        let secs =
                                            crate::provider_cooldown::provider_health_timings()
                                                .slow_cooldown_s;
                                        crate::provider_cooldown::put_provider_in_short_cooldown(
                                            provider_key,
                                            human_reason,
                                            secs,
                                        );
                                        tracing::warn!(
                                            "Provider '{}' COOLDOWN BREVE {}s ({}): {}",
                                            provider,
                                            secs,
                                            err_class,
                                            human_reason
                                        );
                                    }
                                }
                                last_error_class = Some(err_class.to_string());
                                last_error = Some(text.clone());
                            }
                        }
                        final_answer.push_str(&text);
                        last_text_segment.push_str(&text);
                        let _ = step_tx.send(AgentStepEvent {
                            run_id: run_id_str.clone(),
                            step: None,
                            trace: None,
                            is_final: false,
                            token_delta: Some(text),
                            thinking_delta: None,
                            meta_step: None,
                        });
                    }
                }
                "tool_use" => {
                    last_text_segment.clear();
                    iteration = iteration.saturating_add(1);
                    let tool_name = evt
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = evt.get("input").cloned().unwrap_or(json!({}));
                    let step = AgentStep {
                        run_id: run_id_str.clone(),
                        step_index: iteration,
                        tool_name,
                        tool_input: input,
                        tool_result: None,
                        status: AgentStepStatus::Running,
                        created_at: chrono::Utc::now().to_rfc3339(),
                    };
                    let _ = step_tx.send(AgentStepEvent {
                        run_id: run_id_str.clone(),
                        step: Some(step.clone()),
                        trace: None,
                        is_final: false,
                        token_delta: None,
                        thinking_delta: None,
                        meta_step: None,
                    });
                    steps.push(step);
                }
                "thinking_delta" => {
                    let text = evt
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        let _ = step_tx.send(AgentStepEvent {
                            run_id: run_id_str.clone(),
                            step: None,
                            trace: None,
                            is_final: false,
                            token_delta: None,
                            thinking_delta: Some(text),
                            meta_step: None,
                        });
                    }
                }
                "tool_result" => {
                    let tool_use_id = evt
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let content = evt
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let is_error = evt
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // Aggiorna l'ultimo step in Running (match by step_index
                    // non disponibile qui — aggiorniamo l'ultimo).
                    if let Some(last) = steps
                        .iter_mut()
                        .rev()
                        .find(|s| s.status == AgentStepStatus::Running)
                    {
                        last.tool_result = Some(content.clone());
                        last.status = if is_error {
                            AgentStepStatus::Failed
                        } else {
                            AgentStepStatus::Completed
                        };
                        let _ = step_tx.send(AgentStepEvent {
                            run_id: run_id_str.clone(),
                            step: Some(last.clone()),
                            trace: None,
                            is_final: false,
                            token_delta: None,
                            thinking_delta: None,
                            meta_step: None,
                        });
                    } else {
                        tracing::warn!(
                            "tool_result senza step Running (tool_use_id={})",
                            tool_use_id
                        );
                    }
                }
                "end_turn" => {
                    ended = true;
                    // Marca la fine della fase generativa (mig 0388): da qui il
                    // frontend e' libero (riceve end_turn) e reflection/learner
                    // girano in post-processing. Il guard anti-run-concorrente
                    // (handlers.rs) esclude i run con generation_ended_at valorizzato,
                    // evitando il 409 "la chat sembra libera". Best-effort: un errore
                    // qui non deve interrompere lo stream. IS NULL: marca una volta sola.
                    let _ = sqlx::query(
                        "UPDATE agent_runs SET generation_ended_at = NOW() \
                         WHERE id = $1 AND generation_ended_at IS NULL",
                    )
                    .bind(run_id)
                    .execute(&db)
                    .await;
                    acc_prompt_tokens = evt
                        .get("prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    acc_completion_tokens = evt
                        .get("completion_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    acc_total_tokens = evt
                        .get("total_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    acc_total_cost = evt
                        .get("total_cost")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    // Token prompt dell'ultima iterazione (per context ratio UI)
                    last_prompt_tokens = evt
                        .get("last_prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32);
                    // B5: legge metadata routing propagati dal brain Python
                    if let Some(tt) = evt.get("nexus_task_type").and_then(|v| v.as_str()) {
                        nexus_task_type = Some(tt.to_string());
                    }
                    if evt
                        .get("forced_close_unverified")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        forced_close_unverified = true;
                    }
                    if evt
                        .get("final_gate_passed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        final_gate_passed = true;
                    }
                    if let Some(at) = evt.get("nexus_agent_type").and_then(|v| v.as_str()) {
                        nexus_agent_type = Some(at.to_string());
                    }
                    // WAVE 3.2: esito dichiarato {outcome, summary, ...}.
                    if let Some(d) = evt.get("declared_outcome").and_then(|v| v.as_object()) {
                        if let Some(o) = d.get("outcome").and_then(|v| v.as_str()) {
                            declared_outcome = Some(o.to_string());
                        }
                        if let Some(s) = d.get("summary").and_then(|v| v.as_str()) {
                            if !s.trim().is_empty() {
                                declared_summary = Some(s.to_string());
                            }
                        }
                    }
                    // Provider/model effettivi dopo cascade fallback sticky:
                    // sovrascrivono i valori iniziali nel risultato finale.
                    if let Some(pu) = evt.get("provider_used").and_then(|v| v.as_str()) {
                        if !pu.trim().is_empty() {
                            effective_provider = pu.to_string();
                        }
                    }
                    if let Some(mu) = evt.get("model_used").and_then(|v| v.as_str()) {
                        if !mu.trim().is_empty() {
                            effective_model = mu.to_string();
                        }
                    }
                    if last_stop_reason.is_none() {
                        last_stop_reason = Some(
                            evt.get("stop_reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("end_turn")
                                .to_string(),
                        );
                    }
                }
                "error" => {
                    let msg = evt
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("errore brain")
                        .to_string();
                    // Lettura del campo strutturato `error_class` propagato dal brain
                    // (vedi brain/providers/error_handler.py::format_error_result).
                    // Ha priorita' sul pattern matching testuale: i messaggi possono
                    // essere localizzati o riformulati ma error_class resta stabile.
                    let err_class_raw = evt.get("error_class").and_then(|v| v.as_str());
                    tracing::error!("brain SSE error (error_class={:?}): {msg}", err_class_raw);

                    if let Some((err_class, kind, human_reason)) =
                        classify_provider_error(err_class_raw, &msg)
                    {
                        let provider_key = provider.split('/').next().unwrap_or(&provider);
                        match kind {
                            CooldownKind::Long => {
                                crate::provider_cooldown::put_provider_in_long_cooldown(
                                    provider_key,
                                    human_reason,
                                );
                                tracing::warn!(
                                    "Provider '{}' COOLDOWN LUNGO 6h ({}): {}. Routing successivo selezionera' un altro provider.",
                                    provider, err_class, human_reason
                                );
                            }
                            CooldownKind::Short => {
                                crate::provider_cooldown::put_provider_in_short_cooldown(
                                    provider_key,
                                    human_reason,
                                    60,
                                );
                                tracing::warn!(
                                    "Provider '{}' COOLDOWN BREVE 60s ({}): {}",
                                    provider,
                                    err_class,
                                    human_reason
                                );
                            }
                        }
                        last_error_class = Some(err_class.to_string());
                    } else if let Some(c) = err_class_raw {
                        // error_class non riconosciuto: lo propaghiamo comunque per audit.
                        last_error_class = Some(c.to_string());
                    }

                    last_stop_reason = Some("error".to_string());
                    last_error = Some(msg);
                }
                "ping" => {
                    // Heartbeat del brain: il run e' ancora attivo.
                    // Il timer MAX_SILENCE viene resettato implicitamente
                    // dalla ricezione di questo chunk (tokio::time::timeout
                    // si riavvia ad ogni Ok). Nessuna azione necessaria.
                    tracing::debug!("brain SSE ping: run attivo");
                }
                "usage" => {
                    // Token cumulativi live emessi dal brain a ogni iterazione
                    // executor (vedi brain/grpc_server/routes/agent.py). Vengono
                    // ritrasmessi al frontend come meta_step kind="usage_snapshot"
                    // che chat_agent.rs mappa all'evento SSE `agent_usage`, cosi'
                    // la barra context si aggiorna in tempo reale senza polling.
                    // Riusa il campo meta_step esistente per non toccare i 12 call
                    // site di AgentStepEvent (regola: niente patch speculative).
                    let prompt_t = evt
                        .get("prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let completion_t = evt
                        .get("completion_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let total_t = evt
                        .get("total_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let cost_t = evt
                        .get("total_cost")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let last_pt = evt
                        .get("last_prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    // Mantieni gli accumulatori coerenti col risultato finale anche
                    // se end_turn non dovesse arrivare (es. stream troncato).
                    if total_t > 0 {
                        acc_prompt_tokens = prompt_t;
                        acc_completion_tokens = completion_t;
                        acc_total_tokens = total_t;
                        acc_total_cost = cost_t;
                    }
                    if last_pt > 0 {
                        last_prompt_tokens = Some(last_pt);
                    }
                    let _ = step_tx.send(AgentStepEvent {
                        run_id: run_id_str.clone(),
                        step: None,
                        trace: None,
                        is_final: false,
                        token_delta: None,
                        thinking_delta: None,
                        meta_step: Some(AgentMetaStep {
                            kind: "usage_snapshot".to_string(),
                            title: String::new(),
                            payload: json!({
                                "totalTokens": total_t,
                                "promptTokens": prompt_t,
                                "completionTokens": completion_t,
                                "lastPromptTokens": last_pt,
                                "totalCostUsd": cost_t,
                            }),
                            correlation_id: None,
                            created_at: chrono::Utc::now().to_rfc3339(),
                        }),
                    });
                }
                "meta_step" => {
                    // Step semantico (plan/routing/clarify/fallback/reflection)
                    // ritrasmesso al frontend come AgentMetaStep. La persistenza
                    // su nexus_agent_meta_steps avviene lato brain Python che
                    // ha gia' accesso al DB tramite psycopg2 (vedi event_bus.py).
                    let kind = evt
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if kind.is_empty() {
                        tracing::warn!("meta_step senza kind, scarto: {payload}");
                        continue;
                    }
                    let title = evt
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let payload_val = evt.get("payload").cloned().unwrap_or(json!({}));
                    let correlation_id = evt
                        .get("correlation_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let created_at = evt
                        .get("created_at")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
                    let _ = step_tx.send(AgentStepEvent {
                        run_id: run_id_str.clone(),
                        step: None,
                        trace: None,
                        is_final: false,
                        token_delta: None,
                        thinking_delta: None,
                        meta_step: Some(AgentMetaStep {
                            kind,
                            title,
                            payload: payload_val,
                            correlation_id,
                            created_at,
                        }),
                    });
                }
                "done" => {
                    // Segnale terminale esplicito dal brain: il graph LangGraph
                    // ha concluso (END node raggiunto). Usciamo subito dal loop
                    // esterno (label 'sse_loop) senza attendere il timeout di
                    // silenzio SSE — questo elimina l'attesa di 120s che
                    // l'utente percepiva come "Agente in esecuzione" anche
                    // dopo aver ricevuto la risposta finale.
                    tracing::info!("brain SSE done ricevuto: chiusura stream {run_id_str}");
                    break 'sse_loop;
                }
                _ => {
                    // Eventi sconosciuti: ignoriamo.
                }
            }
        }
    }

    let status = if last_error.is_some() {
        // Distingui la causa di terminazione con errore
        match last_stop_reason.as_deref() {
            Some("no_capable_provider") | Some("provider_unavailable") => {
                AgentRunStatus::ProviderUnavailable
            }
            _ => AgentRunStatus::Failed,
        }
    } else if forced_close_unverified
        || matches!(
            last_stop_reason.as_deref(),
            Some("loop_detected") | Some("loop_aborted") | Some("loop_abort")
        )
    {
        // Esito canonico (macchina a stati, mig 0386): un abort anti-loop NON e' un
        // successo ne' un errore infrastrutturale, ma un FALLIMENTO DIAGNOSTICATO.
        // Il recap M44 dell'executor porta sempre esito + file toccati + prossimo
        // passo. `forced_close_unverified` e' il segnale autoritativo: copre anche
        // il caso in cui il final_gate ha riscritto stop_reason a "end_turn" sul
        // ramo forced_close (prima un abort finiva erroneamente come Completed).
        AgentRunStatus::FailedDiagnosed
    } else if matches!(
        declared_outcome.as_deref(),
        Some("blocked") | Some("needs_input")
    ) {
        // WAVE 3.2: il modello ha DICHIARATO via task_complete di essere bloccato
        // (causa esterna mancante) o di aver bisogno di input. Esito canonico
        // BlockedNeedsInput, indipendente dalla lingua del testo. Sostituisce
        // l'inferenza lessicale resigned_patterns per questo caso.
        AgentRunStatus::BlockedNeedsInput
    } else if ended && final_gate_passed {
        // Verifica E2E (final_gate) superata: successo verificato (mig 0386).
        AgentRunStatus::CompletedVerified
    } else if ended {
        AgentRunStatus::Completed
    } else {
        AgentRunStatus::Completed
    };

    // ── Hollow completion detection ─────────────────────────────────────────
    // Cattura due varianti del "completamento allucinato":
    //   (A) had_tools && steps.is_empty() && iteration <= 1: l'agente aveva
    //       tool disponibili e ha chiuso senza usarne nessuno al primo turno
    //       (tipico di modelli piccoli che non sanno come usare le tool).
    //   (B) final_answer vuoto/whitespace-only: l'agente ha dichiarato Completed
    //       ma il body della risposta e' assente. Travestito da successo,
    //       tipico ad esempio di deepseek-coder/deepseek-reasoner quando il
    //       provider risponde solo con la status line "Operazione completata"
    //       senza il content reale (visto in prod il 2026-05-20).
    // In entrambi i casi il caller (chat_messages.rs) deve poter ri-tentare
    // con un altro provider (hollow_retry path).
    let had_tools = tools_json
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let final_answer_empty = final_answer.trim().is_empty();
    let hollow_no_tools =
        status == AgentRunStatus::Completed && had_tools && steps.is_empty() && iteration <= 1;
    let hollow_empty_answer = status == AgentRunStatus::Completed && final_answer_empty;

    // ── Detection "RESIGNED" ─────────────────────────────────────────────
    // Il modello completa con content NON vuoto MA il content e' una
    // rinuncia ("non posso", "non riesco", "mi dispiace") E non ha chiamato
    // nessun tool. Significa che ha capacita' insufficiente per il task.
    // Visto in prod 2026-05-20: gemini-2.5-flash su task fix complesso
    // ha emesso 422 token con "Non e' possibile eseguire le operazioni
    // richieste..." senza usare alcun tool.
    //
    // Pattern indicativi di rinuncia (italiano + inglese). False positives
    // mitigati richiedendo:
    //   - had_tools=true (intent richiede tool, quindi 0 tool call e' sintomo)
    //   - steps.is_empty() (nessun tool effettivamente invocato)
    //   - final_answer NON vuoto ma matcha pattern
    let resigned_patterns = [
        "non riesco a",
        "non posso eseguire",
        "non posso accedere",
        "non posso interagire",
        "non e' possibile eseguire",
        "non è possibile eseguire",
        "mi dispiace, ma non posso",
        "mi dispiace ma non posso",
        "i'm unable to",
        "i cannot access",
        "i cannot execute",
        "i can't interact",
        "i'm sorry, i can't",
        "sembra che il server",
        "il server non e' disponibile",
        "il server non è disponibile",
    ];
    let final_lc = final_answer.to_lowercase();
    let matches_resigned = resigned_patterns.iter().any(|p| final_lc.contains(p));
    // RESIGNED rilevato quando:
    //  - status=Completed
    //  - had_tools=true (intent richiede tool)
    //  - final_answer non vuoto MA matcha pattern di rinuncia
    //  - iteration <= 2 (rinuncia presto, dopo max 1-2 step)
    // Non richiediamo steps.is_empty(): il modello potrebbe aver provato
    // 1 tool che ha fallito, e poi aver "rinunciato" emettendo un messaggio
    // tipo "Non riesco a..." invece di provare strategie alternative.
    let hollow_resigned = status == AgentRunStatus::Completed
        && had_tools
        && !final_answer_empty
        && matches_resigned
        && iteration <= 2;

    let hollow_completion = hollow_no_tools || hollow_empty_answer || hollow_resigned;
    let hollow_kind: &str = if !hollow_completion {
        ""
    } else if hollow_resigned {
        "RESIGNED"
    } else if hollow_empty_answer && !hollow_no_tools {
        "EMPTY_ANSWER"
    } else if hollow_empty_answer {
        "EMPTY_ANSWER+NO_TOOLS"
    } else {
        "NO_TOOLS"
    };
    if hollow_completion {
        let kind = hollow_kind;
        tracing::warn!(
            "agent_run {}: HOLLOW COMPLETION [{}] — modello {}/{} ha dichiarato \
             di aver completato (steps={}, iteration={}, final_answer_chars={}). \
             Risposta: {:?}",
            run_id_str,
            kind,
            provider,
            model,
            steps.len(),
            iteration,
            final_answer.len(),
            final_answer.chars().take(180).collect::<String>(),
        );
        // NB: la persistenza diagnostica in `nexus_provider_empty_responses`
        // avviene in `chat_messages/agent_run.rs` (ha accesso allo `state.db`).
        // Il kind e' propagato via `AgentRunResult.hollow_completion_kind`.
    }

    // Evento finale sul broadcast. Emesso solo se il caller lo richiede:
    // i caller con retry loop chiamano questa funzione piu' volte e devono
    // emettere is_final una sola volta a fine retry (vedi doc del parametro).
    if emit_final_event {
        let _ = step_tx.send(AgentStepEvent {
            run_id: run_id_str.clone(),
            step: None,
            trace: None,
            is_final: true,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        });
    }

    AgentRunResult {
        run_id: run_id_str,
        status,
        steps,
        pending_actions: Vec::new(),
        final_answer: if final_answer.is_empty() {
            // WAVE 3.2: il modello ha chiuso con task_complete SENZA testo: il
            // summary dichiarato e' la risposta (evita hollow/placeholder per i
            // run che dichiarano l'esito ma non scrivono un body).
            if let Some(summary) = declared_summary.clone() {
                Some(summary)
            } else {
            last_error.as_ref().map(|e| {
                // Fix M23: distingue le due cause piu comuni di interruzione SSE
                // dal brain Python (chunk decode error, silenzio prolungato) dal
                // generico errore di elaborazione, dando indicazioni utili.
                // Lo stato del progetto e gli step gia eseguiti sono persistiti
                // su disco/DB: l'utente puo riprendere senza perdere il lavoro.
                let lower = e.to_ascii_lowercase();
                if lower.starts_with("sse chunk:")
                    || lower.contains("error decoding response body")
                    || lower.contains("connection reset")
                    || lower.contains("connection closed")
                {
                    format!(
                        "Il flusso dal brain si e' interrotto prima del completamento. \
                         Gli step gia eseguiti e i file generati sono salvati. \
                         Premi Invio (o scrivi \"continua\") per riprendere da dove eri.\n\n\
                         *Dettaglio tecnico: {}*",
                        sanitize_error_for_user(e)
                    )
                } else if lower.contains("silenzioso") || lower.contains("timeout") {
                    format!(
                        "Il brain non ha risposto entro il timeout configurato. \
                         Verifica che sia attivo (`curl http://localhost:8001/health`) \
                         e premi Invio per riprendere.\n\n\
                         *Dettaglio tecnico: {}*",
                        sanitize_error_for_user(e)
                    )
                } else {
                    format!(
                        "Si e' verificato un errore durante l'elaborazione della richiesta. \
                         Riprova tra qualche secondo oppure cambia modello.\n\n\
                         *Dettaglio tecnico: {}*",
                        sanitize_error_for_user(e)
                    )
                }
            })
            }
        } else {
            // Se ci sono stati tool_use, salva solo l'ultimo segmento di testo
            // (la risposta finale dell'agente, non il ragionamento intermedio).
            // Se non ci sono stati tool_use, last_text_segment == final_answer.
            let answer = if last_text_segment.trim().is_empty() {
                final_answer
            } else {
                last_text_segment
            };
            Some(answer)
        },
        provider: effective_provider,
        model: effective_model,
        iteration_count: iteration,
        nexus_override_applied: nexus_agent_type.is_some(),
        nexus_agent_type,
        nexus_q_value: None,
        nexus_task_type,
        provider_privacy_notice: None,
        prompt_tokens: acc_prompt_tokens,
        completion_tokens: acc_completion_tokens,
        total_tokens: acc_total_tokens,
        total_cost: acc_total_cost,
        last_prompt_tokens,
        error_class: last_error_class,
        stop_reason: last_stop_reason,
        hollow_completion,
        hollow_no_tools,
        hollow_completion_kind: hollow_kind.to_string(),
    }
}

/// Rende un messaggio di errore API leggibile per l'utente finale.
/// Rimuove i dettagli interni (indici messaggi, nomi interni di blocchi)
/// e restituisce una descrizione sintetica.
fn sanitize_error_for_user(raw: &str) -> String {
    // Errore tool_use/tool_result mismatch (Anthropic)
    if raw.contains("tool_use ids were found without tool_result") {
        return "conversazione interrotta per un problema di sincronizzazione tra i passaggi interni dell'agente".to_string();
    }
    // Errore di contesto troppo lungo
    if raw.contains("context length")
        || raw.contains("too many tokens")
        || raw.contains("maximum context")
    {
        return "la conversazione ha superato la lunghezza massima consentita dal modello"
            .to_string();
    }
    // Errore di rate limit
    if raw.contains("rate_limit") || raw.contains("429") || raw.contains("Too Many Requests") {
        return "troppe richieste al provider AI, attendere qualche secondo".to_string();
    }
    // Errore di autenticazione
    if raw.contains("401") || raw.contains("authentication") || raw.contains("invalid_api_key") {
        return "errore di autenticazione con il provider AI".to_string();
    }
    // Errore 5xx del provider (503, 502, 500, 504, ecc.)
    if raw.contains("Internal server error")
        || raw.contains("internal server error")
        || raw.contains("service unavailable")
        || raw.contains("Service Unavailable")
        || raw.contains("bad gateway")
        || raw.contains("Bad Gateway")
        || raw.contains("Gateway Timeout")
        || raw.contains("gateway timeout")
        || raw.contains("503")
        || raw.contains("502")
        || raw.contains("504")
    {
        return "il provider AI e' temporaneamente non disponibile (errore server). Il sistema sta provando con un altro provider.".to_string();
    }
    // Errore generico: tronca a 200 caratteri e rimuovi stack trace
    let clean = raw.lines().next().unwrap_or(raw);
    if clean.len() > 200 {
        format!("{}...", &clean[..200])
    } else {
        clean.to_string()
    }
}

/// Estrae il payload dopo `data: ` da un blocco evento SSE (senza `\n\n`).
fn parse_sse_data(block: &str) -> Option<String> {
    let mut out = String::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(rest.trim_start());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Invia una decisione di approvazione/annullamento al brain per un run in
/// `awaiting_confirmation`. Chiama `POST /agent/approve/{thread_id}` sul
/// brain HTTP server (cfr. `brain/grpc_server/main.py`).
///
/// Se `approved = true`, il brain riprende il loop e, se presente, inietta
/// `resume_message` nello state. Se `approved = false`, il run viene marcato
/// come cancellato lato brain.
pub async fn resume_run(
    thread_id: Uuid,
    approved: bool,
    resume_message: Option<String>,
) -> Result<(), String> {
    let url = format!(
        "{}/agent/approve/{}",
        brain_rest_url().trim_end_matches('/'),
        thread_id
    );
    let body = json!({
        "approved": approved,
        "resume_message": resume_message,
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest build: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("brain approve status {status}: {text}"));
    }
    Ok(())
}

fn fail_result(run_id: &str, provider: &str, model: &str, msg: String) -> AgentRunResult {
    AgentRunResult {
        run_id: run_id.to_string(),
        status: AgentRunStatus::Failed,
        steps: Vec::new(),
        pending_actions: Vec::new(),
        final_answer: Some(format!("[brain error] {msg}")),
        provider: provider.to_string(),
        model: model.to_string(),
        iteration_count: 0,
        nexus_override_applied: false,
        nexus_agent_type: None,
        nexus_q_value: None,
        nexus_task_type: None,
        provider_privacy_notice: None,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        total_cost: 0.0,
        last_prompt_tokens: None,
        error_class: None,
        stop_reason: Some("error".to_string()),
        hollow_completion: false,
        hollow_no_tools: false,
        hollow_completion_kind: String::new(),
    }
}

// =====================================================================
// TEST L3: Tool gating per automation_mode
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::AutomationMode;

    /// Whitelist di test (replica della seed DB in mig 0132).
    /// In produzione la whitelist autoritativa viene letta da
    /// `settings.automation.study_mode_readonly_tools`.
    fn test_whitelist() -> Vec<String> {
        vec![
            "read_file",
            "read_file_lines",
            "list_files",
            "search_in_files",
            "search_codebase_semantic",
            "get_project_structure",
            "get_file_diff",
            "git_status",
            "git_log",
            "git_diff",
            "list_services",
            "read_service_output",
            "nexus_mcp_tool_search",
            "list_profiles",
            "get_profile",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    fn make_test_tools() -> Value {
        json!([
            { "name": "read_file", "description": "..." },
            { "name": "list_files", "description": "..." },
            { "name": "search_in_files", "description": "..." },
            { "name": "write_file", "description": "..." },
            { "name": "edit_file", "description": "..." },
            { "name": "run_command", "description": "..." },
            { "name": "git_status", "description": "..." },
            { "name": "git_commit", "description": "..." },
            { "name": "delete_file", "description": "..." },
            { "name": "nexus_mcp_tool_search", "description": "..." },
        ])
    }

    #[test]
    fn automatic_mode_non_filtra_nessun_tool() {
        // Modalita' automatic: l'utente vuole massima autonomia, tutti i
        // tool restano esposti (write_file, edit_file, run_command, ecc.).
        let tools = make_test_tools();
        let original_len = tools.as_array().unwrap().len();
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Automatic, &wl);
        assert_eq!(filtered.as_array().unwrap().len(), original_len);
    }

    #[test]
    fn confirm_mode_non_filtra_lato_rust() {
        // Confirm: il gating e' a livello HITL nel brain Python (interrupt prima
        // di edit/write). Lato Rust non filtriamo, esponiamo tutto.
        let tools = make_test_tools();
        let original_len = tools.as_array().unwrap().len();
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Confirm, &wl);
        assert_eq!(filtered.as_array().unwrap().len(), original_len);
    }

    #[test]
    fn study_mode_filtra_a_solo_readonly() {
        // Study: l'agente DEVE solo analizzare. Filtraggio difensivo:
        // anche se il modello ignora le istruzioni del prompt, NON puo'
        // chiamare un tool che non gli e' stato esposto.
        let tools = make_test_tools();
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Study, &wl);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        // Devono restare i tool read-only
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"search_in_files"));
        assert!(names.contains(&"git_status"));
        assert!(names.contains(&"nexus_mcp_tool_search"));
        // NON devono esserci tool che scrivono / eseguono / eliminano
        assert!(
            !names.contains(&"write_file"),
            "write_file vietato in study"
        );
        assert!(!names.contains(&"edit_file"), "edit_file vietato in study");
        assert!(
            !names.contains(&"run_command"),
            "run_command vietato in study"
        );
        assert!(
            !names.contains(&"git_commit"),
            "git_commit vietato in study"
        );
        assert!(
            !names.contains(&"delete_file"),
            "delete_file vietato in study"
        );
    }

    #[test]
    fn study_mode_su_array_vuoto_ritorna_array_vuoto() {
        let empty = json!([]);
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(empty, &AutomationMode::Study, &wl);
        assert!(filtered.as_array().unwrap().is_empty());
    }

    #[test]
    fn study_mode_su_non_array_ritorna_array_vuoto() {
        // Robustezza: se il JSON in input non e' un array (regressione/bug),
        // ritorniamo array vuoto invece di crashare.
        let bad = json!({"not": "an array"});
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(bad, &AutomationMode::Study, &wl);
        assert!(filtered.as_array().unwrap().is_empty());
    }

    #[test]
    fn study_mode_whitelist_db_driven_personalizzabile() {
        // Caso paradigmatico DB-driven: l'admin puo' restringere o ampliare
        // la whitelist via UPDATE settings senza redeploy. Test simula una
        // whitelist piu' ristretta.
        let tools = make_test_tools();
        let restricted_wl = vec!["read_file".to_string()];
        let filtered =
            filter_tools_by_automation_mode(tools, &AutomationMode::Study, &restricted_wl);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        // Solo read_file passa (whitelist personalizzata)
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn study_mode_whitelist_vuota_blocca_tutti_i_tool() {
        // Edge case: se l'admin svuota la whitelist nel DB, l'agente in
        // study mode non puo' chiamare nessun tool (massima sicurezza).
        let tools = make_test_tools();
        let empty_wl: Vec<String> = Vec::new();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Study, &empty_wl);
        assert!(filtered.as_array().unwrap().is_empty());
    }

    #[test]
    fn study_mode_filtra_tool_mcp_non_in_whitelist() {
        // Tool MCP esterni (non in whitelist) vengono filtrati anche se
        // sembrano innocui. Whitelist conservativa per design.
        let tools = json!([
            { "name": "read_file" },
            { "name": "external_mcp_tool_that_writes" },
            { "name": "send_email" },
        ]);
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Study, &wl);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn fallback_whitelist_contiene_tool_minimi_sicuri() {
        // Il fallback hardcoded (STUDY_MODE_READONLY_TOOLS_FALLBACK) e' usato
        // solo se il DB e' down. Deve contenere ALMENO i tool di base per
        // permettere all'agente di leggere file e cercare nel codebase.
        let must_have = ["read_file", "list_files", "search_in_files", "git_status"];
        for tool in &must_have {
            assert!(
                STUDY_MODE_READONLY_TOOLS_FALLBACK.contains(tool),
                "tool minimo '{}' mancante dalla fallback whitelist",
                tool
            );
        }
        // E NON deve contenere tool che scrivono
        let must_not_have = ["write_file", "edit_file", "run_command", "delete_file"];
        for tool in &must_not_have {
            assert!(
                !STUDY_MODE_READONLY_TOOLS_FALLBACK.contains(tool),
                "tool pericoloso '{}' presente nel fallback (deve essere escluso)",
                tool
            );
        }
    }

    #[test]
    fn is_o_series_rileva_modelli_reasoning() {
        assert!(is_o_series_model("o3"));
        assert!(is_o_series_model("o3-mini"));
        assert!(is_o_series_model("O3"));
        assert!(is_o_series_model("o1"));
        assert!(is_o_series_model("o1-preview"));
        assert!(is_o_series_model("o4-mini"));
        assert!(is_o_series_model("o4-mini-2025-04-16"));
        // NON o-series
        assert!(!is_o_series_model("gpt-4o"));
        assert!(!is_o_series_model("gpt-4o-mini"));
        assert!(!is_o_series_model("claude-sonnet-4-6"));
        assert!(!is_o_series_model("gemini-2.5-flash"));
    }

    #[test]
    fn o_series_essential_contiene_discovery_e_write() {
        // I tool essenziali per o-series devono includere sia tool di
        // scrittura (per operare) sia tool di discovery (per scoprire
        // tool aggiuntivi a runtime).
        let must_have = [
            "write_file",
            "edit_file",
            "run_command", // scrittura
            "nexus_mcp_tool_search",
            "nexus_mcp_tool_call", // discovery
            "read_file",
            "list_files", // lettura
        ];
        for tool in &must_have {
            assert!(
                O_SERIES_ESSENTIAL_TOOLS_FALLBACK.contains(tool),
                "tool essenziale '{}' mancante dal fallback o-series",
                tool
            );
        }
    }

    #[test]
    fn filter_tools_by_whitelist_filtra_correttamente() {
        let tools = make_test_tools();
        let whitelist = vec![
            "read_file".to_string(),
            "write_file".to_string(),
            "nexus_mcp_tool_search".to_string(),
        ];
        let filtered = filter_tools_by_whitelist(tools, &whitelist);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"nexus_mcp_tool_search"));
        // Non devono esserci tool non in whitelist
        assert!(!names.contains(&"edit_file"));
        assert!(!names.contains(&"run_command"));
    }

    // ── Classificazione errori provider / cooldown ────────────────────────────

    #[test]
    fn quota_429_e_billing_cooldown_lungo() {
        // Caso live: OpenAI risponde HTTP 429 "You exceeded your current quota".
        // E' quota/credito esaurito, NON un rate-limit transitorio: deve dare
        // cooldown lungo come credit_balance_too_low.
        let msg = "Error code: 429 - {'error': {'message': 'You exceeded your current quota, please check your plan and billing details.', 'type': 'insufficient_quota'}}";
        let (class, kind, _) =
            classify_provider_error(None, msg).expect("429 quota deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }

    #[test]
    fn rate_limit_429_senza_marker_quota_e_cooldown_breve() {
        // 429 generico (finestra di richieste) senza marker di quota/credito:
        // rate-limit transitorio, cooldown breve.
        let msg = "Error code: 429 - Rate limit reached for requests. Please try again later.";
        let (class, kind, _) =
            classify_provider_error(None, msg).expect("429 rate limit deve essere classificato");
        assert_eq!(class, "rate_limit");
        assert_eq!(kind, CooldownKind::Short);
    }

    #[test]
    fn error_class_rate_limit_ma_messaggio_quota_promosso_a_billing() {
        // Difesa in profondita': anche se il brain (o un provider) ha mappato
        // il 429 a error_class=rate_limit, se il messaggio contiene marker di
        // quota la classe va promossa a billing/cooldown-lungo. Senza questo,
        // l'error_class esplicito short-circuitava il pattern matching e il
        // provider con quota esaurita restava nel cascade (bug live).
        let msg = "You exceeded your current quota, please check your plan and billing details.";
        let (class, kind, _) =
            classify_provider_error(Some("rate_limit"), msg).expect("deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }

    #[test]
    fn error_class_rate_limit_senza_marker_resta_rate_limit() {
        // error_class=rate_limit + messaggio senza marker quota: resta
        // rate-limit transitorio (cooldown breve).
        let (class, kind, _) = classify_provider_error(Some("rate_limit"), "Too Many Requests")
            .expect("deve essere classificato");
        assert_eq!(class, "rate_limit");
        assert_eq!(kind, CooldownKind::Short);
    }

    #[test]
    fn insufficient_quota_error_class_e_billing() {
        // error_class esplicito insufficient_quota -> billing/cooldown-lungo.
        let (class, kind, _) = classify_provider_error(Some("insufficient_quota"), "")
            .expect("deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }

    #[test]
    fn billing_hard_limit_reached_e_billing() {
        // Marker OpenAI billing_hard_limit_reached -> cooldown lungo.
        let msg = "Error code: 429 - billing_hard_limit_reached";
        let (class, kind, _) =
            classify_provider_error(None, msg).expect("deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }
}
