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
    auth::Claims,
    chat_learning::{api_error, parse_user_id, ApiError, ApiResult},
    AppState,
};

/// Fetch dei step di un agent_run come array JSON pronto per la response.
/// Punto unico (regola L, S36) per il blocco SELECT + mapping duplicato fra
/// `get_active_run` e `get_agent_run`.
async fn fetch_agent_steps_json(
    db: &sqlx::PgPool,
    run_id: Uuid,
) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, run_id, step_index, tool_name, tool_input, tool_result, status, created_at
         FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
    )
    .bind(run_id)
    .fetch_all(db)
    .await?;
    Ok(rows
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
    .collect())
}

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
) -> Result<
    Sse<futures::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>>,
    ApiError,
> {
    let _user_id = parse_user_id(&claims)?;
    let _session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    let run_id: Option<Uuid> = params.get("run_id").and_then(|s| Uuid::parse_str(s).ok());

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
        if let Ok(Some(run_row)) =
            sqlx::query("SELECT status, final_answer FROM agent_runs WHERE id = $1")
                .bind(rid)
                .fetch_optional(&state.db)
                .await
        {
            let status: String = run_row.try_get("status").unwrap_or_default();
            // Punto unico (regola L): include gli esiti canonici nuovi
            // (failed_diagnosed, completed_verified) che il match inline
            // precedente dimenticava -> un run chiuso con la "determinazione
            // certa" ora viene riconosciuto come terminato nel replay/recovery.
            // awaiting_confirmation/blocked_needs_input restano NON terminali
            // (run in pausa che attende input): non si emette agent_final.
            let is_terminal =
                crate::agent_types::AgentRunStatus::from_db_str(&status).is_terminal();
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
        state
            .agent_channels
            .iter()
            .next()
            .map(|e| e.value().clone())
    };

    let live_stream: futures::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
        match sender {
            None => Box::pin(stream::empty()),
            Some(tx) => {
                let rx = tx.subscribe();
                let s = BroadcastStream::new(rx).filter_map(|msg| async move {
                    match msg {
                        Ok(event) => {
                            // Snapshot token cumulativi (kind="usage_snapshot"):
                            // mappato a `agent_usage` PRIMA del ramo meta_step
                            // generico, cosi' non appare come card meta-step in
                            // chat ma alimenta la barra context live nel frontend.
                            let is_usage_snapshot = event
                                .meta_step
                                .as_ref()
                                .map(|m| m.kind == "usage_snapshot")
                                .unwrap_or(false);
                            let event_type = if event.token_delta.is_some() {
                                "agent_token"
                            } else if event.thinking_delta.is_some() {
                                "agent_thinking"
                            } else if is_usage_snapshot {
                                "agent_usage"
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
                            } else if event_type == "agent_thinking" {
                                serde_json::json!({ "text": event.thinking_delta }).to_string()
                            } else if event_type == "agent_usage" {
                                // Solo il payload con i token (camelCase gia' pronto).
                                event
                                    .meta_step
                                    .as_ref()
                                    .map(|m| m.payload.to_string())
                                    .unwrap_or_else(|| "{}".to_string())
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

    // Fix S85: propaga errore SQL invece di mascherarlo come "0 steps".
    let steps = fetch_agent_steps_json(&state.db, run_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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

    // Fix S85: propaga errore SQL invece di mascherarlo come "0 steps".
    let steps = fetch_agent_steps_json(&state.db, run_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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

/// GET /api/chat/sessions/:id/worklog -- digest provider-neutro della storia di
/// lavoro della sessione (mig 0411). Espone all'utente "cosa e' stato fatto"
/// (file toccati, comandi con esito, errori, tentativi falliti, decisioni), lo
/// stesso testo iniettato nel system_text dell'LLM. Stringa vuota se il worklog
/// e' assente o disabilitato.
pub async fn get_session_worklog(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;
    let context = crate::chat_sessions::load_session_context(&state.db, session_id, user_id).await?;
    let block = crate::session_worklog::fetch_rendered_block(&state.db, context.session_id)
        .await
        .unwrap_or_default();
    Ok(Json(json!({ "renderedBlock": block })))
}

/// GET /api/chat/agent-runs/:run_id/next-actions -- scelte di proseguimento
/// (meta_step `next_actions`) persistite per il run. Serve a RIPRISTINARE i
/// pulsanti delle scelte dopo un reload o sui turni passati: i meta_step live
/// arrivano via SSE e si perdono al refresh, mentre qui li rileggiamo dal DB
/// (nexus_agent_meta_steps). Ritorna l'ULTIMA card del run (le precedenti sono
/// tentativi superati dai fallback). Sempre {choices: [...]}, eventualmente vuoto.
pub async fn get_agent_run_next_actions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(run_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let run_id = Uuid::parse_str(&run_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;

    // Ownership verificata via join su agent_runs.user_id: nessun leak cross-utente.
    let row = sqlx::query(
        "SELECT m.payload
         FROM nexus_agent_meta_steps m
         JOIN agent_runs r ON r.id = m.run_id
         WHERE m.run_id = $1 AND m.kind = 'next_actions' AND r.user_id = $2
         ORDER BY m.created_at DESC
         LIMIT 1",
    )
    .bind(run_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let choices: Value = row
        .and_then(|r| r.try_get::<Option<Value>, _>("payload").ok().flatten())
        .and_then(|p| p.get("choices").cloned())
        .unwrap_or_else(|| json!([]));

    Ok(Json(json!({ "choices": choices })))
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
        return Err(api_error(
            StatusCode::CONFLICT,
            "Il run non e' in attesa di conferma",
        ));
    }

    if !body.approved {
        // Cancella il run
        sqlx::query("UPDATE agent_runs SET status='cancelled', completed_at=NOW() WHERE id=$1")
            .bind(run_id)
            .execute(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(
            json!({ "runId": run_id.to_string(), "status": "cancelled" }),
        ));
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
            tracing::error!(
                "confirm_agent_run: brain resume_run fallito run_id={} err={}",
                run_id,
                e
            );
            // Riporta il run a awaiting_confirmation per non lasciarlo appeso.
            let _ = sqlx::query("UPDATE agent_runs SET status='awaiting_confirmation' WHERE id=$1")
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

    let run_row = sqlx::query("SELECT user_id, session_id, status FROM agent_runs WHERE id = $1")
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

    let session_id: Uuid = run.get::<Uuid, _>("session_id");
    let status: String = run.get::<String, _>("status");
    if status != "running" && status != "awaiting_confirmation" {
        // Anche se il run target e' gia' terminale, sblocchiamo eventuali ALTRI
        // run rimasti stuck sulla stessa sessione (vedi fix cascade sotto): il
        // 'Forza Stop' lato utente deve sempre liberare la sessione.
    }

    // Cancel CASCADING per sessione (fix architetturale): l'invariante "max 1
    // run attivo per sessione" che assume la guardia 409 (handlers.rs) puo'
    // venire violata da path "resume", auto-continuation o race condition tra
    // INSERT e cleanup. Senza cascade, un singolo Forza Stop lascia un secondo
    // run "running" stuck nel DB per fino a 15 min (sintomo osservato: dopo
    // Stop, la POST successiva sulla stessa sessione torna 409 ripetuto).
    // Cancellando TUTTI i run attivi della sessione si ristabilisce
    // l'invariante e si sblocca subito l'utente, in modo idempotente.
    // Delega al punto unico (regola L): stessa logica autoritativa usata dal
    // last-wins di spawn_agent_run e dal resume. Marca i run attivi della
    // sessione 'cancelled' + cancellation_requested (segnale di stop cooperativo
    // che il brain rispetta tra le iterazioni) ed emette is_final sui channel.
    let cancelled_ids =
        crate::chat_messages::supersede_active_runs(&state, session_id, "user_cancel").await;

    Ok(Json(json!({
        "runId": run_id.to_string(),
        "status": "cancelled",
        "cancelledIds": cancelled_ids.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
    })))
}
