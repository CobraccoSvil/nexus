//! Admin API: gestione template prompt con cache TTL locale.
//!
//! La cache restituisce `None` per chiavi non caricate o scadute.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::AppState;

#[derive(Clone, Debug)]
pub struct TemplateCache {
    inner: Arc<DashMap<String, (String, Instant)>>,
    ttl: Duration,
}

impl TemplateCache {
    /// Crea una nuova cache con TTL di 60 secondi.
    ///
    /// # Esempi
    ///
    /// ```
    /// use admin_service::prompt_templates::TemplateCache;
    ///
    /// let cache = TemplateCache::new();
    /// // Chiave assente restituisce None
    /// assert!(cache.get("missing").is_none());
    /// ```
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            ttl: Duration::from_secs(60),
        }
    }
    pub fn get(&self, key: &str) -> Option<String> {
        self.inner
            .get(key)
            .and_then(|e| if e.1.elapsed() < self.ttl { Some(e.0.clone()) } else { None })
    }
    pub fn set(&self, key: String, value: String) {
        self.inner.insert(key, (value, Instant::now()));
    }
    pub fn invalidate(&self, key: &str) {
        self.inner.remove(key);
    }
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow, Clone)]
pub struct PromptTemplate {
    pub id: i32,
    pub key: String,
    pub category: String,
    pub title: String,
    pub content: String,
    pub is_active: bool,
    pub version: i32,
    pub updated_by: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub usage_context: Option<String>,
    /// Schema del prompt: 'plain' (legacy) | 'xml' (v2). Default 'plain'.
    #[serde(default = "default_schema_type")]
    pub schema_type: String,
    /// Placeholder dichiarati che il prompt usa, es. ["lang_hint","type_hint"].
    /// Default: array vuoto.
    #[serde(default)]
    pub placeholder_vars: serde_json::Value,
    /// Variante sperimentale (canary) generata dal PromptOptimizerWorker.
    /// Default: false.
    #[serde(default)]
    pub experimental: bool,
}

