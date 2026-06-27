//! HTTP routes per il dispatcher centrale di eventi.
//!
//! - `GET /api/projects/:id/event-stream?topics=...` — SSE live (tutti i pannelli)
//! - `GET /api/projects/:id/snapshot?topics=...` — bootstrap iniziale o post-`SnapshotRequired`
//! - `GET /api/projects/:id/flags` — read-only stato flag (incluso in snapshot)
//!
//! Pattern derivato da `chat_agent.rs::agent_stream` (linee 32-86).

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::stream::{self, StreamExt};
use nexus_events::{
    dispatcher,
    event::{EnvelopedEvent, ProjectEvent, SCHEMA_VERSION},
};
use serde_json::{json, Value};
use sqlx::Row;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

use crate::{
    auth::Claims,
    projects::{api_error, load_project_context, parse_user_id},
    AppState,
};

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn parse_topics(s: Option<&String>) -> Option<HashSet<String>> {
    s.map(|raw| {
        raw.split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && t != "*")
            .collect::<HashSet<String>>()
    })
    .filter(|set| !set.is_empty())
}

/// GET /api/projects/:id/event-stream?topics=playwright,ports&since=42
///
/// Live stream SSE di tutti gli `EnvelopedEvent` del progetto, filtrati per
/// topic (omettere o passare `*` per ricevere tutto). Header
/// `Last-Event-ID` (o query `since`) abilita il replay dal ring buffer.
pub async fn event_stream(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ctx = load_project_context(&state.db, project_id, user_id).await?;

    let topics = parse_topics(params.get("topics"));
    let topics_for_filter = topics.clone();

    // Replay: priorita' Last-Event-ID, poi query `since`
    let since: Option<u64> = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .or_else(|| params.get("since").and_then(|s| s.parse().ok()));

    // Registra il canale (lazy-create) e ottieni receiver
    let handle = dispatcher::register(&state.project_channels, project_id);
    let rx = handle.subscribe();

    // Replay eventuale dal ring buffer per chiudere il gap
    let replay_events: Vec<EnvelopedEvent> = if let Some(last_seq) = since {
        match dispatcher::replay_since(&state.project_channels, project_id, last_seq) {
            Some(evs) => evs,
            None => {
                // Gap troppo grande: invia subito SnapshotRequired
                vec![EnvelopedEvent::new(
                    project_id,
                    0,
                    ProjectEvent::SnapshotRequired {
                        reason: "gap_oltre_ring_buffer".into(),
                        last_known_seq: last_seq,
                    },
                    None,
                )]
            }
        }
    } else {
        Vec::new()
    };

    // Evento iniziale "connected" per forzare il flush degli header attraverso
    // proxy che bufferizzano (Next.js rewrite). Senza questo primo byte, il
    // browser EventSource resta in stato CONNECTING perche' il proxy trattiene
    // la risposta fino al primo dato.
    let init_event = Event::default().comment("connected");
    let init_stream = stream::once(async { Ok::<Event, std::convert::Infallible>(init_event) });

    let replay_stream = stream::iter(replay_events.into_iter().map(Ok));

    let live_stream = BroadcastStream::new(rx).filter_map(move |msg| {
        let topics = topics_for_filter.clone();
        async move {
            match msg {
                Ok(env) => Some(Ok(env)),
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    let _ = topics;
                    tracing::warn!(missed = n, "event_stream live: Lagged");
                    None
                }
            }
        }
    });

    let event_stream = replay_stream.chain(live_stream).filter_map(
        move |res: Result<EnvelopedEvent, std::convert::Infallible>| {
            let topics = topics.clone();
            async move {
                let env = match res {
                    Ok(e) => e,
                    Err(_) => return None,
                };
                // Filtro topic (se specificato)
                if let Some(ref allowed) = topics {
                    if !allowed.contains(&env.topic) {
                        return None;
                    }
                }
                let event_type = env.payload.kind_name();
                let id_str = env.seq.to_string();
                let data = serde_json::to_string(&env).unwrap_or_default();
                Some(Ok(Event::default().event(event_type).id(id_str).data(data)))
            }
        },
    );

    // Combina: init (flush immediato) + replay + live, tutto in un unico stream
    let full_stream = init_stream.chain(event_stream);
    let boxed: futures::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
        Box::pin(full_stream);

    // Header anti-buffering: necessari quando il response transita attraverso
    // proxy (Next.js rewrite, nginx, ecc.) che altrimenti accumulano i chunk
    // SSE e li rilasciano in blocco solo alla chiusura della connessione.
    let mut resp = Sse::new(boxed)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response();
    let h = resp.headers_mut();
    h.insert(
        "Cache-Control",
        HeaderValue::from_static("no-store, no-transform"),
    );
    h.insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    Ok(resp)
}

