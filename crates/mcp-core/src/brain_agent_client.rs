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
pub async fn build_tools_json_for_agent(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
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

    if mcp_tool_count < hard_limit && mcp_tool_count > 0 {
        // Catalogo piccolo: include le definizioni MCP direttamente
        tracing::debug!(
            "build_tools_json: {} tool MCP < soglia {}, includo definizioni dirette",
            mcp_tool_count, hard_limit
        );
        let mcp_tools = crate::mcp_connectors::load_mcp_tools_for_agent(db, user_id, Some(project_id)).await;
        if mcp_tools.is_empty() {
            return base_tools;
        }
        let mut all = base_tools.as_array().cloned().unwrap_or_default();
        all.extend(mcp_tools);
        json!(all)
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
    }
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

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
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

    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("brain SSE stream chunk error: {e}");
                last_error = Some(format!("SSE chunk: {e}"));
                break;
            }
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
        nexus_override_applied: false,
        nexus_agent_type: None,
        nexus_q_value: None,
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
        provider_privacy_notice: None,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        total_cost: 0.0,
        error_class: None,
        stop_reason: Some("error".to_string()),
    }
}
