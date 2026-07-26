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

/// Carica una riga `agent_runs` eseguendo `sql` (che DEVE selezionare `user_id` e
/// avere `WHERE id = $1`), poi verifica esistenza (404) e ownership (403) rispetto
/// a `user_id`. Punto unico (regola L) per la verifica esistenza+ownership di un
/// run condivisa dagli handler `get_agent_run` / `confirm_agent_run` / `cancel_agent_run`.
/// Ritorna ANCHE il pool del progetto risolto: le tabelle correlate del run
/// (agent_steps, nexus_agent_meta_steps, UPDATE su agent_runs) vivono nello stesso
/// DB del run — i chiamanti DEVONO riusare questo pool, non `state.db` (meta).
async fn fetch_owned_run_row(
    db: &sqlx::PgPool,
    sql: &str,
    run_id: Uuid,
    user_id: Uuid,
) -> Result<(sqlx::postgres::PgRow, sqlx::PgPool), ApiError> {
    // Separazione DB: endpoint keyed solo dal run_id. agent_runs (+ eventuale JOIN
    // chat_sessions) vive nel DB del progetto -> pool via directory di routing
    // (fallback ricerca). Niente fallback al meta (mig 0527): DB progetto non
    // disponibile -> 503 propagato al client.
    let run_pool = crate::project_db_routes::project_data_pool_by_run_from(db, run_id).await?;
    let run_row = sqlx::query(sql)
        .bind(run_id)
        .fetch_optional(&run_pool)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(run) = run_row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Agent run non trovato"));
    };

    let owner: Uuid = run.try_get("user_id").unwrap_or(Uuid::nil());
    if owner != user_id {
        return Err(api_error(StatusCode::FORBIDDEN, "Run non accessibile"));
    }
    Ok((run, run_pool))
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
    let session_id = Uuid::parse_str(&session_id)
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
    // Separazione DB: agent_steps/agent_runs sono tabelle migrate, instradate
    // sul pool del progetto risolto via session_id. Il replay e' best-effort:
    // se il DB del progetto non e' disponibile si salta il replay (WARN) e lo
    // stream prosegue col solo live broadcast, senza fallback al meta (mig 0527).
    let replay_pool = if run_id.is_some() {
        match crate::project_db_routes::project_data_pool_by_session_from(&state.db, session_id)
            .await
        {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "agent_stream: DB progetto non disponibile, salto il replay degli step"
                );
                None
            }
        }
    } else {
        None
    };
    if let (Some(rid), Some(proj_pool)) = (run_id, replay_pool) {
        // Replay step dal DB
        if let Ok(rows) = sqlx::query(
            "SELECT step_index, tool_name, tool_input, tool_result, status, created_at
             FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
        )
        .bind(rid)
        .fetch_all(&proj_pool)
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
        // Replay meta-step dal DB: chiude la stessa race degli step per gli
        // eventi semantici emessi prima che il client apra lo stream (es.
        // Consiglio delle Competenze). Fonte strutturata, nessun parsing testo.
        if let Ok(rows) = sqlx::query(
            "SELECT kind, title, payload, correlation_id, created_at
             FROM nexus_agent_meta_steps WHERE run_id = $1 ORDER BY created_at ASC, id ASC",
        )
        .bind(rid)
        .fetch_all(&proj_pool)
        .await
        {
            for r in rows {
                let kind: String = r.try_get("kind").unwrap_or_default();
                if kind.is_empty() {
                    continue;
                }
                let title: String = r.try_get("title").unwrap_or_default();
                let payload: Value = r.try_get("payload").unwrap_or(json!({}));
                let correlation_id: Option<String> = r.try_get("correlation_id").unwrap_or(None);
                let created_at: chrono::DateTime<chrono::Utc> = r
                    .try_get("created_at")
                    .unwrap_or_else(|_| chrono::Utc::now());
                let data = json!({
                    "runId": rid.to_string(),
                    "metaStep": {
                        "kind": kind,
                        "title": title,
                        "payload": payload,
                        "correlationId": correlation_id,
                        "createdAt": created_at.to_rfc3339(),
                    },
                })
                .to_string();
                replay_events.push(Event::default().event("agent_meta_step").data(data));
            }
        }
        // Se il run e' gia' terminato, emette agent_final con final_answer
        if let Ok(Some(run_row)) =
            sqlx::query("SELECT status, final_answer FROM agent_runs WHERE id = $1")
                .bind(rid)
                .fetch_optional(&proj_pool)
                .await
        {
            let status: String = run_row.try_get("status").unwrap_or_default();
            // Punto unico (regola L): include gli esiti canonici nuovi
            // (failed_diagnosed, completed_verified) che il match inline
            // precedente dimenticava -> un run chiuso con la "determinazione
            // certa" ora viene riconosciuto come terminato nel replay/recovery.
            // awaiting_confirmation resta NON terminale (run sospeso con resume
            // HITL): non si emette agent_final. blocked_needs_input e' TERMINALE
            // (ADR 0034: run concluso con dichiarazione "serve input") -> il
            // replay emette agent_final e la UI mostra l'esito onesto.
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

    // Separazione DB: agent_runs/agent_steps sono tabelle migrate -> pool del
    // progetto risolto dalla sessione. Sul meta le tabelle sono vuote: leggerle
    // li' faceva sparire il run attivo al refresh. Niente fallback (mig 0527).
    let run_pool =
        crate::project_db_routes::project_data_pool_by_session_from(&state.db, session_id).await?;
    let run_row = sqlx::query(&format!(
        "SELECT id, session_id, project_id, user_id, status, automation_mode, provider, model,
                iteration_count, final_answer, pending_actions_json, created_at, completed_at
         FROM agent_runs
         WHERE session_id = $1 AND user_id = $2
           AND status IN ({})
           AND nexus_agent_type IS DISTINCT FROM 'subagent'
         ORDER BY created_at DESC
         LIMIT 1",
        crate::agent_types::ACTIVE_RUN_STATUS_SQL
    ))
    .bind(session_id)
    .bind(user_id)
    .fetch_optional(&run_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(run) = run_row else {
        return Ok(Json(json!({ "activeRun": null })));
    };

    let run_id: Uuid = run.try_get("id").unwrap_or(Uuid::nil());

    // Fix S85: propaga errore SQL invece di mascherarlo come "0 steps".
    let steps = fetch_agent_steps_json(&run_pool, run_id)
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

    let (run, run_pool) = fetch_owned_run_row(
        &state.db,
        "SELECT id, session_id, project_id, user_id, status, automation_mode, provider, model,
                iteration_count, final_answer, pending_actions_json, created_at, completed_at,
                prompt_tokens, completion_tokens, total_tokens, total_cost
         FROM agent_runs WHERE id = $1",
        run_id,
        user_id,
    )
    .await?;

    // Fix S85: propaga errore SQL invece di mascherarlo come "0 steps".
    // Separazione DB: agent_steps vive nello stesso DB del run (pool risolto
    // sopra), NON nel meta — sul meta la tabella e' vuota a flag ON e il
    // pannello mostrava "Nessuno step registrato" su run con step persistiti.
    let steps = fetch_agent_steps_json(&run_pool, run_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let pending: Value = run
        .try_get::<Option<Value>, _>("pending_actions_json")
        .unwrap_or(None)
        .unwrap_or(json!([]));

    let prompt_tokens = run.try_get::<i32, _>("prompt_tokens").unwrap_or(0);
    let completion_tokens = run.try_get::<i32, _>("completion_tokens").unwrap_or(0);
    let total_tokens = run.try_get::<i32, _>("total_tokens").unwrap_or(0);
    // Costo del run INCLUSI i suoi sub-run (figure del consiglio, revisori,
    // sub-agenti dispatchati): girano su provider PROPRI e con run_id propri,
    // quindi il solo `agent_runs.total_cost` del padre ne ignorava la spesa.
    // Misurato su verifica-wd: card a "mistral $0.0986" mentre il costo reale del
    // run era $0.1337 su 6 run (openrouter 0.0170, google 0.0085, deepseek
    // 0.0048, mistral figli 0.0048) -- il 26% mancava, ed era esattamente il
    // lavoro delle figure che l'utente vedeva elencate sopra la card.
    let costo_figli: f64 = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(SUM(total_cost), 0)::float8 FROM agent_runs WHERE parent_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&run_pool)
    .await
    .unwrap_or(0.0);
    let total_cost = run.try_get::<f64, _>("total_cost").unwrap_or(0.0) + costo_figli;

    // Gli id su cui contare la spesa: il run e i suoi sub-run. Serve al breakdown
    // per elencare anche i provider delle figure, che altrimenti comparivano
    // nella narrazione ma non nel riepilogo dei costi.
    let mut run_ids_con_figli: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM agent_runs WHERE parent_run_id = $1",
    )
    .bind(run_id)
    .fetch_all(&run_pool)
    .await
    .unwrap_or_default();
    run_ids_con_figli.push(run_id);

    // M71: breakdown per coppia provider/model dalla ai_usage_ledger.
    // Mostriamo una riga per ogni provider/modello effettivamente usato nel run
    // (cascade fallback puo' aver coinvolto piu' provider).
    // ai_usage_ledger e' contabilita' di PIATTAFORMA (scritta dal gateway sul
    // meta-DB): qui `state.db` e' corretto, NON va instradata sul pool progetto.
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
         WHERE run_id = ANY($1) AND status = 'finalized'
         GROUP BY provider, model
         ORDER BY MIN(created_at) ASC",
    )
    .bind(&run_ids_con_figli)
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
    let context = crate::chat_sessions::load_session_context(&state, session_id, user_id).await?;
    // Worklog nel DB del progetto (separazione DB): pool risolto dalla sessione.
    let wpool =
        crate::project_db_routes::project_data_pool_by_session_from(&state.db, context.session_id)
            .await?;
    let block = crate::session_worklog::fetch_rendered_block(&state.db, &wpool, context.session_id)
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

    // Separazione DB: nexus_agent_meta_steps + agent_runs vivono nel DB del
    // progetto -> pool risolto dal run. Niente fallback al meta (mig 0527).
    let run_pool =
        crate::project_db_routes::project_data_pool_by_run_from(&state.db, run_id).await?;
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
    .fetch_optional(&run_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let choices: Value = row
        .and_then(|r| r.try_get::<Option<Value>, _>("payload").ok().flatten())
        .and_then(|p| p.get("choices").cloned())
        .unwrap_or_else(|| json!([]));

    Ok(Json(json!({ "choices": choices })))
}

/// GET /api/chat/sessions/:session_id/meta-steps -- ripristina l'INTERA timeline
/// dei meta_step (plan/routing/clarify/fallback/reflection/next_actions) persistiti
/// per i run della sessione. Gemello di `get_agent_run_next_actions` ma esteso a
/// tutta la sessione e a tutti i kind: serve a ricostruire `metaStepsMap` nel
/// frontend dopo un reload, dato che gli eventi SSE vivono solo in memoria e si
/// perdono al refresh (la timeline delle card sparirebbe pur restando nel DB).
/// Risposta: { runs: { "<run_id>": [{kind,title,payload,correlationId,createdAt}] } }.
pub async fn get_session_meta_steps(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    // Ownership via filtro sui run della sessione di proprieta' dell'utente:
    // nessun leak cross-utente. Si limita agli ULTIMI 30 run (per evitare di
    // caricare timeline sterminate) ma includendo SEMPRE i run piu' recenti --
    // un LIMIT globale con ORDER BY created_at ASC taglierebbe proprio l'ultimo
    // run, cioe' il caso d'uso del refresh. I meta_step tornano in ordine
    // cronologico per la ricostruzione fedele della timeline.
    // Separazione DB: nexus_agent_meta_steps + agent_runs vivono nel DB del
    // progetto -> pool risolto dalla sessione. Sul meta le tabelle sono vuote:
    // la timeline narrativa spariva al reload della chat. Niente fallback (mig 0527).
    let run_pool =
        crate::project_db_routes::project_data_pool_by_session_from(&state.db, session_id).await?;
    let rows = sqlx::query(
        "SELECT m.run_id, m.kind, m.title, m.payload, m.correlation_id, m.created_at
         FROM nexus_agent_meta_steps m
         WHERE m.run_id IN (
             SELECT id FROM agent_runs
             WHERE session_id = $1 AND user_id = $2
               AND nexus_agent_type IS DISTINCT FROM 'subagent'
             ORDER BY created_at DESC
             LIMIT 30
         )
         ORDER BY m.created_at ASC",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_all(&run_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // STATO REALE DEI TODO del piano. Il payload del meta-step "plan" e' la
    // FOTOGRAFIA scattata quando il piano e' stato creato: tutti i todo
    // `pending`. Gli aggiornamenti live (`TodoUpdated`) vivono solo in memoria
    // nel client, quindi bastava un reload -- o un run finito male, che e'
    // proprio il momento in cui si vuole sapere cosa e' stato fatto -- perche' la
    // checklist tornasse a mostrare TUTTO da fare a lavoro svolto.
    // Qui il payload viene servito con lo stato CORRENTE letto da
    // `nexus_agent_todos`, cosi' la vista non racconta piu' un piano fermo.
    // Best-effort: se la lettura fallisce si serve la fotografia, come prima.
    let plan_runs: Vec<Uuid> = rows
        .iter()
        .filter(|r| {
            r.try_get::<String, _>("kind")
                .map(|k| k == "plan")
                .unwrap_or(false)
        })
        .filter_map(|r| r.try_get::<Uuid, _>("run_id").ok())
        .collect();
    let mut stato_todo: std::collections::HashMap<(Uuid, String), String> =
        std::collections::HashMap::new();
    if !plan_runs.is_empty() {
        let righe: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT run_id, id::text, status FROM nexus_agent_todos WHERE run_id = ANY($1)",
        )
        .bind(&plan_runs)
        .fetch_all(&run_pool)
        .await
        .unwrap_or_default();
        for (r, id, st) in righe {
            stato_todo.insert((r, id), st);
        }
    }

    // Raggruppa per run_id nel formato MetaStepEntry atteso dal frontend.
    let mut runs: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for row in rows {
        let run_id: Uuid = match row.try_get("run_id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind: String = row.try_get("kind").unwrap_or_default();
        let title: String = row.try_get("title").unwrap_or_default();
        let mut payload: Value = row
            .try_get::<Option<Value>, _>("payload")
            .ok()
            .flatten()
            .unwrap_or_else(|| json!({}));
        // Sovrascrive lo status di ogni todo con quello attuale (vedi sopra). I
        // todo assenti dalla mappa restano com'erano: un piano di cui non si
        // trovano piu' le righe si serve invariato, non azzerato.
        if kind == "plan" {
            if let Some(todos) = payload.get_mut("todos").and_then(|t| t.as_array_mut()) {
                for t in todos.iter_mut() {
                    let id = t.get("id").and_then(|v| v.as_str()).map(str::to_string);
                    if let Some(st) = id.and_then(|i| stato_todo.get(&(run_id, i))) {
                        t["status"] = json!(st);
                    }
                }
            }
        }
        let correlation_id: Option<String> = row.try_get("correlation_id").ok().flatten();
        let created_at: chrono::DateTime<chrono::Utc> = row
            .try_get("created_at")
            .unwrap_or_else(|_| chrono::Utc::now());
        runs.entry(run_id.to_string()).or_default().push(json!({
            "kind": kind,
            "title": title,
            "payload": payload,
            "correlationId": correlation_id,
            "createdAt": created_at.to_rfc3339(),
        }));
    }

    Ok(Json(json!({ "runs": runs })))
}

/// GET /api/chat/sessions/:session_id/traces -- ripristina le tracce gateway LLM
/// (AITraceEvent: provider/model effettivi, token, stop_reason per iterazione)
/// persistite per i run della sessione. Gemello di `get_session_meta_steps` ma
/// per le tracce (nexus_agent_traces, mig 0485): serve a ricostruire il trace
/// panel dopo un reload, dato che gli eventi SSE `agent_trace` vivono solo in
/// memoria/sessionStorage e si perdono al refresh.
/// Risposta: { runs: { "<run_id>": [AITraceEvent...] } }.
pub async fn get_session_traces(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    // Separazione DB: nexus_agent_traces vive nel DB del progetto (scritta dal
    // motore nativo sul run_db) -> pool risolto dalla sessione.
    let run_pool =
        crate::project_db_routes::project_data_pool_by_session_from(&state.db, session_id).await?;
    // Punto unico (regola L): ownership + raggruppamento per run nel trace_store,
    // speculare a get_session_meta_steps.
    let runs = crate::trace_store::get_session_traces(&run_pool, session_id, user_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "runs": runs })))
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

    // Verifica ownership e stato atteso. Il pool del progetto e' quello risolto
    // dal punto unico fetch_owned_run_row: riusato per tutti gli UPDATE sotto
    // (separazione DB: agent_runs e' migrata sul pool del progetto).
    let (run, run_pool) = fetch_owned_run_row(
        &state.db,
        "SELECT user_id, status, session_id, project_id, provider, model,
                pending_actions_json, run_message_id, automation_mode, engine
         FROM agent_runs WHERE id = $1",
        run_id,
        user_id,
    )
    .await?;

    let status: String = run.try_get("status").unwrap_or_default();

    if !body.approved {
        if status != "awaiting_confirmation" {
            return Err(api_error(
                StatusCode::CONFLICT,
                "Il run non e' in attesa di conferma",
            ));
        }
        // Cancella il run
        sqlx::query("UPDATE agent_runs SET status='cancelled', completed_at=NOW() WHERE id=$1")
            .bind(run_id)
            .execute(&run_pool)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(Json(
            json!({ "runId": run_id.to_string(), "status": "cancelled" }),
        ));
    }

    // Idempotenza approve: un doppio click (o timeout client + retry) puo'
    // arrivare dopo che lo status e' gia' `running` (UPDATE sincrono sotto).
    // Rispondi 200 coerente invece di 409 per non far fallire la UI.
    if status == "running" {
        return Ok(Json(json!({
            "runId": run_id.to_string(),
            "status": "running",
        })));
    }

    if status != "awaiting_confirmation" {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Il run non e' in attesa di conferma",
        ));
    }

    // Approvato: il resume va instradato al MOTORE su cui girava il run (regola L:
    // un solo punto decide il motore, `agent_runs.engine`, lo stesso scritto a
    // spawn da `select_engine`). I due motori hanno checkpoint NON
    // interscambiabili, quindi un run nativo deve riprendere dal grafo nativo, non
    // dal brain.
    //   - engine='rust': resume IN-PROCESS sul grafo nativo (dal checkpoint
    //     Postgres `nexus_graph_checkpoints`), finalizzato da mcp-core.
    //   - altrimenti (python / NULL legacy): resume sul brain via
    //     `POST /agent/approve/{thread_id}` (comportamento storico invariato).
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

    // Segna come running prima di riprendere (sia nativo sia brain).
    sqlx::query("UPDATE agent_runs SET status='running', completed_at=NULL WHERE id=$1")
        .bind(run_id)
        .execute(&run_pool)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Resume nativo in BACKGROUND: la POST risponde subito con `running` cosi' il
    // client non va in timeout HTTP mentre il grafo riprende dal checkpoint.
    // Finalizzazione + is_final SSE dentro `confirm_native_run`.
    //
    // Qui si leggeva `agent_runs.engine` e, se non era 'rust', si mandava la
    // conferma al brain Python. La colonna e' NULLABLE e i run legacy hanno NULL
    // (mig 0451): per quelle righe la conferma HITL finiva su un servizio rimosso
    // e l'utente vedeva "Brain non raggiungibile". Il motore e' uno solo: il
    // valore della colonna non decide piu' nulla.
    let session_id: Uuid = run.get::<Uuid, _>("session_id");
    let provider: String = run.try_get("provider").unwrap_or_default();
    let model: String = run.try_get("model").unwrap_or_default();
    let automation_mode: String = run
        .try_get::<Option<String>, _>("automation_mode")
        .ok()
        .flatten()
        .unwrap_or_default();
    let state_bg = state.clone();
    let resume_message_bg = resume_message.clone();
    tokio::spawn(async move {
        let _ = crate::chat_messages::confirm_native_run(
            &state_bg,
            run_id,
            session_id,
            provider,
            model,
            automation_mode,
            &resume_message_bg,
        )
        .await;
    });
    Ok(Json(json!({
        "runId": run_id.to_string(),
        "status": "running",
    })))
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

    let (run, _run_pool) = fetch_owned_run_row(
        &state.db,
        "SELECT user_id, session_id, status FROM agent_runs WHERE id = $1",
        run_id,
        user_id,
    )
    .await?;

    let session_id: Uuid = run.get::<Uuid, _>("session_id");
    let status: String = run.get::<String, _>("status");
    if status != "running" && status != "awaiting_confirmation" {
        // Anche se il run target e' gia' terminale, sblocchiamo eventuali ALTRI
        // run rimasti stuck sulla stessa sessione (vedi fix cascade sotto):
        // l'interruzione richiesta dall'utente deve sempre liberare la sessione.
    }

    // Cancel CASCADING per sessione (fix architetturale): l'invariante "max 1
    // run attivo per sessione" che assume la guardia 409 (handlers.rs) puo'
    // venire violata da path "resume", auto-continuation o race condition tra
    // INSERT e cleanup. Senza cascade, una singola interruzione lascia un secondo
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