fn default_schema_type() -> String {
    "plain".to_string()
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct PromptTemplateHistory {
    pub id: i32,
    pub template_id: i32,
    pub content: String,
    pub version: i32,
    pub changed_by: String,
    pub changed_at: chrono::DateTime<chrono::Utc>,
    pub change_note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertTemplateReq {
    pub title: Option<String>,
    pub content: String,
    pub updated_by: Option<String>,
    pub change_note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AiSuggestReq {
    pub instruction: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FalsePositiveReq {
    pub reason: Option<String>,
    pub rule_key: Option<String>,
    #[allow(dead_code)]
    pub code_snippet: Option<String>,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub struct FalsePositiveStat {
    pub rule_key: Option<String>,
    pub count: Option<i64>,
}

#[allow(dead_code)]
pub async fn get_template_or_default(db: &PgPool, cache: &TemplateCache, key: &str) -> String {
    if let Some(cached) = cache.get(key) {
        return cached;
    }
    let result = sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = TRUE",
    )
    .bind(key)
    .fetch_optional(db)
    .await;

    match result {
        Ok(Some(content)) => {
            cache.set(key.to_string(), content.clone());
            content
        }
        Ok(None) => {
            tracing::error!("PROMPT TEMPLATE MANCANTE: key='{}'", key);
            String::new()
        }
        Err(e) => {
            tracing::error!("Errore lettura prompt template '{}': {}", key, e);
            String::new()
        }
    }
}

pub async fn list_templates_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<PromptTemplate>>, StatusCode> {
    let templates = sqlx::query_as::<_, PromptTemplate>(
        "SELECT id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context, schema_type, placeholder_vars, experimental FROM nexus_prompt_templates ORDER BY category, key",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(templates))
}

pub async fn get_template_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let template = sqlx::query_as::<_, PromptTemplate>(
        "SELECT id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context, schema_type, placeholder_vars, experimental FROM nexus_prompt_templates WHERE key = $1",
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let history = sqlx::query_as::<_, PromptTemplateHistory>(
        "SELECT id, template_id, content, version, changed_by, changed_at, change_note FROM nexus_prompt_template_history WHERE template_id = $1 ORDER BY version DESC LIMIT 20",
    )
    .bind(template.id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    Ok(Json(serde_json::json!({ "template": template, "history": history })))
}

pub async fn upsert_template_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<UpsertTemplateReq>,
) -> Result<Json<PromptTemplate>, StatusCode> {
    let updated_by = req.updated_by.unwrap_or_else(|| "user".to_string());

    // Check if template exists
    let current: Option<(i32, i32)> = sqlx::query_as(
        "SELECT id, version FROM nexus_prompt_templates WHERE key = $1",
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let template = if let Some((cur_id, _cur_version)) = current {
        // Save history
        let _ = sqlx::query(
            "INSERT INTO nexus_prompt_template_history (template_id, content, version, changed_by, change_note) SELECT id, content, version, $2, $3 FROM nexus_prompt_templates WHERE id = $1",
        )
        .bind(cur_id)
        .bind(&updated_by)
        .bind(&req.change_note)
        .execute(&state.db)
        .await;

        // Update
        sqlx::query_as::<_, PromptTemplate>(
            "UPDATE nexus_prompt_templates SET content=$1, version=version+1, updated_by=$2, updated_at=NOW(), title=COALESCE($3, title) WHERE key=$4 RETURNING id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context, schema_type, placeholder_vars, experimental",
        )
        .bind(&req.content)
        .bind(&updated_by)
        .bind(&req.title)
        .bind(&key)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        sqlx::query_as::<_, PromptTemplate>(
            "INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES ($1, 'system', $2, $3, $4) RETURNING id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context, schema_type, placeholder_vars, experimental",
        )
        .bind(&key)
        .bind(req.title.unwrap_or_else(|| key.clone()))
        .bind(&req.content)
        .bind(&updated_by)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    state.template_cache.invalidate(&key);
    Ok(Json(template))
}

// ── Preview rendering ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct PreviewReq {
    /// Intent semantico da usare per risolvere `{{type_hint}}` (es. "bug_fix").
    /// Default "chat".
    #[serde(default)]
    pub intent: Option<String>,
    /// Linguaggio dominante del repo (es. "TypeScript"). Risolve `{{lang_hint}}`.
    #[serde(default)]
    pub repo_lang: Option<String>,
    /// Sintesi del repo per `{{repo_summary}}`.
    #[serde(default)]
    pub repo_summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PreviewResp {
    pub key: String,
    pub schema_type: String,
    pub rendered: String,
    /// Placeholder presenti nel template ma non risolti (per debug UI).
    pub unresolved_placeholders: Vec<String>,
}

/// Restituisce l'anteprima del prompt con i placeholder risolti.
///
/// La logica e' una porta della funzione Python `brain/agents/prompt_renderer.py`:
/// stessa mappa intent->type_hint, stesso default per repo_summary, stesso
/// formato per `{{lang_hint}}`. La duplicazione e' deliberata perche' la UI
/// admin non deve dipendere dal brain Python (che potrebbe essere down).
pub async fn preview_template_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PreviewReq>,
) -> Result<Json<PreviewResp>, StatusCode> {
    let row: (String, String) = sqlx::query_as(
        "SELECT content, schema_type FROM nexus_prompt_templates WHERE key = $1",
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let (template, schema_type) = row;
    let intent = req.intent.unwrap_or_else(|| "chat".to_string());

    let lang_hint = match req.repo_lang.as_deref() {
        Some(l) if !l.trim().is_empty() => format!(", linguaggio {}", l.trim()),
        _ => String::new(),
    };
    let type_hint = match intent.as_str() {
        "code_generation" => "moduli e funzioni produzione-ready",
        "code_modification" => "modifiche chirurgiche al codice esistente",
        "bug_fix" => "fix mirata + test di regressione",
        "refactoring" => "refactor a parita' di comportamento",
        "test_generation" => "test unitari indipendenti",
        "code_review" => "report di code review strutturato",
        "documentation" => "documentazione tecnica concisa",
        "architecture" => "design architetturale e contratti",
        "performance" => "ottimizzazione misurata before/after",
        "security" => "audit di sicurezza con remediation",
        "database" => "schema, query e migrazioni idempotenti",
        "infrastructure" => "configurazione infrastrutturale riproducibile",
        "deployment" => "pipeline di deployment automatizzata",
        "chat" => "risposta concisa e accionabile",
        _ => "task generico",
    }
    .to_string();
    let repo_summary = req
        .repo_summary
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "repository utente (metadati non disponibili)".to_string());

    let mut rendered = template;
    let mut unresolved: Vec<String> = Vec::new();

    let known: [(&str, &str); 3] = [
        ("lang_hint", lang_hint.as_str()),
        ("type_hint", type_hint.as_str()),
        ("repo_summary", repo_summary.as_str()),
    ];
    for (name, value) in known.iter() {
        let needle = format!("{{{{{}}}}}", name);
        rendered = rendered.replace(&needle, value);
    }

    // Placeholder rimanenti (non noti): li sostituisce con stringa vuota
    // e li elenca nella response per UI debug.
    let re = regex::Regex::new(r"\{\{\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\}\}").unwrap();
    for cap in re.captures_iter(&rendered.clone()) {
        if let Some(m) = cap.get(1) {
            unresolved.push(m.as_str().to_string());
        }
    }
    rendered = re.replace_all(&rendered, "").to_string();

    Ok(Json(PreviewResp {
        key,
        schema_type,
        rendered,
        unresolved_placeholders: unresolved,
    }))
}

pub async fn disable_template_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<StatusCode, StatusCode> {
    sqlx::query("UPDATE nexus_prompt_templates SET is_active=FALSE, updated_at=NOW() WHERE key=$1")
        .bind(&key)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.template_cache.invalidate(&key);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn enable_template_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<StatusCode, StatusCode> {
    sqlx::query("UPDATE nexus_prompt_templates SET is_active=TRUE, updated_at=NOW() WHERE key=$1")
        .bind(&key)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.template_cache.invalidate(&key);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn mark_false_positive_handler(
    State(state): State<AppState>,
    Path(finding_id): Path<uuid::Uuid>,
    Json(req): Json<FalsePositiveReq>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    sqlx::query(
        "UPDATE project_quality_findings SET is_false_positive=TRUE, false_positive_reason=$1, false_positive_at=NOW(), false_positive_rule_key=$2 WHERE id=$3",
    )
    .bind(&req.reason)
    .bind(&req.rule_key)
    .bind(finding_id)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(rule_key) = &req.rule_key {
        let count: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM project_quality_findings WHERE false_positive_rule_key=$1 AND false_positive_at > NOW() - INTERVAL '7 days'",
        )
        .bind(rule_key)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

        if count >= 3 {
            let db = state.db.clone();
            let rk = rule_key.clone();
            tokio::spawn(async move {
                let _ = generate_nexus_suggestion(&db, &rk).await;
            });
        }
    }

    Ok(Json(serde_json::json!({"ok": true})))
}

pub async fn false_positive_stats_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let rows = sqlx::query(
        "SELECT false_positive_rule_key, COUNT(*) as cnt FROM project_quality_findings WHERE is_false_positive=TRUE AND false_positive_rule_key IS NOT NULL AND false_positive_at > NOW() - INTERVAL '7 days' GROUP BY false_positive_rule_key ORDER BY cnt DESC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stats: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let rule_key: Option<String> = row.get("false_positive_rule_key");
            let count: i64 = row.get("cnt");
            serde_json::json!({ "rule_key": rule_key, "count": count })
        })
        .collect();

    Ok(Json(serde_json::json!(stats)))
}

async fn generate_nexus_suggestion(db: &PgPool, rule_key: &str) -> anyhow::Result<()> {
    let row: Option<(i32, String)> = sqlx::query_as(
        "SELECT id, content FROM nexus_prompt_templates WHERE key=$1",
    )
    .bind(rule_key)
    .fetch_optional(db)
    .await?;

    let Some((tmpl_id, tmpl_content)) = row else { return Ok(()); };

    let has_pending: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM nexus_prompt_template_history WHERE template_id=$1 AND changed_by='nexus' AND changed_at > NOW() - INTERVAL '1 day'",
    )
    .bind(tmpl_id)
    .fetch_one(db)
    .await
    .unwrap_or(0);

    if has_pending > 0 { return Ok(()); }

    let examples: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT false_positive_reason FROM project_quality_findings WHERE false_positive_rule_key=$1 AND false_positive_at > NOW() - INTERVAL '7 days' LIMIT 3",
    )
    .bind(rule_key)
    .fetch_all(db)
    .await?;

    let examples_text: Vec<String> = examples.into_iter().flatten().collect();

    let suggestion = format!(
        "{}\n\n[Auto-suggestion pending based on {} false positives. Examples: {}]",
        tmpl_content,
        examples_text.len(),
        examples_text.join("; ")
    );

    sqlx::query(
        "INSERT INTO nexus_prompt_template_history (template_id, content, version, changed_by, change_note) SELECT id, $2, version, 'nexus', $3 FROM nexus_prompt_templates WHERE id=$1",
    )
    .bind(tmpl_id)
    .bind(&suggestion)
    .bind(format!("Auto-suggestion from {} false positives", examples_text.len()))
    .execute(db)
    .await?;

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SuggestedTool {
    pub tool_name: String,
    pub tool_server: String,
    pub usage_context: String,
}

/// Estrae il primo array JSON `[...]` da una stringa che puo contenere testo prima/dopo.
fn extract_json_array(s: &str) -> String {
    if let (Some(start), Some(end)) = (s.find('['), s.rfind(']')) {
        if start <= end {
            return s[start..=end].to_string();
        }
    }
    s.trim().to_string()
}

pub async fn ai_suggest_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<AiSuggestReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let template = sqlx::query_as::<_, PromptTemplate>(
        "SELECT id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context, schema_type, placeholder_vars, experimental FROM nexus_prompt_templates WHERE key = $1",
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))?
    .ok_or((StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "template non trovato"}))))?;

    // Provider/model da DB (purpose_model 'admin_fallback_default') se non specificati.
    // Niente fallback hardcoded: se la tabella non e' configurata, errore esplicito.
    let (db_provider, db_model) = if req.provider.is_none() || req.model.is_none() {
        sqlx::query_as::<_, (String, String)>(
            "SELECT provider, model_id FROM nexus_purpose_model WHERE purpose = 'admin_fallback_default' LIMIT 1"
        )
        .fetch_optional(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("DB error: {e}")}))))?
        .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
            "error": "nexus_purpose_model: 'admin_fallback_default' non configurato. Applica migrazione 0102."
        }))))?
    } else {
        (String::new(), String::new())
    };
    let provider = req.provider.as_deref().unwrap_or(&db_provider);
    let model = req.model.as_deref().unwrap_or(&db_model);

    let usage_ctx = template.usage_context.as_deref().unwrap_or("(nessun contesto d'uso)");

    let meta_prompt = format!(
        r#"Sei un esperto di prompt engineering per il sistema Nexus.

CONTESTO D'USO: {usage_ctx}
CHIAVE: {key}  CATEGORIA: {category}  TITOLO: {title}

CONTENUTO ATTUALE:
---
{content}
---

RICHIESTA: {instruction}

Rispondi SOLO con il nuovo testo del prompt, senza preamboli."#,
        usage_ctx = usage_ctx,
        key = template.key,
        category = template.category,
        title = template.title,
        content = template.content,
        instruction = req.instruction.trim(),
    );

    // Call brain service for LLM completion
    let brain_url = std::env::var("NEURAL_CORE_REST_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());

    let client = nexus_http::NexusClient::with_timeout(60).inner().clone();

    let response = client
        .post(format!("{brain_url}/generate"))
        .json(&serde_json::json!({
            "provider": provider,
            "model": model,
            "prompt": meta_prompt,
        }))
        .send()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))?;

    let result: serde_json::Value = response
        .json()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))?;

    let suggestion = result["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_string();

    // STEP 2: Suggest tools for this template (optional, non-blocking)
    let mut suggested_tools: Option<Vec<SuggestedTool>> = None;

    // Costruisci lista tool disponibili con nome e descrizione
    let tools_list = nexus_builtin_tools()
        .iter()
        .map(|t| {
            let desc = t.description.as_deref().unwrap_or("(no desc)");
            // Tronca a 120 chars per non sforare il context
            let short_desc = if desc.len() > 120 { &desc[..120] } else { desc };
            format!("- {}: {}", t.name, short_desc)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let tools_prompt = format!(
        r#"Sei un esperto di configurazione agenti AI.

TEMPLATE AGENTE:
  Titolo: {title}
  Categoria: {category}
  Contenuto (prime 400 chars): {content_preview}

TOOL DISPONIBILI:
{tools_list}

ISTRUZIONE: Seleziona i 4-8 tool che questo agente usera PIU SPESSO nei suoi task tipici.
Includi SOLO tool strettamente necessari al ruolo specifico. Escludi tool generici non pertinenti.

Rispondi con SOLO un array JSON valido, senza commenti, senza markdown:
[{{"tool_name":"nexus_fs_read","tool_server":"nexus_builtin"}},{{"tool_name":"nexus_git_status","tool_server":"nexus_builtin"}}]"#,
        title = template.title,
        category = template.category,
        content_preview = &template.content.chars().take(400).collect::<String>(),
        tools_list = tools_list,
    );

    if let Ok(tools_resp) = client
        .post(format!("{}/generate", brain_url))
        .json(&serde_json::json!({
            "provider": provider,
            "model": model,
            "prompt": tools_prompt,
        }))
        .send()
        .await
    {
        if let Ok(tools_result) = tools_resp.json::<serde_json::Value>().await {
            if let Some(tools_text) = tools_result["content"].as_str() {
                // Estrai array JSON dalla risposta (puo avere testo prima/dopo)
                let cleaned = extract_json_array(tools_text);
                if let Ok(tools) = serde_json::from_str::<Vec<SuggestedTool>>(&cleaned) {
                    if !tools.is_empty() {
                        suggested_tools = Some(tools);
                    }
                }
            }
        }
    }
    Ok(Json(serde_json::json!({
        "suggestion": suggestion,
        "provider": provider,
        "model": model,
        "suggested_tools": suggested_tools,
    })))
}

#[allow(dead_code)]
pub async fn get_disabled_quality_rules(db: &PgPool) -> std::collections::HashSet<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT key FROM nexus_prompt_templates WHERE category='quality' AND is_active=FALSE",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .collect()
}

// -- MCP Tools Management --

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PromptMcpTool {
    pub tool_name: String,
    pub tool_server: String,
    pub usage_context: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptToolsResponse {
    pub assigned_tools: Vec<PromptMcpTool>,
    pub suggested_tools: Vec<PromptMcpTool>,
    pub available_tools: Vec<PromptMcpTool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePromptToolsRequest {
    pub assigned_tools: Vec<PromptMcpTool>,
}

#[derive(Debug, Serialize)]
pub struct AvailableMcpTool {
    pub name: String,
    pub server: String,
    pub description: Option<String>,
    pub input_schema: Option<Value>,
}

/// GET /api/admin/prompts/:key/tools
pub async fn get_prompt_tools_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<PromptToolsResponse>, StatusCode> {
    let template = sqlx::query(
        "SELECT id, mcp_tools_json, suggested_tools_json FROM nexus_prompt_templates WHERE key=$1",
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let _template_id: i32 = template.get("id");
    let assigned_tools_json: Value = template.try_get("mcp_tools_json").unwrap_or(Value::Array(vec![]));
    let suggested_tools_json: Value = template.try_get("suggested_tools_json").unwrap_or(Value::Array(vec![]));

    let assigned_tools: Vec<PromptMcpTool> = serde_json::from_value(assigned_tools_json)
        .unwrap_or_default();
    let suggested_tools: Vec<PromptMcpTool> = serde_json::from_value(suggested_tools_json)
        .unwrap_or_default();

    // TODO: Load available tools from plugin-service
    let available_tools = Vec::new();

    Ok(Json(PromptToolsResponse {
        assigned_tools,
        suggested_tools,
        available_tools,
    }))
}

/// PUT /api/admin/prompts/:key/tools
pub async fn update_prompt_tools_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<UpdatePromptToolsRequest>,
) -> Result<StatusCode, StatusCode> {
    let tools_json = serde_json::to_value(&req.assigned_tools)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    sqlx::query(
        "UPDATE nexus_prompt_templates SET mcp_tools_json=$1, updated_at=NOW() WHERE key=$2",
    )
    .bind(tools_json)
    .bind(&key)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Built-in Nexus agent tools — always available regardless of external MCP servers.
fn nexus_builtin_tools() -> Vec<AvailableMcpTool> {
    let defs: &[(&str, &str)] = &[
        ("read_file",              "Legge il contenuto di un file del progetto"),
        ("read_file_lines",        "Legge un intervallo specifico di righe da un file"),
        ("write_file",             "Crea o sovrascrive un file con contenuto arbitrario"),
        ("edit_file",              "Sostituzione chirurgica di stringhe in un file (old_string → new_string)"),
        ("list_files",             "Elenca file e directory in un percorso del progetto"),
        ("delete_file",            "Elimina un file o directory (con opzione ricorsiva)"),
        ("rename_file",            "Rinomina o sposta un file/directory"),
        ("search_in_files",        "Cerca testo o espressioni regolari nei file del progetto"),
        ("search_codebase_semantic","Ricerca semantica nel codebase tramite embedding vettoriale"),
        ("search_file_semantic",   "Ricerca semantica all'interno di un singolo file (TF-IDF)"),
        ("scan_code_quality",      "Analizza qualità del codice: complessità, naming, smells, sicurezza"),
        ("git_status",             "Mostra lo stato del repository Git (staged, unstaged, untracked)"),
        ("git_stage",              "Aggiunge file all'area di staging Git"),
        ("git_commit",             "Crea un commit Git con messaggio"),
        ("git_push",               "Effettua push al remote Git"),
        ("git_pull",               "Effettua pull con rebase dal remote Git"),
        ("run_command",            "Esegue un comando shell (sincrono o in background)"),
        ("run_tests",              "Esegue i test del progetto con auto-rilevamento del comando (npm/cargo/pytest/ecc.) — ciclo test-fix-test"),
        ("run_service",            "Avvia un processo a lunga esecuzione (server, watcher, ecc.)"),
        ("read_service_output",    "Legge l'output di un servizio avviato con run_service"),
        ("stop_service",           "Termina un processo avviato con run_service"),
        ("create_profile",         "Crea un profilo utente specializzato per un dominio"),
        ("update_profile",         "Aggiorna un profilo utente esistente"),
        ("dispatch_subtask",       "Delega lavoro a un sotto-agente parallelo"),
        ("nexus_doc_generate",     "Genera un documento Word (.docx) dal contenuto del progetto"),
        ("request_tools",          "Richiede l'abilitazione di tool MCP aggiuntivi durante una sessione"),
    ];
    defs.iter()
        .map(|(name, desc)| AvailableMcpTool {
            name: name.to_string(),
            server: "nexus_builtin".to_string(),
            description: Some(desc.to_string()),
            input_schema: None,
        })
        .collect()
}

/// GET /api/admin/available-mcp-tools
pub async fn get_available_mcp_tools_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<AvailableMcpTool>>, StatusCode> {
    // Start with built-in Nexus tools (always available)
    let mut tools = nexus_builtin_tools();

    // Append tools from registered external MCP servers
    let rows = sqlx::query(
        r#"SELECT DISTINCT
            mcp_tools.name,
            mcp_servers.name as server_name,
            mcp_tools.description,
            mcp_tools.input_schema
        FROM mcp_server_tools as mcp_tools
        JOIN mcp_servers ON mcp_tools.server_id = mcp_servers.id
        WHERE mcp_servers.enabled = true
        ORDER BY mcp_servers.name, mcp_tools.name"#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    for row in &rows {
        tools.push(AvailableMcpTool {
            name: row.get("name"),
            server: row.get("server_name"),
            description: row.try_get("description").ok(),
            input_schema: row.try_get("input_schema").ok(),
        });
    }

    Ok(Json(tools))
}

// ─────────────────────────────────────────────────────────────────────────────
// STEP 3: Bulk Tool Assignment Handler
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BulkAssignToolsReq {
    pub template_keys: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct BulkToolResult {
    pub key: String,
    pub tools_count: usize,
}

static BATCH_ASSIGN_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static BATCH_ASSIGN_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Riduce drasticamente gli strumenti assegnati quando il catalogo MCP DB è grande: solo ricerca+call runtime.
fn apply_meta_builtin_tools_substitution(
    catalog_mcp_tools_count: usize,
    tool_pairs: &HashSet<(String, String)>,
    normalized: &mut Vec<serde_json::Value>,
) {
    if catalog_mcp_tools_count < 80 || normalized.is_empty() {
        return;
    }
    let servers: HashSet<String> = tool_pairs.iter().map(|(srv, _)| srv.clone()).collect();
    let mut meta_server: Option<String> = None;
    for srv in servers {
        let search_pair = (srv.clone(), "nexus_mcp_tool_search".to_string());
        let call_pair = (srv.clone(), "nexus_mcp_tool_call".to_string());
        if tool_pairs.contains(&search_pair) && tool_pairs.contains(&call_pair) {
            meta_server = Some(srv);
            break;
        }
    }
    let Some(srv) = meta_server else {
        return;
    };
    let usage_search = "Cerca tool MCP disponibili solo quando servono (riduce token).";
    let usage_call = "Invoca un tool MCP specifico (server_id + tool_name).";
    *normalized = vec![
        serde_json::json!({
            "tool_name": "nexus_mcp_tool_search",
            "tool_server": srv.clone(),
            "usage_context": usage_search,
        }),
        serde_json::json!({
            "tool_name": "nexus_mcp_tool_call",
            "tool_server": srv,
            "usage_context": usage_call,
        }),
    ];
}

async fn run_batch_assign_tools_job(
    state: AppState,
    req: Option<BulkAssignToolsReq>,
) -> Result<serde_json::Value, String> {
    // Carica template: per default tutti (non solo 'agent')
    let query = if let Some(keys) = req
        .as_ref()
        .and_then(|r| r.template_keys.as_ref())
        .filter(|k| !k.is_empty())
    {
        sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT key, title, content, category FROM nexus_prompt_templates WHERE key = ANY($1) ORDER BY key",
        )
        .bind(keys)
        .fetch_all(&state.db)
        .await
    } else {
        sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT key, title, content, category FROM nexus_prompt_templates ORDER BY key",
        )
        .fetch_all(&state.db)
        .await
    }
    .map_err(|e| e.to_string())?;

    let brain_url =
        std::env::var("NEURAL_CORE_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let client = nexus_http::NexusClient::with_timeout(30).inner().clone();

    // Lista compatta token-saver (stesso formato di mcp-core): `server::tool` senza descrizioni lunghe.
    let mut tool_lines: Vec<String> = Vec::new();
    let mut tool_by_pair: HashSet<(String, String)> = HashSet::new();

    for t in nexus_builtin_tools().iter() {
        tool_lines.push(format!("- {}::{}", t.server, t.name));
    }

    let rows = sqlx::query(
        r#"SELECT DISTINCT
            mcp_tools.name,
            mcp_servers.name as server_name
        FROM mcp_server_tools as mcp_tools
        JOIN mcp_servers ON mcp_tools.server_id = mcp_servers.id
        WHERE mcp_servers.enabled = true
        ORDER BY mcp_servers.name, mcp_tools.name"#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| e.to_string())?;

    let catalog_mcp_tools_count = rows.len();

    for row in &rows {
        let name: String = row.get("name");
        let server_name: String = row.get("server_name");
        tool_by_pair.insert((server_name.clone(), name.clone()));
        tool_lines.push(format!("- {}::{}", server_name, name));
    }

    let tools_list = tool_lines.join("\n");

    const BASE_MAX: usize = 3;
    const HARD_MAX: usize = 8;

    // Provider/model per la selezione tool: letti da nexus_purpose_model
    // purpose='admin.tool_selection' (mig 0171). CLAUDE.md §G: niente hardcode.
    let (admin_provider, admin_model) = sqlx::query_as::<_, (String, String)>(
        "SELECT provider, model_id FROM nexus_purpose_model WHERE purpose = 'admin.tool_selection' LIMIT 1"
    )
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("DB error caricando admin.tool_selection: {e}"))?
    .ok_or_else(|| "nexus_purpose_model: 'admin.tool_selection' non configurato. Applica migrazione 0171.".to_string())?;

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut assigned = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for (key, title, content, category) in &query {
        let content_preview: String = content.chars().take(700).collect();

        let prompt = format!(
            r#"Sei un esperto di configurazione tool per prompt template.

TEMPLATE:
  Chiave: {key}
  Titolo: {title}
  Categoria: {category}
  Contenuto (prime 700 chars): {content_preview}

TOOL DISPONIBILI (formato compatto: `- server_esatto::nome_tool`; copia `tool_server` e `tool_name` esattamente così come appaiono prima di `::` e dopo):
{tools_list}

ISTRUZIONE:
- Seleziona SOLO i tool indispensabili per questo template (massimo {BASE_MAX} in assenza di motivi forti).
- Puoi superare {BASE_MAX} SOLO se aggiungi un campo usage_context non vuoto per ogni tool extra, fino a un massimo HARD di {HARD_MAX}.
- Evita tool generici/non correlati.

Rispondi con SOLO un array JSON valido, nessun testo aggiuntivo:
[{{"tool_name":"read_file","tool_server":"nexus_builtin"}},{{"tool_name":"browser.navigate","tool_server":"Nexus Browser Bridge (localci)","usage_context":"navigazione E2E"}}]"#,
        );

        match client
            .post(format!("{brain_url}/generate"))
            .json(&serde_json::json!({
                "provider": admin_provider,
                "model": admin_model,
                "prompt": prompt,
            }))
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(result) = resp.json::<serde_json::Value>().await {
                    let content_str = result["content"].as_str().unwrap_or("[]");
                    let json_str = extract_json_array(content_str);
                    match serde_json::from_str::<serde_json::Value>(&json_str) {
                        Ok(tools_val) if tools_val.is_array() => {
                            let Some(tools_arr) = tools_val.as_array() else { continue };
                            let mut normalized: Vec<serde_json::Value> = Vec::new();

                            for (idx, item) in tools_arr.iter().enumerate() {
                                let Some(obj) = item.as_object() else { continue };
                                let tool_name = obj.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").trim();
                                let tool_server = obj.get("tool_server").and_then(|v| v.as_str()).unwrap_or("").trim();
                                if tool_name.is_empty() || tool_server.is_empty() {
                                    continue;
                                }
                                let usage_context = obj
                                    .get("usage_context")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty());

                                // Cap logic: allow beyond BASE_MAX only with usage_context
                                if idx < BASE_MAX {
                                    normalized.push(serde_json::json!({
                                        "tool_name": tool_name,
                                        "tool_server": tool_server,
                                    }));
                                } else if normalized.len() < HARD_MAX {
                                    if let Some(uc) = usage_context {
                                        normalized.push(serde_json::json!({
                                            "tool_name": tool_name,
                                            "tool_server": tool_server,
                                            "usage_context": uc,
                                        }));
                                    }
                                }
                            }

                            apply_meta_builtin_tools_substitution(
                                catalog_mcp_tools_count,
                                &tool_by_pair,
                                &mut normalized,
                            );

                            let count = normalized.len();
                            if count > 0 {
                                let tools_val = serde_json::Value::Array(normalized);
                                let _ = sqlx::query(
                                    "UPDATE nexus_prompt_templates SET mcp_tools_json = $1, updated_at = NOW() WHERE key = $2",
                                )
                                .bind(&tools_val)
                                .bind(key)
                                .execute(&state.db)
                                .await;
                                assigned += 1;
                                results.push(serde_json::json!({"key": key, "tools_count": count, "status": "ok"}));
                            } else {
                                skipped += 1;
                                results.push(serde_json::json!({"key": key, "status": "empty_response"}));
                            }
                        }
                        _ => {
                            errors += 1;
                            results.push(serde_json::json!({"key": key, "status": "parse_error", "raw": &json_str[..json_str.len().min(120)]}));
                        }
                    }
                } else {
                    errors += 1;
                    results.push(serde_json::json!({"key": key, "status": "json_error"}));
                }
            }
            Err(e) => {
                errors += 1;
                results.push(serde_json::json!({"key": key, "status": "request_error", "error": e.to_string()}));
            }
        }
    }

    Ok(serde_json::json!({
        "status": "completed",
        "processed": query.len(),
        "assigned": assigned,
        "skipped": skipped,
        "errors": errors,
        "results": results,
        "base_max_tools_per_template": BASE_MAX,
        "hard_max_tools_per_template": HARD_MAX,
    }))
}

pub async fn batch_assign_tools_handler(
    State(state): State<AppState>,
    Json(req): Json<Option<BulkAssignToolsReq>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Modalità asincrona (non blocca UI): esegui in background con debounce.
    if BATCH_ASSIGN_RUNNING
        .compare_exchange(false, true, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst)
        .is_ok()
    {
        let state_clone = state.clone();
        tokio::spawn(async move {
            let _ = run_batch_assign_tools_job(state_clone, req).await;
            BATCH_ASSIGN_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);

            // Se nel frattempo è arrivata un'altra richiesta, riesegui una volta.
            if BATCH_ASSIGN_PENDING.swap(false, std::sync::atomic::Ordering::SeqCst) {
                if BATCH_ASSIGN_RUNNING
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    let state_clone2 = state.clone();
                    tokio::spawn(async move {
                        let _ = run_batch_assign_tools_job(state_clone2, None).await;
                        BATCH_ASSIGN_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
                    });
                }
            }
        });

        Ok(Json(serde_json::json!({
            "status": "queued",
            "message": "Assegnazione tool avviata in background.",
        })))
    } else {
        BATCH_ASSIGN_PENDING.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Json(serde_json::json!({
            "status": "queued",
            "message": "Assegnazione tool già in corso: richiesta accodata.",
        })))
    }
}
