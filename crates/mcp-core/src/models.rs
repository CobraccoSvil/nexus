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
            r#"SELECT provider, model, display_name,
               input_cost_per_million_tokens::float8 AS input_cost_per_million_tokens,
               output_cost_per_million_tokens::float8 AS output_cost_per_million_tokens,
               currency, performance_tier, speed_tier,
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
            r#"SELECT provider, model, display_name,
               input_cost_per_million_tokens::float8 AS input_cost_per_million_tokens,
               output_cost_per_million_tokens::float8 AS output_cost_per_million_tokens,
               currency, performance_tier, speed_tier,
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

    // Fix M53-auto: post-sync auto-promotion. Identifica per ogni "famiglia"
    // di modelli (es. gpt-5-mini, claude-opus-4-7, mistral-large-3) il piu
    // recente nel catalog, lo abilita e lo propaga in nexus_routing_matrix
    // + nexus_provider_default_model. Idempotente: se la famiglia e gia al
    // top, no-op.
    if let Err(e) = auto_upgrade_models_and_routing(db).await {
        tracing::warn!("auto_upgrade_models_and_routing fallito: {e}");
    }

    Ok((added, updated, skipped))
}

/// Fix M53-auto: regole "famiglia modello" per auto-promotion. La regex
/// matcha tutti i modelli appartenenti alla stessa categoria funzionale
/// (es. mini, nano, opus, ecc.). L'ordinamento semantico individua il piu
/// recente: vengono confrontate le parti numeriche separate da "-" o ".".
const FAMILY_RULES: &[(&str /* provider */, &str /* regex */, &str /* label */)] = &[
    // OpenAI
    ("openai",    r"^gpt-\d+(\.\d+)?$",                       "gpt-frontier"),
    ("openai",    r"^gpt-\d+(\.\d+)?-pro$",                   "gpt-pro"),
    ("openai",    r"^gpt-\d+(\.\d+)?-mini$",                  "gpt-mini"),
    ("openai",    r"^gpt-\d+(\.\d+)?-nano$",                  "gpt-nano"),
    ("openai",    r"^gpt-\d+(\.\d+)?-codex$",                 "gpt-codex"),
    ("openai",    r"^gpt-\d+(\.\d+)?-codex-mini$",            "gpt-codex-mini"),
    // Anthropic
    ("anthropic", r"^claude-opus-\d+-\d+$",                   "claude-opus"),
    ("anthropic", r"^claude-sonnet-\d+-\d+$",                 "claude-sonnet"),
    ("anthropic", r"^claude-haiku-\d+-\d+$",                  "claude-haiku"),
    // Google
    ("google",    r"^gemini-\d+(\.\d+)?-flash$",              "gemini-flash"),
    ("google",    r"^gemini-\d+(\.\d+)?-flash-lite$",         "gemini-flash-lite"),
    ("google",    r"^gemini-\d+(\.\d+)?-pro$",                "gemini-pro"),
    // Mistral: matcha sia il formato data abbreviata (large-2411) sia
    // la nuova nomenclatura semantica (large-3). parse_version skippa
    // i YYMM date, quindi "large-3" [3] vince su "large-2411" [].
    ("mistral",   r"^mistral-large-\d+$",                     "mistral-large"),
    ("mistral",   r"^mistral-medium-\d+(-\d+-\d+)?$",         "mistral-medium"),
    ("mistral",   r"^mistral-small-\d+(-\d+-\d+)?$",          "mistral-small"),
    ("mistral",   r"^magistral-medium-\d+(-\d+-\d+)?$",       "magistral-medium"),
    ("mistral",   r"^codestral-\d+$",                         "codestral"),
    ("mistral",   r"^devstral-medium-\d+$",                   "devstral-medium"),
    // DeepSeek
    ("deepseek",  r"^deepseek-v\d+(\.\d+)?$",                 "deepseek-v"),
];

