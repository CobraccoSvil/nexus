use std::collections::HashMap;

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

use crate::{
    agent_types::AgentStepEvent,
    auth::Claims,
    chat_learning::{api_error, parse_user_id, ApiError, ApiResult},
    AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmAgentRunRequest {
    pub approved: bool,
}

/// SSE: stream degli AgentStepEvent per una sessione (o un run specifico).
/// Parametro opzionale nella query: ?run_id=<uuid>
pub async fn agent_stream(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Sse<futures::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>>, ApiError> {
    let _user_id = parse_user_id(&claims)?;
    let _session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    let run_id: Option<Uuid> = params
        .get("run_id")
        .and_then(|s| Uuid::parse_str(s).ok());

    // ── REPLAY dal DB ─────────────────────────────────────────────────────
    // Race condition fix: il client riceve la response POST /messages e POI
    // apre lo SSE. Se l'agente risponde velocemente (Mistral/Haiku ~500ms),
    // gli eventi nel broadcast vengono emessi PRIMA che il client si connetta
    // e sono persi (0 receiver). Il client vede stream vuoto.
    //
    // Fix: replay degli step gia' persistiti + final_answer come primo blob
    // di eventi, POI continua col live broadcast.
    let mut replay_events: Vec<Event> = Vec::new();
    if let Some(rid) = run_id {
        // Replay step dal DB
        if let Ok(rows) = sqlx::query(
            "SELECT step_index, tool_name, tool_input, tool_result, status, created_at
             FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
        )
        .bind(rid)
        .fetch_all(&state.db)
        .await
        {
            for r in rows {
                let step_index: i32 = r.try_get("step_index").unwrap_or(0);
                let tool_name: String = r.try_get("tool_name").unwrap_or_default();
                let tool_input: Value = r.try_get("tool_input").unwrap_or(json!({}));
                let tool_result: Option<String> = r.try_get("tool_result").unwrap_or(None);
                let status: String = r.try_get("status").unwrap_or_default();
                let data = json!({
                    "runId": rid.to_string(),
                    "isFinal": false,
                    "step": {
                        "stepIndex": step_index,
                        "toolName": tool_name,
                        "toolInput": tool_input,
                        "toolResult": tool_result,
                        "status": status,
                    },
                })
                .to_string();
                replay_events.push(Event::default().event("agent_step").data(data));
            }
        }
        // Se il run e' gia' terminato, emette agent_final con final_answer
        if let Ok(Some(run_row)) = sqlx::query(
            "SELECT status, final_answer FROM agent_runs WHERE id = $1",
        )
        .bind(rid)
        .fetch_optional(&state.db)
        .await
        {
            let status: String = run_row.try_get("status").unwrap_or_default();
            let is_terminal = matches!(
                status.as_str(),
                "completed" | "failed" | "timed_out" | "cancelled" | "interrupted"
                    | "loop_aborted" | "provider_unavailable"
            );
            if is_terminal {
                let final_answer: Option<String> = run_row.try_get("final_answer").unwrap_or(None);
                let data = json!({
                    "runId": rid.to_string(),
                    "isFinal": true,
                    "status": status,
                    "finalAnswer": final_answer,
                })
                .to_string();
                replay_events.push(Event::default().event("agent_final").data(data));
            }
        }
    }

    // Trova il sender nel DashMap per il run specificato (eventi live)
    let sender = if let Some(rid) = run_id {
        state.agent_channels.get(&rid).map(|e| e.value().clone())
    } else {
        state.agent_channels.iter().next().map(|e| e.value().clone())
    };

    let live_stream: futures::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
        match sender {
            None => Box::pin(stream::empty()),
            Some(tx) => {
                let rx = tx.subscribe();
                let s = BroadcastStream::new(rx).filter_map(|msg| async move {
                    match msg {
                        Ok(event) => {
                            let event_type = if event.token_delta.is_some() {
                                "agent_token"
                            } else if event.meta_step.is_some() {
                                "agent_meta_step"
                            } else if event.is_final {
                                "agent_final"
                            } else if event.trace.is_some() {
                                "agent_trace"
                            } else {
                                "agent_step"
                            };
                            let data = if event_type == "agent_token" {
                                serde_json::json!({ "delta": event.token_delta }).to_string()
                            } else {
                                serde_json::to_string(&event).unwrap_or_default()
                            };
                            Some(Ok(Event::default().event(event_type).data(data)))
                        }
                        Err(_) => None,
                    }
                });
                Box::pin(s)
            }
        };

    // Combina replay + live. Replay viene emesso immediatamente (stream::iter),
    // poi il live broadcast prende il sopravvento.
    let replay_stream = stream::iter(replay_events.into_iter().map(Ok));
    let combined = replay_stream.chain(live_stream);
    let boxed: futures::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
        Box::pin(combined);

    Ok(Sse::new(boxed).keep_alive(KeepAlive::default()))
}

/// GET /api/chat/sessions/:session_id/active-run -- restituisce il run attivo (running/awaiting) per una sessione.
/// Usato dal frontend dopo un page refresh per riconnettersi all'agente in corso.
pub async fn get_active_run_for_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    let run_row = sqlx::query(
        "SELECT id, session_id, project_id, user_id, status, automation_mode, provider, model,
                iteration_count, final_answer, pending_actions_json, created_at, completed_at
         FROM agent_runs
         WHERE session_id = $1 AND user_id = $2
           AND status IN ('running', 'awaiting_confirmation')
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(run) = run_row else {
        return Ok(Json(json!({ "activeRun": null })));
    };

    let run_id: Uuid = run.try_get("id").unwrap_or(Uuid::nil());

    let steps = sqlx::query(
        "SELECT id, run_id, step_index, tool_name, tool_input, tool_result, status, created_at
         FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
    )
    .bind(run_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .iter()
    .map(|r| {
        json!({
            "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
            "stepIndex": r.try_get::<i32, _>("step_index").unwrap_or(0),
            "toolName": r.try_get::<String, _>("tool_name").unwrap_or_default(),
            "toolInput": r.try_get::<Value, _>("tool_input").unwrap_or(json!({})),
            "toolResult": r.try_get::<Option<String>, _>("tool_result").unwrap_or(None),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "createdAt": r.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
        })
    })
    .collect::<Vec<_>>();

    let pending: Value = run
        .try_get::<Option<Value>, _>("pending_actions_json")
        .unwrap_or(None)
        .unwrap_or(json!([]));

    Ok(Json(json!({
        "activeRun": {
            "runId": run_id.to_string(),
            "sessionId": run.try_get::<Uuid, _>("session_id").ok().map(|v| v.to_string()),
            "status": run.try_get::<String, _>("status").unwrap_or_default(),
            "automationMode": run.try_get::<String, _>("automation_mode").unwrap_or_default(),
            "provider": run.try_get::<String, _>("provider").unwrap_or_default(),
            "model": run.try_get::<String, _>("model").unwrap_or_default(),
            "iterationCount": run.try_get::<i32, _>("iteration_count").unwrap_or(0),
            "finalAnswer": run.try_get::<Option<String>, _>("final_answer").unwrap_or(None),
            "pendingActions": pending,
            "steps": steps,
            "createdAt": run.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
        }
    })))
}

/// GET /api/chat/agent-runs/:run_id -- legge stato run + steps.
pub async fn get_agent_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(run_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let run_id = Uuid::parse_str(&run_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;

    let run_row = sqlx::query(
        "SELECT id, session_id, project_id, user_id, status, automation_mode, provider, model,
                iteration_count, final_answer, pending_actions_json, created_at, completed_at,
                prompt_tokens, completion_tokens, total_tokens, total_cost
         FROM agent_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(run) = run_row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Agent run non trovato"));
    };

    let owner: Uuid = run.try_get("user_id").unwrap_or(Uuid::nil());
    if owner != user_id {
        return Err(api_error(StatusCode::FORBIDDEN, "Run non accessibile"));
    }

    let steps = sqlx::query(
        "SELECT id, run_id, step_index, tool_name, tool_input, tool_result, status, created_at
         FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
    )
    .bind(run_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .iter()
    .map(|r| {
        json!({
            "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
            "stepIndex": r.try_get::<i32, _>("step_index").unwrap_or(0),
            "toolName": r.try_get::<String, _>("tool_name").unwrap_or_default(),
            "toolInput": r.try_get::<Value, _>("tool_input").unwrap_or(json!({})),
            "toolResult": r.try_get::<Option<String>, _>("tool_result").unwrap_or(None),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "createdAt": r.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
        })
    })
    .collect::<Vec<_>>();

    let pending: Value = run
        .try_get::<Option<Value>, _>("pending_actions_json")
        .unwrap_or(None)
        .unwrap_or(json!([]));

    let prompt_tokens = run.try_get::<i32, _>("prompt_tokens").unwrap_or(0);
    let completion_tokens = run.try_get::<i32, _>("completion_tokens").unwrap_or(0);
    let total_tokens = run.try_get::<i32, _>("total_tokens").unwrap_or(0);
    let total_cost = run.try_get::<f64, _>("total_cost").unwrap_or(0.0);

    // M71: breakdown per coppia provider/model dalla ai_usage_ledger.
    // Mostriamo una riga per ogni provider/modello effettivamente usato nel run
    // (cascade fallback puo' aver coinvolto piu' provider).
    let breakdown_rows = sqlx::query(
        "SELECT provider, model,
                SUM(prompt_tokens)::bigint     AS prompt_tokens,
                SUM(completion_tokens)::bigint AS completion_tokens,
                SUM(total_tokens)::bigint      AS total_tokens,
                SUM(total_cost)::float8        AS total_cost,
                COUNT(*)::int                  AS calls,
                MIN(created_at)                AS first_call_at,
                MAX(created_at)                AS last_call_at
         FROM ai_usage_ledger
         WHERE run_id = $1 AND status = 'finalized'
         GROUP BY provider, model
         ORDER BY MIN(created_at) ASC",
    )
    .bind(run_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let usage_breakdown: Vec<Value> = breakdown_rows
        .iter()
        .map(|r| {
            json!({
                "provider": r.try_get::<String, _>("provider").unwrap_or_default(),
                "model": r.try_get::<String, _>("model").unwrap_or_default(),
                "promptTokens": r.try_get::<i64, _>("prompt_tokens").unwrap_or(0),
                "completionTokens": r.try_get::<i64, _>("completion_tokens").unwrap_or(0),
                "totalTokens": r.try_get::<i64, _>("total_tokens").unwrap_or(0),
                "totalCost": r.try_get::<f64, _>("total_cost").unwrap_or(0.0),
                "calls": r.try_get::<i32, _>("calls").unwrap_or(0),
                "firstCallAt": r.try_get::<DateTime<Utc>, _>("first_call_at").ok().map(|v| v.to_rfc3339()),
                "lastCallAt": r.try_get::<DateTime<Utc>, _>("last_call_at").ok().map(|v| v.to_rfc3339()),
            })
        })
        .collect();

    Ok(Json(json!({
        "runId": run.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
        "sessionId": run.try_get::<Uuid, _>("session_id").ok().map(|v| v.to_string()),
        "status": run.try_get::<String, _>("status").unwrap_or_default(),
        "automationMode": run.try_get::<String, _>("automation_mode").unwrap_or_default(),
        "provider": run.try_get::<String, _>("provider").unwrap_or_default(),
        "model": run.try_get::<String, _>("model").unwrap_or_default(),
        "iterationCount": run.try_get::<i32, _>("iteration_count").unwrap_or(0),
        "finalAnswer": run.try_get::<Option<String>, _>("final_answer").unwrap_or(None),
        "pendingActions": pending,
        "steps": steps,
        "usage": {
            "totalPromptTokens": prompt_tokens,
            "totalCompletionTokens": completion_tokens,
            "totalTokens": total_tokens,
        },
        "totalCostUsd": total_cost,
        "usageBreakdown": usage_breakdown,
        "createdAt": run.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
        "completedAt": run.try_get::<Option<DateTime<Utc>>, _>("completed_at").unwrap_or(None).map(|v| v.to_rfc3339()),
    })))
}

/// POST /api/chat/agent-runs/:run_id/confirm -- approva o annulla le pending actions.
pub async fn confirm_agent_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(run_id): AxumPath<String>,
    Json(body): Json<ConfirmAgentRunRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let run_id = Uuid::parse_str(&run_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;

    // Verifica ownership e stato atteso
    let run_row = sqlx::query(
        "SELECT user_id, status, session_id, project_id, provider, model,
                pending_actions_json, run_message_id, automation_mode
         FROM agent_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(run) = run_row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Agent run non trovato"));
    };

    let owner: Uuid = run.try_get("user_id").unwrap_or(Uuid::nil());
    if owner != user_id {
        return Err(api_error(StatusCode::FORBIDDEN, "Run non accessibile"));
    }

    let status: String = run.try_get("status").unwrap_or_default();
    if status != "awaiting_confirmation" {
        return Err(api_error(StatusCode::CONFLICT, "Il run non e' in attesa di conferma"));
    }

    if !body.approved {
        // Cancella il run
        sqlx::query("UPDATE agent_runs SET status='cancelled', completed_at=NOW() WHERE id=$1")
            .bind(run_id)
            .execute(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(json!({ "runId": run_id.to_string(), "status": "cancelled" })));
    }

    // Approvato: delega la ripresa del loop al brain LangGraph via
    // `POST /agent/approve/{thread_id}`. Il brain mantiene lo state del
    // thread ed e' l'unica sorgente autoritativa del loop (Fase 4 refactor).
    let pending_json: Option<Value> = run
        .try_get::<Option<Value>, _>("pending_actions_json")
        .unwrap_or(None);
    let pending_actions_str = pending_json
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "[]".to_string());

    let resume_message = format!(
        "Azioni confermate dall'utente. Esegui le seguenti operazioni: {}",
        pending_actions_str
    );

    // Segna come running prima di chiamare il brain.
    sqlx::query("UPDATE agent_runs SET status='running', completed_at=NULL WHERE id=$1")
        .bind(run_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match crate::brain_agent_client::resume_run(run_id, true, Some(resume_message)).await {
        Ok(()) => Ok(Json(json!({
            "runId": run_id.to_string(),
            "status": "running",
        }))),
        Err(e) => {
            tracing::error!("confirm_agent_run: brain resume_run fallito run_id={} err={}", run_id, e);
            // Riporta il run a awaiting_confirmation per non lasciarlo appeso.
            let _ = sqlx::query(
                "UPDATE agent_runs SET status='awaiting_confirmation' WHERE id=$1",
            )
            .bind(run_id)
            .execute(&state.db)
            .await;
            Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Brain non raggiungibile per approve: {e}"),
            ))
        }
    }
}

/// POST /api/chat/agent-runs/:run_id/cancel -- interrompe un run in corso.
pub async fn cancel_agent_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(run_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let run_id = Uuid::parse_str(&run_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;

    let run_row = sqlx::query("SELECT user_id, status FROM agent_runs WHERE id = $1")
        .bind(run_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(run) = run_row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Agent run non trovato"));
    };

    let owner: Uuid = run.get::<Uuid, _>("user_id");
    if owner != user_id {
        return Err(api_error(StatusCode::FORBIDDEN, "Run non accessibile"));
    }

    let status: String = run.get::<String, _>("status");
    if status != "running" && status != "awaiting_confirmation" {
        return Ok(Json(json!({ "runId": run_id.to_string(), "status": status, "message": "Run già terminato" })));
    }

    sqlx::query(
        "UPDATE agent_runs SET status='cancelled', completed_at=NOW(), \
         final_answer='Operazione annullata.' WHERE id=$1",
    )
    .bind(run_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Emetti is_final sul broadcast del run: il frontend chiude l'SSE
    // immediatamente e poll il DB per leggere lo stato 'cancelled'. Senza
    // questo evento la UI rimarrebbe "in esecuzione" finche' il tokio::spawn
    // sottostante non termina autonomamente (anche minuti).
    if let Some(ch) = state.agent_channels.get(&run_id) {
        let _ = ch.send(AgentStepEvent {
            run_id: run_id.to_string(),
            step: None,
            trace: None,
            is_final: true,
            token_delta: None,
            meta_step: None,
        });
    }

    Ok(Json(json!({ "runId": run_id.to_string(), "status": "cancelled" })))
}
