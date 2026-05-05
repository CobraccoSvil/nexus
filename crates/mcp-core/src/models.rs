use axum::{
    extract::{Query, State},
    Json,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use crate::AppState;

// ── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    pub provider: String,
    pub model: String,
    #[sqlx(default)]
    pub display_name: String,
    pub input_cost_per_million_tokens: f64,
    pub output_cost_per_million_tokens: f64,
    pub currency: String,
    #[sqlx(default)]
    pub performance_tier: String,
    #[sqlx(default)]
    pub speed_tier: String,
    #[sqlx(default)]
    pub capabilities: Value,
    #[sqlx(default)]
    pub context_window: i32,
    #[sqlx(default)]
    pub supports_tool_use: bool,
    #[sqlx(default)]
    pub batch_discount_pct: i32,
    #[sqlx(default)]
    pub is_featured: bool,
    pub is_enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ModelsQuery {
    pub provider: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RoutingPreviewQuery {
    pub mode: Option<String>,
}

// La matrice di routing e' stata spostata in DB (tabella nexus_routing_matrix,
// migrazione 0101). Vedi crate::routing_matrix per il loader con cache 60s.

// ── Handlers ──────────────────────────────────────────────────────────────

/// GET /api/models[?provider=xxx]
pub async fn list_models(
    State(state): State<AppState>,
    Query(params): Query<ModelsQuery>,
) -> Json<Value> {
    let result: Result<Vec<ModelCatalogEntry>, _> = if let Some(ref provider) = params.provider {
        sqlx::query_as(
            r#"SELECT provider, model, display_name, input_cost_per_million_tokens,
               output_cost_per_million_tokens, currency, performance_tier, speed_tier,
               capabilities, context_window, supports_tool_use, batch_discount_pct,
               is_featured, is_enabled
               FROM ai_price_catalog
               WHERE provider = $1 AND is_enabled = TRUE
               ORDER BY is_featured DESC, input_cost_per_million_tokens ASC"#,
        )
        .bind(provider)
        .fetch_all(&state.db)
        .await
    } else {
        sqlx::query_as(
            r#"SELECT provider, model, display_name, input_cost_per_million_tokens,
               output_cost_per_million_tokens, currency, performance_tier, speed_tier,
               capabilities, context_window, supports_tool_use, batch_discount_pct,
               is_featured, is_enabled
               FROM ai_price_catalog
               WHERE is_enabled = TRUE
               ORDER BY provider, is_featured DESC, input_cost_per_million_tokens ASC"#,
        )
        .fetch_all(&state.db)
        .await
    };

    match result {
        Ok(models) => Json(json!({ "models": models })),
        Err(e) => Json(json!({ "error": e.to_string(), "models": Value::Array(vec![]) })),
    }
}

/// GET /api/models/routing-preview?mode=bilanciata
pub async fn routing_preview(
    State(state): State<AppState>,
    Query(params): Query<RoutingPreviewQuery>,
) -> Json<Value> {
    let mode = params.mode.as_deref().unwrap_or("bilanciata").to_string();
    let valid_modes = ["veloce", "economica", "bilanciata", "approfondita"];
    let mode = if valid_modes.contains(&mode.as_str()) { mode } else { "bilanciata".to_string() };

    // Legge la matrice da DB (cache 60s). Se non disponibile ritorna preview vuota
    // con error: il chiamante (admin UI) mostra il messaggio.
    let matrix_arc = match state.orchestrator.routing_matrix.current_async().await {
        Ok(m) => m,
        Err(e) => {
            return Json(json!({
                "mode": mode,
                "routing": [],
                "error": format!("routing_matrix non disponibile: {e}"),
            }));
        }
    };
    let entries: Vec<(String, String, String)> = matrix_arc
        .by_intent_mode
        .iter()
        .filter(|((_, m), _)| m == &mode)
        .map(|((intent, _mode), (provider, model))| {
            (intent.clone(), provider.clone(), model.clone())
        })
        .collect();

    // Fetch prices from DB for each model in the preview
    let mut routing = Vec::new();
    let mut total_cost = 0.0f64;
    let mut count = 0usize;

    for (intent, provider, model) in &entries {
        let price_row: Option<(f64, String)> = sqlx::query_as::<_, (f64, String)>(
            "SELECT input_cost_per_million_tokens, speed_tier FROM ai_price_catalog WHERE provider = $1 AND model = $2 LIMIT 1"
        )
        .bind(provider)
        .bind(model)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);

        let (input_cost, speed) = price_row.unwrap_or((0.0, "medium".to_string()));
        total_cost += input_cost;
        count += 1;

        routing.push(json!({
            "intent": intent,
            "provider": provider,
            "model": model,
            "inputCost": input_cost,
            "speed": speed,
        }));
    }

    let avg_cost = if count > 0 { total_cost / count as f64 } else { 0.0 };

    Json(json!({
        "mode": mode,
        "estimatedAvgCostInputPerMillion": (avg_cost * 100.0).round() / 100.0,
        "routing": routing,
    }))
}

/// Esegue la sync del catalogo modelli da LiteLLM. Riusabile sia dall'handler
/// REST che da un task background schedulato.
pub async fn run_catalog_sync(db: &sqlx::PgPool) -> Result<(i32, i32, i32), String> {
    const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let resp = client.get(LITELLM_URL).send().await
        .map_err(|e| format!("fetch: {e}"))?;
    let data: Value = resp.json().await
        .map_err(|e| format!("parse: {e}"))?;
    let obj = data.as_object().ok_or_else(|| "JSON non oggetto".to_string())?;

    let provider_map: &[(&str, &str)] = &[
        ("claude-",           "anthropic"),
        ("gpt-",              "openai"),
        ("o1",                "openai"),
        ("o3",                "openai"),
        ("o4",                "openai"),
        ("gemini/",           "google"),
        ("deepseek/",         "deepseek"),
        ("mistral/",          "mistral"),
        ("codestral/",        "mistral"),
    ];

    let mut updated = 0i32;
    let mut added = 0i32;
    let mut skipped = 0i32;

    for (key, entry) in obj {
        let Some(provider) = provider_map.iter()
            .find(|(prefix, _)| key.starts_with(prefix))
            .map(|(_, p)| *p) else {
            skipped += 1; continue;
        };

        let input_cost = entry.get("input_cost_per_token").and_then(Value::as_f64).map(|c| c * 1_000_000.0).unwrap_or(0.0);
        let output_cost = entry.get("output_cost_per_token").and_then(Value::as_f64).map(|c| c * 1_000_000.0).unwrap_or(0.0);

        if input_cost == 0.0 && output_cost == 0.0 { skipped += 1; continue; }

        let model_id = if let Some(pos) = key.find('/') { &key[pos + 1..] } else { key.as_str() };

        let context_window = entry.get("max_input_tokens")
            .and_then(Value::as_i64)
            .or_else(|| entry.get("max_tokens").and_then(Value::as_i64))
            .unwrap_or(8192) as i32;

        let supports_tools = entry.get("supports_function_calling").and_then(Value::as_bool).unwrap_or(true);

        let result = sqlx::query(
            r#"INSERT INTO ai_price_catalog (
                provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens,
                currency, context_window, supports_tool_use, is_enabled, display_name
              ) VALUES ($1, $2, $3, $4, 'USD', $5, $6, FALSE, $2)
              ON CONFLICT (provider, model) DO UPDATE SET
                input_cost_per_million_tokens = EXCLUDED.input_cost_per_million_tokens,
                output_cost_per_million_tokens = EXCLUDED.output_cost_per_million_tokens,
                context_window = EXCLUDED.context_window,
                supports_tool_use = EXCLUDED.supports_tool_use,
                updated_at = NOW()
              RETURNING (xmax = 0) AS inserted"#,
        )
        .bind(provider).bind(model_id).bind(input_cost).bind(output_cost)
        .bind(context_window).bind(supports_tools)
        .fetch_one(db).await;

        match result {
            Ok(row) => {
                let inserted: bool = row.try_get("inserted").unwrap_or(false);
                if inserted { added += 1; } else { updated += 1; }
            }
            Err(_) => { skipped += 1; }
        }
    }

    let _ = sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('model_catalog_last_sync', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(db).await;

    tracing::info!("run_catalog_sync: added={} updated={} skipped={}", added, updated, skipped);
    Ok((added, updated, skipped))
}

/// POST /api/admin/sync-model-catalog
/// Scarica il JSON LiteLLM da GitHub e aggiorna i prezzi in ai_price_catalog
pub async fn sync_model_catalog(State(state): State<AppState>) -> Json<Value> {
    match run_catalog_sync(&state.db).await {
        Ok((added, updated, skipped)) => Json(json!({
            "added": added, "updated": updated, "skipped": skipped,
            "source": "LiteLLM GitHub",
        })),
        Err(e) => Json(json!({
            "error": e, "added": 0, "updated": 0, "skipped": 0,
        })),
    }
}