/// Parsa una versione embedded in un nome modello in una tupla di interi.
/// Skippa i suffissi data (YYYYMMDD, 8 cifre) per evitare di confonderli
/// con sub-versioni semantiche. Es:
/// - "gpt-5.4-mini"               -> [5, 4]
/// - "claude-opus-4-7"            -> [4, 7]
/// - "claude-opus-4-7-20260416"   -> [4, 7]   (skip 20260416)
/// - "claude-opus-4-20250514"     -> [4]      (skip 20250514)
/// - "mistral-large-2411"         -> [2411]   (4 cifre, non skippato — e "data abbreviata" mistral pattern)
/// - "deepseek-v3.2"              -> [3, 2]
fn parse_version(name: &str) -> Vec<i64> {
    // Numeri da skippare come "date abbreviate o complete":
    // - 8 cifre = YYYYMMDD (es. 20260416)
    // - 6 cifre = YYYYMM   (es. 202604)
    // - 4 cifre = YYMM     (formato Mistral: es. 2411 = nov 2024)
    fn looks_like_date(s: &str, n: i64) -> bool {
        match s.len() {
            8 => (20200101..=20351231).contains(&n),
            6 => (202001..=203512).contains(&n),
            4 => {
                // YYMM: anno 24-35, mese 01-12. Es. 2411 = nov 2024.
                let yy = n / 100;
                let mm = n % 100;
                (24..=35).contains(&yy) && (1..=12).contains(&mm)
            }
            _ => false,
        }
    }
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in name.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else {
            if !cur.is_empty() {
                if let Ok(n) = cur.parse::<i64>() {
                    if !looks_like_date(&cur, n) {
                        nums.push(n);
                    }
                }
                cur.clear();
            }
        }
    }
    if !cur.is_empty() {
        if let Ok(n) = cur.parse::<i64>() {
            if !looks_like_date(&cur, n) {
                nums.push(n);
            }
        }
    }
    nums
}

