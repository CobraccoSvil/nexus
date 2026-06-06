//! Client del routing per i crate FUORI dal processo mcp-core (admin-service,
//! worker di nexus-orchestrator). PUNTO UNICO di accesso al routing per tier
//! da un altro processo: consuma l'endpoint interno
//! `GET /api/internal/routing/purpose?purpose=...` di mcp-core, dove vive la
//! decisione (resolve_purpose_model, tier-only). Niente re-implementazione della
//! logica di routing qui (regola L) ne' lettura del (provider, model_id) statico.
//!
//! L'URL di mcp-core viene da `settings.mcp_core_url` (DB, regola G/I): nessun
//! hardcode. Se la setting manca o l'endpoint risponde 404/503, si propaga un
//! errore esplicito (regola H: niente fallback silenzioso).

use serde::Deserialize;
use sqlx::PgPool;

#[derive(Debug, Deserialize)]
struct PurposeResolveResponse {
    provider: String,
    model: String,
    #[serde(default)]
    no_capable_provider: bool,
}

/// Risolve `(provider, model)` per un purpose interrogando il routing tier-only
/// di mcp-core via HTTP. `Err(messaggio)` se l'URL non e' configurato, l'endpoint
/// non risponde, il purpose non e' risolvibile (404) o non c'e' modello capable
/// per il tier (503).
pub async fn resolve_purpose_via_http(
    db: &PgPool,
    purpose: &str,
) -> Result<(String, String), String> {
    let base = nexus_auth::get_setting(db, "mcp_core_url")
        .await
        .ok_or_else(|| {
            "settings.mcp_core_url non configurato: impossibile raggiungere il routing di mcp-core"
                .to_string()
        })?;
    let base = base.trim_end_matches('/');
    let url = format!("{base}/api/internal/routing/purpose?purpose={purpose}");

    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("routing mcp-core irraggiungibile ({purpose}): {e}"))?;

    let status = resp.status();
    if status.as_u16() == 404 {
        return Err(format!(
            "purpose '{purpose}' non configurato o privo di tier in nexus_purpose_model"
        ));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "routing mcp-core ha risposto {status} per '{purpose}': {body}"
        ));
    }

    let parsed: PurposeResolveResponse = resp
        .json()
        .await
        .map_err(|e| format!("risposta routing non valida per '{purpose}': {e}"))?;

    if parsed.no_capable_provider {
        return Err(format!(
            "nessun modello capable per il tier del purpose '{purpose}' (capability mancante o cooldown)"
        ));
    }
    Ok((parsed.provider, parsed.model))
}
