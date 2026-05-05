use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

use crate::{auth, AppState};

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
            tracing::warn!("require_admin: user={}, role={}, path={}", claims.sub, claims.role, req.uri());
            if claims.role != "admin" {
                tracing::warn!("require_admin: access denied - role={} is not admin", claims.role);
                return Err(StatusCode::FORBIDDEN);
            }
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(e) => {
            tracing::warn!("require_admin: token validation failed: {:?}, path={}", e, req.uri());
            Err(e)
        }
    }
}
