//! Endpoint admin per il browser-bridge-mcp.
//!
//! Proxy verso il daemon (default 127.0.0.1:4055) per:
//!   - `GET /api/admin/browser-bridge/info`        -> JSON status + URL asset
//!   - `GET /api/admin/browser-bridge/install.ps1` -> script PowerShell di installazione
//!   - `GET /api/admin/browser-bridge/install.sh`  -> script bash di installazione
//!   - `GET /api/admin/browser-bridge/extension.crx` -> .crx scaricabile
//!
//! Razionale: dall'admin UI l'utente clicca "Installa estensione browser",
//! ottiene il link a `install.ps1` (Windows) o `install.sh` (Linux), lo lancia
//! una volta come admin e Chrome la installa silenziosamente al riavvio.
//!
//! Sicurezza: queste route stanno gia` sotto `require_admin`. Il daemon
//! e` raggiungibile solo su loopback (127.0.0.1).

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

const DEFAULT_BRIDGE_BASE: &str = "http://127.0.0.1:4055";

fn bridge_base() -> String {
    std::env::var("BROWSER_BRIDGE_URL").unwrap_or_else(|_| DEFAULT_BRIDGE_BASE.to_string())
}

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    // Gli handler non usano lo state dell'app: il router e` generico cosi`
    // si nesta sotto qualsiasi parent senza vincoli di tipo.
    Router::new()
        .route("/info", get(info))
        .route("/install.ps1", get(install_ps1))
        .route("/install.sh", get(install_sh))
        .route("/extension.crx", get(extension_crx))
}

async fn info() -> Response {
    proxy("/extension/info", "application/json").await
}

async fn install_ps1() -> Response {
    proxy("/extension/install.ps1", "text/plain; charset=utf-8").await
}

async fn install_sh() -> Response {
    proxy("/extension/install.sh", "text/x-shellscript; charset=utf-8").await
}

async fn extension_crx() -> Response {
    proxy("/extension/extension.crx", "application/x-chrome-extension").await
}

async fn proxy(path: &str, content_type: &'static str) -> Response {
    let url = format!("{}{}", bridge_base(), path);
    let client = nexus_http::build_client();
    let r = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, %url, "browser-bridge daemon non raggiungibile");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("daemon browser-bridge-mcp non raggiungibile su {}: {}", bridge_base(), e),
            )
                .into_response();
        }
    };
    let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    // Preserva content-disposition per il .crx (download diretto).
    let cd = r.headers().get(header::CONTENT_DISPOSITION).cloned();
    let bytes = match r.bytes().await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    let mut resp = Response::builder().status(status).body(Body::from(bytes)).unwrap_or_else(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "build response").into_response()
    });
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Some(cd_value) = cd {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, cd_value);
    }
    resp
}