/// Auto-promotion modelli: per ogni famiglia trova il modello piu recente,
/// lo abilita in ai_price_catalog, e aggiorna nexus_routing_matrix +
/// nexus_provider_default_model dove esiste un modello "vecchio" della stessa
/// famiglia. Idempotente.
pub async fn auto_upgrade_models_and_routing(db: &sqlx::PgPool) -> Result<(), String> {
    use regex::Regex;
    let mut promotions: Vec<(String, String, String)> = Vec::new(); // (provider, family, top_model)
    let mut enabled_count = 0_usize;

    for (provider, pattern, family_label) in FAMILY_RULES {
        let re = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("auto_upgrade: regex {pattern} invalida: {e}");
                continue;
            }
        };
        // Carica tutti i modelli del provider che matchano la famiglia.
        // IMPORTANTE (chiusura residuo probe attivo): escludiamo i modelli con
        // `auto_disabled_reason` esplicito (es. `failed_initial_probe:model_not_found`
        // settato dal probe-on-insert in `model_catalog_sync.rs`, oppure
        // `model_not_found`/`hollow_completion` dal `model_health_probe` worker,
        // oppure `manual:*` impostato dall'admin). Senza questo filtro, la
        // riabilitazione di massa alla riga sottostante annulla qualsiasi
        // decisione del probe e i modelli "fantasma" tornerebbero enabled +
        // verrebbero promossi come "top family" -> default_provider_model.
        let candidates: Vec<String> = sqlx::query_scalar(
            "SELECT model FROM ai_price_catalog \
             WHERE provider = $1 \
               AND (auto_disabled_reason IS NULL OR auto_disabled_reason = '')",
        )
        .bind(provider)
        .fetch_all(db)
        .await
        .map_err(|e| format!("query candidates: {e}"))?
        .into_iter()
        .filter(|m: &String| re.is_match(m))
        .collect();
        if candidates.is_empty() {
            continue;
        }
        // Ordina semanticamente per parse_version desc, con tiebreaker
        // sulla lunghezza: a parita di versione preferisco il nome piu
        // corto (alias non-dated, es. claude-opus-4-7 < claude-opus-4-7-20260416).
        let mut sorted = candidates.clone();
        sorted.sort_by(|a, b| {
            let va = parse_version(a);
            let vb = parse_version(b);
            vb.cmp(&va).then(a.len().cmp(&b.len()))
        });
        let top = sorted[0].clone();
        promotions.push((provider.to_string(), family_label.to_string(), top.clone()));

        // Abilita TUTTI i modelli della famiglia (utente vuole scelta)
        for m in &candidates {
            let res = sqlx::query(
                "UPDATE ai_price_catalog SET is_enabled = true \
                 WHERE provider = $1 AND model = $2 AND is_enabled = false",
            )
            .bind(provider)
            .bind(m)
            .execute(db)
            .await;
            if let Ok(r) = res {
                if r.rows_affected() > 0 {
                    enabled_count += 1;
                }
            }
        }

        // Aggiorna routing_matrix: per ogni record con stesso provider e
        // model "vecchio" della famiglia (model che matcha la regex ma e
        // diverso dal top), sostituisci con top.
        let to_replace: Vec<String> = candidates
            .iter()
            .filter(|m| **m != top)
            .cloned()
            .collect();
        if !to_replace.is_empty() {
            let res = sqlx::query(
                "UPDATE nexus_routing_matrix \
                 SET model_id = $1, updated_at = NOW() \
                 WHERE provider = $2 AND model_id = ANY($3)",
            )
            .bind(&top)
            .bind(provider)
            .bind(&to_replace)
            .execute(db)
            .await;
            if let Ok(r) = res {
                if r.rows_affected() > 0 {
                    tracing::info!(
                        "auto_upgrade: routing_matrix [{}/{}] {} record -> {}",
                        provider, family_label, r.rows_affected(), top
                    );
                }
            }
            // Idem per default_model_for_provider (se il default e vecchio della stessa famiglia)
            let _ = sqlx::query(
                "UPDATE nexus_provider_default_model \
                 SET model_id = $1, updated_at = NOW(), notes = COALESCE(notes,'') || ' | auto-upgrade ' || $2 \
                 WHERE provider = $3 AND model_id = ANY($4)",
            )
            .bind(&top)
            .bind(family_label.to_string())
            .bind(provider)
            .bind(&to_replace)
            .execute(db)
            .await;
        }
    }

    tracing::info!(
        "auto_upgrade_models_and_routing: enabled={} promotions={}",
        enabled_count, promotions.len()
    );
    for (p, fam, top) in &promotions {
        tracing::debug!("  {} / {} -> top = {}", p, fam, top);
    }

    Ok(())
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

/// POST /api/admin/auto-upgrade-models
/// Trigger manuale per la promotion auto (utile per testare senza aspettare cron).
pub async fn auto_upgrade_models_endpoint(State(state): State<AppState>) -> Json<Value> {
    match auto_upgrade_models_and_routing(&state.db).await {
        Ok(()) => Json(json!({ "ok": true })),
        Err(e) => Json(json!({ "ok": false, "error": e })),
    }
}

/// POST /api/admin/probe-models
/// Esegue un round one-shot del model_health_probe: pinga ogni modello
/// enabled, applica counter / auto-disable. Usa la soglia configurata in
/// settings.model_health_probe_failure_threshold (default 3).
pub async fn probe_models_now(State(state): State<AppState>) -> Json<Value> {
    let threshold = crate::settings::get_setting(&state.db, "model_health_probe_failure_threshold")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i32>().ok())
        .unwrap_or(3);
    let orchestrator = std::sync::Arc::new(state.orchestrator.clone());
    let stats = crate::model_health_probe::run_one_round(&orchestrator, &state.db, threshold).await;
    Json(json!({
        "ok": true,
        "total": stats.total,
        "healthy": stats.healthy,
        "provider_wide_errors": stats.provider_wide_errors,
        "model_errors": stats.model_errors,
        "auto_disabled": stats.auto_disabled,
        "skipped_provider_cooldown": stats.skipped_provider_cooldown,
        "failure_threshold": threshold,
    }))
}

