//! Client HTTP minimale del Nexus LLM Gateway per i tool di `nexus-agent-tools`.
//!
//! `nexus-agent-tools` e' un crate INFERIORE a `mcp-core` nella gerarchia del
//! workspace: non puo' dipendere da `mcp-core::nexus_gateway::NexusGatewayClient`
//! (creerebbe un ciclo). Questo modulo espone un client ridotto al solo
//! sottoinsieme che serve ai tool del crate (oggi: la chiamata multimodale
//! vision), parlando lo STESSO contratto wire del gateway (`POST /v1/complete`,
//! stessi nomi di campo serde di `nexus-gateway::types`). NON re-implementa la
//! logica di routing/cooldown/privacy: quella vive nel gateway (regola L,
//! punto unico). Qui si fa solo il trasporto HTTP + il pin del provider deciso
//! a monte via routing matrix DB (regola G: nessun modello hardcoded).
//!
//! URL e token sono risolti dal DB/env senza panicare (a differenza di
//! `nexus_auth::resolve_port`, pensato per l'avvio): un purpose non configurato
//! o il gateway giu' devono degradare a errore restituito al modello, non far
//! crashare il processo.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;

use nexus_auth::get_setting_checked;

/// Porta di default del gateway, coerente con `settings.nexus_gateway_port`
/// (mig 0239). Usata SOLO come rete di sicurezza se la lettura del setting
/// fallisce: documentata, non un "magic fallback" di modello (regola G).
const GATEWAY_DEFAULT_PORT: u16 = 4060;
/// Timeout HTTP della chiamata al gateway. Vision puo' essere lento (immagini
/// grandi, cold start del provider).
const GATEWAY_HTTP_TIMEOUT_SECS: u64 = 60;

/// Metadati di tracciamento/tenancy della richiesta (`RequestMetadata` del
/// gateway). I tool interni valorizzano solo `feature`; il resto va a default
/// (stringhe vuote, tier 0), come gli altri call site interni di mcp-core
/// (es. `intent_classifier`).
#[derive(Serialize, Default)]
struct GwMetadata {
    tenant_id: String,
    user_id: String,
    request_id: String,
    sensitivity_tier: u8,
    feature: String,
}

/// Corpo di `POST /v1/complete` (sottoinsieme usato dai tool del crate).
#[derive(Serialize)]
struct GwRequestBody {
    model: String,
    /// Messaggi della conversazione. `content` e' un [`Value`] perche' il
    /// contratto del gateway (`MessageContent` untagged) accetta sia una
    /// stringa sia una lista di blocchi `{type, ...}` (text/image_url).
    messages: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// Pin esplicito del provider deciso a monte (routing matrix DB): il
    /// gateway esegue ESATTAMENTE quel provider senza secondo routing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pin_provider: Option<String>,
    metadata: GwMetadata,
}

/// Risposta di `POST /v1/complete` (solo i campi consumati dai tool del crate).
/// Deserializzazione tollerante: i campi extra del contratto sono ignorati.
#[derive(Deserialize)]
struct GwResponseBody {
    #[serde(default)]
    content: String,
    #[serde(default)]
    model_used: String,
    #[serde(default)]
    provider_used: String,
}

/// Esito di una chiamata multimodale al gateway: testo grezzo + modello usato.
pub struct GwVisionResult {
    /// Testo grezzo della risposta del modello (da parsare dal chiamante).
    pub content: String,
    /// Etichetta `provider/model` realmente eseguita dal gateway, per
    /// trasparenza verso il modello agente.
    pub model_used: String,
}

/// Esegue una chiamata multimodale (testo + immagini) al gateway pinnando il
/// provider deciso a monte. `Err(messaggio)` se URL/token non risolvibili, il
/// gateway e' irraggiungibile o risponde con errore: il chiamante (tool vision)
/// rigira l'errore al modello, che ricade su un altro tool (fallback onesto).
///
/// - `provider`/`model`: risolti dal purpose via routing (regola G).
/// - `content_blocks`: lista di blocchi `{type:"text"|"image_url", ...}` gia'
///   costruita dal chiamante (data URI base64 per le immagini).
/// - `feature`: etichetta di tracciamento (es. nome del purpose).
pub async fn gateway_vision_complete(
    db: &PgPool,
    provider: &str,
    model: &str,
    content_blocks: Value,
    max_tokens: u32,
    feature: &str,
) -> Result<GwVisionResult, String> {
    let base_url = resolve_gateway_url(db).await;
    let token = resolve_gateway_token();

    let body = GwRequestBody {
        // Il gateway accetta "provider/model" come pin esplicito; valorizziamo
        // anche `pin_provider` per evitare un secondo routing divergente.
        model: format!("{provider}/{model}"),
        messages: json!([
            {
                "role": "user",
                "content": content_blocks,
            }
        ]),
        max_tokens: Some(max_tokens),
        pin_provider: Some(provider.to_string()),
        metadata: GwMetadata {
            feature: feature.to_string(),
            ..Default::default()
        },
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GATEWAY_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("impossibile costruire client HTTP gateway: {e}"))?;

    let resp = client
        .post(format!("{base_url}/v1/complete"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            format!(
                "gateway LLM irraggiungibile ({base_url}): {e}. \
                 Verifica che il nexus-gateway sia attivo."
            )
        })?;

    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "gateway LLM ha risposto HTTP {} ({provider}/{model}): {detail}",
            status.as_u16()
        ));
    }

    let parsed: GwResponseBody = resp
        .json()
        .await
        .map_err(|e| format!("risposta gateway non valida: {e}"))?;

    let model_used = if parsed.model_used.is_empty() {
        format!("{provider}/{model}")
    } else if parsed.provider_used.is_empty() {
        parsed.model_used
    } else {
        format!("{}/{}", parsed.provider_used, parsed.model_used)
    };

    Ok(GwVisionResult {
        content: parsed.content,
        model_used,
    })
}

/// Risolve l'URL del gateway da `settings.nexus_gateway_port` (mig 0239) in modo
/// TOLLERANTE (no panic): se la lettura fallisce ricade sulla porta di default
/// documentata. L'override di emergenza `NEXUS_GATEWAY_PORT` (stesso usato
/// dall'avvio) e' rispettato.
async fn resolve_gateway_url(db: &PgPool) -> String {
    if let Ok(port) = std::env::var("NEXUS_GATEWAY_PORT") {
        if let Ok(p) = port.trim().parse::<u16>() {
            if p > 0 {
                return format!("http://127.0.0.1:{p}");
            }
        }
    }
    let port = match get_setting_checked(db, "nexus_gateway_port").await {
        Ok(Some(raw)) => raw.trim().parse::<u16>().ok().filter(|p| *p > 0),
        _ => None,
    };
    let port = port.unwrap_or_else(|| {
        tracing::warn!(
            "gateway_client: settings.nexus_gateway_port non leggibile, uso default {}",
            GATEWAY_DEFAULT_PORT
        );
        GATEWAY_DEFAULT_PORT
    });
    format!("http://127.0.0.1:{port}")
}

/// Token di servizio del gateway. E' un SEGRETO: vive in env
/// `NEXUS_GATEWAY_SERVICE_TOKEN` (stessa convenzione di `main.rs`), mai nel DB.
/// Il fallback dev e' coerente con gli altri call site interni.
fn resolve_gateway_token() -> String {
    std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN").unwrap_or_else(|_| "dev-internal-token".to_string())
}
