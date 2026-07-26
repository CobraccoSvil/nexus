//! Client del Nexus Gateway per i crate FUORI dal processo mcp-core
//! (admin-service, worker di nexus-orchestrator).
//!
//! PUNTO UNICO (regola L) della completion testuale via gateway per questi
//! crate: prima ogni call site costruiva a mano il POST `/v1/complete` (e prima
//! ancora un POST al brain Python, eliminato). La logica di routing NON vive
//! qui: il `(provider, model)` arriva gia' risolto per tier dal chiamante, via
//! [`crate::routing_client::resolve_purpose_via_http`].
//!
//! L'URL del gateway viene dal DB (`settings.nexus_gateway_port`, regola G:
//! nessuna porta hardcoded). Dentro mcp-core esiste gia' il client in-process
//! (`nexus_gateway::NexusGatewayClient`): questo modulo e' per chi non ce l'ha.

use sqlx::PgPool;

/// Corpo della richiesta `/v1/complete`. `pin_provider` esegue ESATTAMENTE il
/// provider gia' risolto a monte: senza, il gateway rifarebbe un routing suo e
/// potrebbe divergere dalla decisione per tier del chiamante (regola G).
fn complete_body(
    provider: &str,
    model: &str,
    prompt: &str,
    feature: &str,
    max_tokens: Option<u32>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "pin_provider": provider,
        "metadata": {
            "tenant_id": "nexus",
            "user_id": feature,
            "request_id": uuid::Uuid::new_v4().to_string(),
            "feature": feature,
        },
    });
    if let Some(mt) = max_tokens {
        body["max_tokens"] = serde_json::json!(mt);
    }
    body
}

/// Esegue una completion testuale via Nexus Gateway (`POST /v1/complete`).
///
/// - `provider`/`model`: gia' risolti a monte (tier-only). Il `provider` diventa
///   `pin_provider` per eseguire ESATTAMENTE quel modello, senza un secondo
///   routing divergente (regola G).
/// - `feature`: etichetta di tracciamento nel ledger (es. il nome del purpose).
/// - `max_tokens`: `None` lascia decidere il gateway.
///
/// Ritorna il testo generato (`LlmResponse.content`). L'errore e' un messaggio
/// leggibile: il chiamante decide se loggarlo o rigirarlo.
pub async fn gateway_text_complete(
    db: &PgPool,
    provider: &str,
    model: &str,
    prompt: &str,
    feature: &str,
    max_tokens: Option<u32>,
) -> Result<String, String> {
    let gw_port = nexus_auth::resolve_port(db, "nexus_gateway_port").await;
    let gw_url = format!("http://127.0.0.1:{gw_port}");
    let body = complete_body(provider, model, prompt, feature, max_tokens);

    // Bearer di servizio: JWT a vita breve firmato con la chiave di piattaforma
    // (punto unico `nexus_auth::service_bearer`). Prima era un valore statico
    // con fallback hardcoded nel sorgente, che valeva come bypass dell'auth.
    let bearer = nexus_auth::service_bearer(db)
        .await
        .map_err(|e| format!("bearer di servizio non disponibile: {e}"))?;

    let resp = reqwest::Client::new()
        .post(format!("{gw_url}/v1/complete"))
        .header("Authorization", format!("Bearer {bearer}"))
        .json(&body)
        .timeout(std::time::Duration::from_secs(GATEWAY_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| format!("Nexus Gateway irraggiungibile ({gw_url}): {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Nexus Gateway ha risposto HTTP {} ({provider}/{model}): {detail}",
            status.as_u16()
        ));
    }

    let parsed: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("risposta del gateway non e' JSON valido: {e}"))?;

    Ok(parsed["content"].as_str().unwrap_or_default().to_string())
}

/// Budget della chiamata. Generoso: qui passano revisioni di prompt e
/// valutazioni, non completamenti interattivi.
const GATEWAY_TIMEOUT_SECS: u64 = 120;
