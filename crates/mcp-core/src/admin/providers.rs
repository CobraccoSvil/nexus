//! Endpoint admin per la gestione dei provider LLM data-driven dal registry.
//!
//! Fonte unica (regola G/L): la dashboard NON hardcoda piu' l'elenco dei provider
//! ne' i link billing. `list_provider_registry` espone `nexus_provider_registry`
//! (mig 0565+) come contratto per la UI; `list_provider_models` /
//! `set_model_enabled` gestiscono l'abilitazione dei singoli modelli del catalog
//! (i provider onboardati opt-in, es. Groq/OpenRouter/Perplexity, hanno i modelli
//! seedati `is_enabled=false`: senza questo controllo resterebbero inerti).
//!
//! Sicurezza (regola F): nessun segreto transita da qui. Le API key restano in
//! `settings` (mascherate da `list_settings`) e si scrivono via
//! `PUT /api/admin/setting/:key`. Questo modulo espone solo metadati di
//! configurazione (name, activation, base_url, billing_url) e i flag del catalog.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::models::ModelCatalogEntry;
use crate::AppState;

/// Riga del registry provider esposta alla dashboard admin. SOLO metadati di
/// configurazione: nessun segreto (le key restano in settings).
#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRegistryEntry {
    pub name: String,
    pub api_format: String,
    /// Setting da cui il gateway legge la API key (NULL per provider senza key,
    /// es. vllm): la UI lo usa per associare la card della key al provider.
    pub key_setting: Option<String>,
    /// Setting del flag `*_enabled` (NULL = nessun toggle dedicato).
    pub enabled_setting: Option<String>,
    pub base_url_setting: Option<String>,
    pub base_url_default: Option<String>,
    /// Criterio di attivazione: `api_key` | `base_url` | `api_key_or_vertex`.
    pub activation: String,
    pub supports_tools: bool,
    pub is_active: bool,
    pub sort_order: i32,
    /// URL della console billing/keys del provider (mig 0570). NULL = self-host.
    pub billing_url: Option<String>,
}

/// GET /api/admin/provider-registry
///
/// Fonte unica data-driven per la dashboard: elenco dei provider del registry,
/// criterio di attivazione, base_url e link billing. Nessun segreto.
pub async fn list_provider_registry(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let rows: Vec<ProviderRegistryEntry> = sqlx::query_as(
        r#"SELECT name, api_format, key_setting, enabled_setting, base_url_setting,
                  base_url_default, activation, supports_tools, is_active,
                  sort_order, billing_url
           FROM nexus_provider_registry
           WHERE is_active = TRUE
           ORDER BY sort_order, name"#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "providers": rows })))
}

#[derive(Debug, Deserialize)]
pub struct ProviderModelsQuery {
    pub provider: Option<String>,
}

/// GET /api/admin/provider-models[?provider=X]
///
/// Come `/api/models` ma INCLUDE i modelli disabilitati (`is_enabled=false`): la
/// dashboard admin deve poterli vedere per abilitarli. I disabilitati sono
/// ordinati per ultimi (abilitati in cima). Riusa `ModelCatalogEntry`.
pub async fn list_provider_models(
    State(state): State<AppState>,
    Query(q): Query<ProviderModelsQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let result: Result<Vec<ModelCatalogEntry>, _> = if let Some(ref provider) = q.provider {
        sqlx::query_as(
            r#"SELECT provider, model, display_name,
                      input_cost_per_million_tokens::float8 AS input_cost_per_million_tokens,
                      output_cost_per_million_tokens::float8 AS output_cost_per_million_tokens,
                      currency, performance_tier, speed_tier,
                      capabilities, context_window, supports_tool_use, batch_discount_pct,
                      is_featured, is_enabled
               FROM ai_price_catalog
               WHERE provider = $1
               ORDER BY is_enabled DESC, is_featured DESC, input_cost_per_million_tokens ASC"#,
        )
        .bind(provider)
        .fetch_all(&state.db)
        .await
    } else {
        sqlx::query_as(
            r#"SELECT provider, model, display_name,
                      input_cost_per_million_tokens::float8 AS input_cost_per_million_tokens,
                      output_cost_per_million_tokens::float8 AS output_cost_per_million_tokens,
                      currency, performance_tier, speed_tier,
                      capabilities, context_window, supports_tool_use, batch_discount_pct,
                      is_featured, is_enabled
               FROM ai_price_catalog
               ORDER BY provider, is_enabled DESC, is_featured DESC, input_cost_per_million_tokens ASC"#,
        )
        .fetch_all(&state.db)
        .await
    };
    let rows = result.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "models": rows })))
}

#[derive(Debug, Deserialize)]
pub struct SetModelEnabledRequest {
    pub provider: String,
    pub model: String,
    pub enabled: bool,
}

/// PUT /api/admin/provider-models/enabled
///
/// Abilita/disabilita un modello del catalog (`ai_price_catalog.is_enabled`).
/// E' il controllo che rende usabile un provider onboardato opt-in: i suoi
/// modelli sono seedati `is_enabled=false` (l'health probe e il routing dinamico
/// leggono `is_enabled`). Regola H: modifica del flag via endpoint tracciato,
/// non `UPDATE` SQL a mano.
pub async fn set_model_enabled(
    State(state): State<AppState>,
    Json(req): Json<SetModelEnabledRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let res = sqlx::query(
        "UPDATE ai_price_catalog SET is_enabled = $3 WHERE provider = $1 AND model = $2",
    )
    .bind(&req.provider)
    .bind(&req.model)
    .bind(req.enabled)
    .execute(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    if res.rows_affected() == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!(
                    "modello non trovato nel catalog: {}/{}",
                    req.provider, req.model
                )
            })),
        ));
    }

    Ok(Json(json!({
        "ok": true,
        "provider": req.provider,
        "model": req.model,
        "enabled": req.enabled,
    })))
}
