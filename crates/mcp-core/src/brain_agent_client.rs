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

use crate::agent_types::{
    AgentRunResult, AgentRunStatus, AgentStep, AgentStepEvent, AgentStepStatus,
};
use crate::agent_tools::AGENT_TOOLS_JSON;

/// URL REST del brain (FastAPI). Default allineato al port di `--rest` del brain.
fn brain_rest_url() -> String {
    std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string())
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
        "SELECT value FROM settings WHERE key = 'automation.study_mode_readonly_tools'"
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

    let base_tools: Value =
        serde_json::from_str(AGENT_TOOLS_JSON).unwrap_or_else(|_| json!([]));

    let full_tools = if mcp_tool_count < hard_limit && mcp_tool_count > 0 {
        // Catalogo piccolo: include le definizioni MCP direttamente
        tracing::debug!(
            "build_tools_json: {} tool MCP < soglia {}, includo definizioni dirette",
            mcp_tool_count, hard_limit
        );
        let mcp_tools = crate::mcp_connectors::load_mcp_tools_for_agent(db, user_id, Some(project_id)).await;
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
                mcp_tool_count, hard_limit
            );
        }
        base_tools
    };

    // Gating finale per automation_mode: in `study` filtriamo a solo
    // read-only. `confirm` e `automatic` passano la lista intera.
    // La whitelist e' letta da `settings.automation.study_mode_readonly_tools`
    // (mig 0132) — niente lista hardcoded nel codice (regola G CLAUDE.md).
    let readonly_whitelist = load_study_mode_readonly_tools(db).await;
    filter_tools_by_automation_mode(full_tools, automation_mode, &readonly_whitelist)
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
    // Step 1: error_class esplicito propagato dal brain.
    match error_class {
        Some("billing_error") | Some("billing_required") | Some("quota_exceeded")
        | Some("credit_balance_too_low") | Some("insufficient_quota") => {
            return Some((
                "billing_error",
                CooldownKind::Long,
                "Quota AI esaurita o credito insufficiente",
            ));
        }
        Some("rate_limit") => {
            return Some((
                "rate_limit",
                CooldownKind::Short,
                "Rate limit raggiunto",
            ));
        }
        Some("overloaded") | Some("provider_error") | Some("server_error") => {
            return Some((
                "provider_error",
                CooldownKind::Short,
                "Provider sovraccarico o errore temporaneo",
            ));
        }
        _ => {}
    }

    // Step 2: pattern matching sul testo (it/en).
    let lower = msg.to_lowercase();
    if (lower.contains("credit balance") && lower.contains("too low"))
        || lower.contains("insufficient_quota")
        || lower.contains("exceeded your current quota")
        || lower.contains("plans & billing")
        || lower.contains("upgrade or purchase credits")
        || lower.contains("billing required")
        || lower.contains("payment required")
        || lower.contains("quota ai esaurita")
        || lower.contains("credito insufficiente")
    {
        return Some((
            "billing_error",
            CooldownKind::Long,
            "Quota AI esaurita o credito insufficiente",
        ));
    }
    if lower.contains("rate limit")
        || lower.contains("limite di richieste")
        || lower.contains("too many requests")
        || lower.contains("429")
    {
        return Some((
            "rate_limit",
            CooldownKind::Short,
            "Rate limit raggiunto",
        ));
    }
    if lower.contains("overloaded")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("502")
        || lower.contains("503")
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
) -> AgentRunResult {
    let run_id_str = run_id.to_string();
    let url = format!("{}/agent/run/stream", brain_rest_url().trim_end_matches('/'));

    let body = json!({
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
    });

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
        Err(e) => return fail_result(&run_id_str, &provider, &model, format!("reqwest build: {e}")),
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
    // B5: metadata routing propagati dal brain Python nell'evento end_turn
    let mut nexus_task_type: Option<String> = None;
    let mut nexus_agent_type: Option<String> = None;

    // Timeout per-silence: se il brain non emette alcun chunk SSE (inclusi i
    // ping heartbeat ogni ~30s) per `sse_max_silence_secs` secondi, il run
    // viene considerato bloccato e il loop esce. Valore letto dal caller via
    // `settings.routing.sse_heartbeat_max_silence_secs` (mig 0132).
    // I ping del brain resettano il timer implicitamente: ogni chunk ricevuto
    // riavvia il wait_for.
    let max_silence = Duration::from_secs(sse_max_silence_secs);
    loop {
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
                                        crate::provider_cooldown::put_provider_in_long_cooldown(provider_key, human_reason);
                                        tracing::warn!("Provider '{}' COOLDOWN LUNGO ({}): {}", provider, err_class, human_reason);
                                    }
                                    CooldownKind::Short => {
                                        crate::provider_cooldown::put_provider_in_short_cooldown(provider_key, human_reason, 60);
                                        tracing::warn!("Provider '{}' COOLDOWN BREVE 60s ({}): {}", provider, err_class, human_reason);
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
                    });
                    steps.push(step);
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
                    acc_prompt_tokens = evt.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    acc_completion_tokens = evt.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    acc_total_tokens = evt.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    acc_total_cost = evt.get("total_cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    // B5: legge metadata routing propagati dal brain Python
                    if let Some(tt) = evt.get("nexus_task_type").and_then(|v| v.as_str()) {
                        nexus_task_type = Some(tt.to_string());
                    }
                    if let Some(at) = evt.get("nexus_agent_type").and_then(|v| v.as_str()) {
                        nexus_agent_type = Some(at.to_string());
                    }
                    if last_stop_reason.is_none() {
                        last_stop_reason = Some(
                            evt.get("stop_reason").and_then(|v| v.as_str()).unwrap_or("end_turn").to_string()
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
                                crate::provider_cooldown::put_provider_in_long_cooldown(provider_key, human_reason);
                                tracing::warn!(
                                    "Provider '{}' COOLDOWN LUNGO 6h ({}): {}. Routing successivo selezionera' un altro provider.",
                                    provider, err_class, human_reason
                                );
                            }
                            CooldownKind::Short => {
                                crate::provider_cooldown::put_provider_in_short_cooldown(provider_key, human_reason, 60);
                                tracing::warn!(
                                    "Provider '{}' COOLDOWN BREVE 60s ({}): {}",
                                    provider, err_class, human_reason
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
    } else if ended {
        // Distingui fine normale da loop abortito
        match last_stop_reason.as_deref() {
            Some("loop_detected") | Some("loop_aborted") => AgentRunStatus::LoopAborted,
            _ => AgentRunStatus::Completed,
        }
    } else {
        AgentRunStatus::Completed
    };

    // Evento finale sul broadcast.
    let _ = step_tx.send(AgentStepEvent {
        run_id: run_id_str.clone(),
        step: None,
        trace: None,
        is_final: true,
        token_delta: None,
    });

    AgentRunResult {
        run_id: run_id_str,
        status,
        steps,
        pending_actions: Vec::new(),
        final_answer: if final_answer.is_empty() {
            last_error.as_ref().map(|e| {
                format!(
                    "Si e' verificato un errore durante l'elaborazione della richiesta. \
                     Riprova tra qualche secondo oppure cambia modello.\n\n\
                     *Dettaglio tecnico: {}*",
                    sanitize_error_for_user(e)
                )
            })
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
        provider,
        model,
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
        error_class: last_error_class,
        stop_reason: last_stop_reason,
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
    if raw.contains("context length") || raw.contains("too many tokens") || raw.contains("maximum context") {
        return "la conversazione ha superato la lunghezza massima consentita dal modello".to_string();
    }
    // Errore di rate limit
    if raw.contains("rate_limit") || raw.contains("429") || raw.contains("Too Many Requests") {
        return "troppe richieste al provider AI, attendere qualche secondo".to_string();
    }
    // Errore di autenticazione
    if raw.contains("401") || raw.contains("authentication") || raw.contains("invalid_api_key") {
        return "errore di autenticazione con il provider AI".to_string();
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

fn fail_result(
    run_id: &str,
    provider: &str,
    model: &str,
    msg: String,
) -> AgentRunResult {
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
        error_class: None,
        stop_reason: Some("error".to_string()),
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
            "read_file", "read_file_lines", "list_files", "search_in_files",
            "search_codebase_semantic", "get_project_structure", "get_file_diff",
            "git_status", "git_log", "git_diff",
            "list_services", "read_service_output",
            "nexus_mcp_tool_search", "list_profiles", "get_profile",
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
        assert!(!names.contains(&"write_file"), "write_file vietato in study");
        assert!(!names.contains(&"edit_file"), "edit_file vietato in study");
        assert!(!names.contains(&"run_command"), "run_command vietato in study");
        assert!(!names.contains(&"git_commit"), "git_commit vietato in study");
        assert!(!names.contains(&"delete_file"), "delete_file vietato in study");
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
        let filtered = filter_tools_by_automation_mode(
            tools,
            &AutomationMode::Study,
            &restricted_wl,
        );
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
        let filtered = filter_tools_by_automation_mode(
            tools,
            &AutomationMode::Study,
            &empty_wl,
        );
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
}