/// GET /api/projects/:id/snapshot?topics=playwright,ports
///
/// Snapshot consolidato dello stato corrente, usato per bootstrap iniziale
/// del frontend o dopo `SnapshotRequired`. Aggrega gli endpoint REST gia'
/// esistenti (`get_playwright_runs`, `get_project_ports`, `get_project_problems`)
/// + flag e monitor in-memory.
pub async fn project_snapshot(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ctx = load_project_context(&state.db, project_id, user_id).await?;

    let topics_filter = parse_topics(params.get("topics"));
    let want = |t: &str| topics_filter.as_ref().is_none_or(|s| s.contains(t));

    // Seq corrente (cliente userra' come `since` per il successivo event-stream)
    // Seq corrente reale del canale (P2): il client lo usa come `since` per
    // riagganciare lo stream senza perdere eventi. Prima era hardcoded a 0 ->
    // snapshot incoerente / SnapshotRequired ricorrenti.
    let current_seq = state
        .project_channels
        .get(&project_id)
        .map(|ch| ch.current_seq())
        .unwrap_or(0);

    let mut snapshot = json!({
        "project_id": project_id,
        "schema_version": SCHEMA_VERSION,
        "seq": current_seq,
    });

    // ── Playwright runs ───────────────────────────────────────────────
    if want(nexus_events::event::TOPIC_PLAYWRIGHT) {
        let rows = sqlx::query(
            r#"SELECT id, kind, status, input, created_at
               FROM jobs
               WHERE project_id = $1 AND kind ILIKE '%playwright%'
               ORDER BY created_at DESC LIMIT 50"#,
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let runs: Vec<Value> = rows
            .into_iter()
            .map(|row| {
                let input = row.get::<Value, _>("input");
                json!({
                    "id": row.get::<Uuid, _>("id").to_string(),
                    "label": input.get("label").and_then(Value::as_str).unwrap_or("Playwright run"),
                    "status": row.get::<String, _>("status"),
                    "summary": input.get("message").and_then(Value::as_str),
                    "artifacts": input.get("artifacts").cloned().unwrap_or_else(|| json!([])),
                    "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                })
            })
            .collect();
        snapshot["playwright"] = json!({ "runs": runs });
    }

    // ── Flags ─────────────────────────────────────────────────────────
    if want(nexus_events::event::TOPIC_FLAGS) {
        let rows = sqlx::query("SELECT key, value FROM nexus_project_flags WHERE project_id = $1")
            .bind(project_id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let mut flags = serde_json::Map::new();
        for row in rows {
            let k: String = row.get("key");
            let v: Value = row.get("value");
            flags.insert(k, v);
        }
        snapshot["flags"] = Value::Object(flags);
    }

    // ── Monitors (in-memory) ──────────────────────────────────────────
    if want(nexus_events::event::TOPIC_MONITOR) {
        let monitors = state
            .monitor_registry
            .read()
            .get(&project_id)
            .cloned()
            .unwrap_or_default();
        snapshot["monitors"] = json!(monitors);
    }

    // Per i topic restanti, indichiamo al client di usare gli endpoint REST
    // gia' esistenti (problems, ports, services, git) per evitare di duplicare
    // logica. Il client li chiamera' in parallelo al bootstrap.
    let pending: Vec<&str> = [
        nexus_events::event::TOPIC_PROBLEMS,
        nexus_events::event::TOPIC_PORTS,
        nexus_events::event::TOPIC_SERVICES,
        nexus_events::event::TOPIC_GIT,
        nexus_events::event::TOPIC_FILES,
        nexus_events::event::TOPIC_DATABASE,
    ]
    .iter()
    .copied()
    .filter(|t| want(t))
    .collect();
    snapshot["fetch_topics"] = json!(pending);
    Ok(Json(snapshot))
}

/// POST /api/projects/:id/dispatcher/test
///
/// Endpoint di TEST/DIAGNOSTICA: invoca direttamente le primitive dispatcher
/// (notification, set_flag, update_monitor, highlight) come farebbe l'agente,
/// senza richiedere brain attivo. Pensato per smoke test E2E del pipeline
/// dispatcher → SSE → UI. Body JSON con campo `action` discriminator.
pub async fn dispatcher_test(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ctx = load_project_context(&state.db, project_id, user_id).await?;

    let action = body
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'action' mancante"))?;

    let env = match action {
        "notification" => {
            let severity = body
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("info")
                .to_string();
            let message = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Test notification")
                .to_string();
            dispatcher::emit(
                &state.project_channels,
                project_id,
                ProjectEvent::Notification {
                    severity,
                    message,
                    panel: body
                        .get("panel")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    ttl_ms: body.get("ttl_ms").and_then(Value::as_u64),
                    run_id: None,
                },
            )
        }
        "set_flag" => {
            let key = body
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'key' mancante"))?
                .to_string();
            let value = body.get("value").cloned().unwrap_or(Value::Null);
            // Persisti su DB (come fa tool_dispatcher_set_flag)
            let _ = sqlx::query(
                r#"INSERT INTO nexus_project_flags (project_id, key, value)
                   VALUES ($1, $2, $3)
                   ON CONFLICT (project_id, key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()"#,
            )
            .bind(project_id)
            .bind(&key)
            .bind(&value)
            .execute(&state.db)
            .await;
            dispatcher::emit(
                &state.project_channels,
                project_id,
                ProjectEvent::FlagChanged { key, value },
            )
        }
        "update_monitor" => {
            let monitor_id = body
                .get("monitor_id")
                .and_then(Value::as_str)
                .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'monitor_id' mancante"))?
                .to_string();
            let value = body.get("value").cloned().unwrap_or(Value::Null);
            let label = body
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string);
            // Salva in registry in-memory (HashMap<Uuid, HashMap<String, Value>>)
            let mut reg = state.monitor_registry.write();
            let project_map = reg.entry(project_id).or_default();
            project_map.insert(
                monitor_id.clone(),
                json!({
                    "value": value.clone(),
                    "label": label.clone(),
                    "updated_at": chrono::Utc::now().to_rfc3339(),
                }),
            );
            drop(reg);
            dispatcher::emit(
                &state.project_channels,
                project_id,
                ProjectEvent::MonitorUpdated {
                    monitor_id,
                    value,
                    label,
                },
            )
        }
        "highlight" => {
            let panel = body
                .get("panel")
                .and_then(Value::as_str)
                .unwrap_or("playwright")
                .to_string();
            let duration_ms = body
                .get("duration_ms")
                .and_then(Value::as_u64)
                .unwrap_or(2000);
            dispatcher::emit(
                &state.project_channels,
                project_id,
                ProjectEvent::HighlightPanel { panel, duration_ms },
            )
        }
        "file_changed" => {
            let path = body
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'path' mancante"))?
                .to_string();
            let op = body
                .get("op")
                .and_then(Value::as_str)
                .unwrap_or("modified")
                .to_string();
            dispatcher::emit(
                &state.project_channels,
                project_id,
                ProjectEvent::FileChanged { path, op },
            )
        }
        other => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!("action sconosciuta: {}", other),
            ))
        }
    };

    Ok(Json(json!({
        "ok": true,
        "emitted": {
            "event_id": env.event_id,
            "seq": env.seq,
            "topic": env.topic,
            "kind": env.payload.kind_name(),
        }
    })))
}
