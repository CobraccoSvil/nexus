use axum::{
    extract::{MatchedPath, State},
    http::{Method, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

use crate::{auth, AppState};

/// Middleware di timing HTTP: registra durata/route/status nell'histogram
/// in-memory (`http_metrics`), esposto da `GET /nexus/metrics`. Usa il
/// template di route (`MatchedPath`) come label, mai il path raw: gli UUID
/// nei path non devono esplodere la cardinalita'. Richieste senza route
/// matchata (404 da fallback) sono ignorate di proposito.
pub async fn http_timing_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let method = req.method().as_str().to_string();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());
    let start = std::time::Instant::now();

    let resp = next.run(req).await;

    if let Some(route) = route {
        crate::http_metrics::record(&route, &method, resp.status().as_u16(), start.elapsed());
    }
    resp
}

pub async fn require_auth(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let claims = auth::validate_token(&state.db, req.headers()).await?;
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

pub async fn require_admin(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    tracing::warn!("require_admin: CHECKING path={}", req.uri());
    match auth::validate_token(&state.db, req.headers()).await {
        Ok(claims) => {
            tracing::warn!(
                "require_admin: user={}, role={}, path={}",
                claims.sub,
                claims.role,
                req.uri()
            );
            if claims.role != "admin" {
                tracing::warn!(
                    "require_admin: access denied - role={} is not admin",
                    claims.role
                );
                return Err(StatusCode::FORBIDDEN);
            }
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(e) => {
            tracing::warn!(
                "require_admin: token validation failed: {:?}, path={}",
                e,
                req.uri()
            );
            Err(e)
        }
    }
}

/// Middleware catch-all: cattura risposte 2xx su verbi mutate (POST/PUT/DELETE/PATCH)
/// ed emette `MutationRecorded` via il dispatcher.
pub async fn event_capture_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    let resp = next.run(req).await;

    let is_mutation = matches!(
        method,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    );
    if is_mutation && resp.status().is_success() {
        if let Some(pid) = extract_project_id(&path) {
            let session_id = extract_session_id(&path);
            nexus_events::dispatcher::emit(
                &state.project_channels,
                pid,
                nexus_events::ProjectEvent::MutationRecorded {
                    method: method.to_string(),
                    path,
                    status_code: resp.status().as_u16(),
                    session_id,
                    summary: None,
                    actor_user_id: None,
                },
            );
        }
    }

    resp
}

fn extract_project_id(path: &str) -> Option<Uuid> {
    let parts: Vec<&str> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "projects" {
            if let Some(next) = parts.get(i + 1) {
                return next.parse::<Uuid>().ok();
            }
        }
    }
    None
}

fn extract_session_id(path: &str) -> Option<Uuid> {
    let parts: Vec<&str> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "sessions" {
            if let Some(next) = parts.get(i + 1) {
                return next.parse::<Uuid>().ok();
            }
        }
    }
    None
}
