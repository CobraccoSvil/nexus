//! Prompt templates: cache TTL e endpoint HTTP admin.
//!
//! La `TemplateCache` mantiene prompt caricati dal DB con TTL configurabile.
//! Cache miss e chiavi inesistenti restituiscono `None`.

use axum::response::IntoResponse;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::sync::atomic::{AtomicBool, Ordering};

// TemplateCache e get_template_or_default: punto unico in nexus-types
// (regola L / ADR 0026). Qui solo re-export per i call site interni
// `crate::prompt_templates::{TemplateCache, get_template_or_default}`.
pub use nexus_types::{get_template_or_default, TemplateCache};

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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PromptMcpTool {
    pub tool_name: String,
    pub tool_server: String,
    pub usage_context: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AiSuggestReq {
    pub instruction: String,
    pub provider: Option<String>,
    pub model: Option<String>,
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
pub struct FalsePositiveReq {
    pub reason: Option<String>,
    pub rule_key: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FalsePositiveStat {
    pub rule_key: Option<String>,
    pub count: Option<i64>,
}

// La costante AGENT_ACT_FIRST_SUFFIX e' stata estratta nel DB come
// `system.nexus_act_first_suffix` (mig 0441): caricata via get_template_or_default
// in handlers.rs (gate automation_mode != Study). Regola G/D: niente prompt
// hardcoded nei sorgenti, modificabile a caldo dalla pagina admin (cache 60s).

/// GET /api/prompt-templates
pub async fn list_templates_handler(
    State(state): State<crate::AppState>,
) -> Result<Json<Vec<PromptTemplate>>, StatusCode> {
    let templates = sqlx::query_as::<_, PromptTemplate>(
        "SELECT id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context FROM nexus_prompt_templates ORDER BY category, key"
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(templates))
}

/// GET /api/prompt-templates/:key
pub async fn get_template_handler(
    State(state): State<crate::AppState>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let template = sqlx::query_as::<_, PromptTemplate>(
        "SELECT id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context FROM nexus_prompt_templates WHERE key = $1"
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let history = sqlx::query_as::<_, PromptTemplateHistory>(
        "SELECT id, template_id, content, version, changed_by, changed_at, change_note FROM nexus_prompt_template_history WHERE template_id = $1 ORDER BY version DESC LIMIT 20"
    )
    .bind(template.id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    Ok(Json(
        serde_json::json!({ "template": template, "history": history }),
    ))
}

/// PUT /api/prompt-templates/:key
pub async fn upsert_template_handler(
    State(state): State<crate::AppState>,
    Path(key): Path<String>,
    Json(req): Json<UpsertTemplateReq>,
) -> Result<Json<PromptTemplate>, StatusCode> {
    let updated_by = req.updated_by.unwrap_or_else(|| "user".to_string());

    // Get current version
    let current = sqlx::query("SELECT id, version FROM nexus_prompt_templates WHERE key = $1")
        .bind(&key)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|row| (row.get::<i32, _>("id"), row.get::<i32, _>("version")));

    let template = if let Some((cur_id, _)) = current {
        // Save history
        let _ = sqlx::query(
            "INSERT INTO nexus_prompt_template_history (template_id, content, version, changed_by, change_note) SELECT id, content, version, $2, $3 FROM nexus_prompt_templates WHERE id = $1"
        )
        .bind(cur_id)
        .bind(&updated_by)
        .bind(&req.change_note)
        .execute(&state.db)
        .await;

        // Update
        sqlx::query_as::<_, PromptTemplate>(
            "UPDATE nexus_prompt_templates SET content=$1, version=version+1, updated_by=$2, updated_at=NOW(), title=COALESCE($3, title) WHERE key=$4 RETURNING id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context"
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
            "INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES ($1, 'system', $2, $3, $4) RETURNING id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context"
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

/// POST /api/prompt-templates/:key/disable
pub async fn disable_template_handler(
    State(state): State<crate::AppState>,
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

/// POST /api/prompt-templates/:key/enable
pub async fn enable_template_handler(
    State(state): State<crate::AppState>,
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

/// POST /api/quality/findings/:id/false-positive
pub async fn mark_false_positive_handler(
    State(state): State<crate::AppState>,
    Path(finding_id): Path<uuid::Uuid>,
    Json(req): Json<FalsePositiveReq>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    sqlx::query(
        "UPDATE project_quality_findings SET is_false_positive=TRUE, false_positive_reason=$1, false_positive_at=NOW(), false_positive_rule_key=$2 WHERE id=$3"
    )
    .bind(&req.reason)
    .bind(&req.rule_key)
    .bind(finding_id)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Check if we should trigger nexus auto-suggestion
    if let Some(rule_key) = &req.rule_key {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM project_quality_findings WHERE false_positive_rule_key=$1 AND false_positive_at > NOW() - INTERVAL '7 days'"
        )
        .bind(rule_key)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

        if count >= 3 {
            // Spawn background task to generate nexus suggestion
            let db = state.db.clone();
            let rk = rule_key.clone();
            tokio::spawn(async move {
                let _ = generate_nexus_suggestion(&db, &rk).await;
            });
        }
    }

    Ok(Json(serde_json::json!({"ok": true})))
}

/// GET /api/quality/false-positive-stats
pub async fn false_positive_stats_handler(
    State(state): State<crate::AppState>,
) -> Result<Json<Vec<FalsePositiveStat>>, StatusCode> {
    let rows = sqlx::query(
        "SELECT false_positive_rule_key as rule_key, COUNT(*) as count FROM project_quality_findings WHERE is_false_positive=TRUE AND false_positive_rule_key IS NOT NULL AND false_positive_at > NOW() - INTERVAL '7 days' GROUP BY false_positive_rule_key ORDER BY count DESC"
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stats: Vec<FalsePositiveStat> = rows
        .iter()
        .map(|row| FalsePositiveStat {
            rule_key: row.get("rule_key"),
            count: row.get("count"),
        })
        .collect();
    Ok(Json(stats))
}

async fn generate_nexus_suggestion(db: &PgPool, rule_key: &str) -> anyhow::Result<()> {
    // Get current template content
    let template = sqlx::query("SELECT id, content FROM nexus_prompt_templates WHERE key=$1")
        .bind(rule_key)
        .fetch_optional(db)
        .await?
        .map(|row| (row.get::<i32, _>("id"), row.get::<String, _>("content")));

    let Some((tmpl_id, tmpl_content)) = template else {
        return Ok(());
    };

    // Check if nexus suggestion already pending (version unchanged since last nexus suggestion)
    let has_pending = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM nexus_prompt_template_history WHERE template_id=$1 AND changed_by='nexus' AND changed_at > NOW() - INTERVAL '1 day'"
    )
    .bind(tmpl_id)
    .fetch_one(db)
    .await?;

    if has_pending > 0 {
        return Ok(());
    }

    // Get recent FP examples
    let examples = sqlx::query(
        "SELECT false_positive_reason FROM project_quality_findings WHERE false_positive_rule_key=$1 AND false_positive_at > NOW() - INTERVAL '7 days' LIMIT 3"
    )
    .bind(rule_key)
    .fetch_all(db)
    .await?;

    let examples_text: Vec<String> = examples
        .iter()
        .filter_map(|row| row.get::<Option<String>, _>("false_positive_reason"))
        .collect();

    // Save nexus suggestion in history (as placeholder — full LLM call can be added later)
    let suggestion = format!(
        "{}\n\n[Auto-suggestion pending based on {} false positives. Examples: {}]",
        tmpl_content,
        examples_text.len(),
        examples_text.join("; ")
    );

    sqlx::query(
        "INSERT INTO nexus_prompt_template_history (template_id, content, version, changed_by, change_note) SELECT id, $2, version, 'nexus', $3 FROM nexus_prompt_templates WHERE id=$1"
    )
    .bind(tmpl_id)
    .bind(&suggestion)
    .bind(format!("Auto-suggestion from {} false positives", examples_text.len()))
    .execute(db)
    .await?;

    Ok(())
}

/// Genera testo rispettando l'ordine dei provider configurato in admin (settings.provider_hierarchy).
/// Se un provider restituisce errore di quota/credito/rate, tenta il successivo nella lista admin.
/// Non aggiunge provider extra: la configurazione admin è autoritativa.
async fn generate_with_admin_fallback(
    neural: &crate::orchestrator::NeuralCoreClient,
    db: &PgPool,
    routing_matrix: &crate::routing_matrix::RoutingMatrix,
    prompt: &str,
    broken_providers: &mut std::collections::HashSet<String>,
    billing_user_id: uuid::Uuid,
    billing_project_id: uuid::Uuid,
) -> Result<serde_json::Value, String> {
    use crate::billing::{self, UsageNumbers};
    use crate::orchestrator::default_model_for_provider;

    // Carica l'ordine provider dall'admin (stesso campo usato dall'agent loop)
    let hierarchy_str: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'provider_hierarchy' LIMIT 1")
            .fetch_optional(db)
            .await
            .unwrap_or(None);

    let providers_csv = hierarchy_str.ok_or_else(|| {
        "Impostazione 'provider_hierarchy' non configurata nella tabella settings".to_string()
    })?;
    let providers: Vec<String> = providers_csv
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && !broken_providers.contains(s.as_str()))
        .collect();

    if providers.is_empty() {
        return Err("Nessun provider disponibile (tutti skip o non configurati)".to_string());
    }

    for provider in &providers {
        let model = default_model_for_provider(routing_matrix, provider);
        // Billing: riserva prima di chiamare il provider, finalizza dopo.
        // Nota: qui non abbiamo un token_budget esplicito; stimiamo un upper bound.
        let prompt_tokens = mcp_token::count_tokens(prompt) as i32;
        let estimated_completion_tokens = 800i32;
        let reservation = match billing::reserve_usage(
            db,
            billing_user_id,
            billing_project_id,
            provider,
            &model,
            prompt_tokens,
            estimated_completion_tokens,
            serde_json::json!({
                "feature": "batch_assign_tools",
                "via": "prompt_templates::generate_with_admin_fallback",
            }),
        )
        .await
        {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::error!(
                    "batch: billing reserve FAILED (provider={provider} model={model}): {e}"
                );
                None
            }
        };

        match neural.generate_completion(provider, &model, prompt).await {
            Ok(v) => {
                // Finalizza il costo con i token reali (se presenti), altrimenti usa stima.
                let usage_numbers: UsageNumbers =
                    billing::extract_usage_numbers(&v, prompt_tokens, estimated_completion_tokens);
                if let Some(res) = &reservation {
                    if let Err(e) =
                        billing::finalize_usage(db, res, uuid::Uuid::new_v4(), &usage_numbers).await
                    {
                        tracing::error!("batch: billing finalize FAILED: {e}");
                    }
                }

                let content = v["content"].as_str().unwrap_or("");
                let lower = content.to_lowercase();
                // Controlla se la risposta è un errore di quota/credito/rate limit
                if lower.contains("credit balance")
                    || lower.contains("too low")
                    || lower.contains("quota")
                    || lower.contains("rate limit")
                    || lower.contains("rate_limit")
                    || lower.contains("529")
                    || lower.contains("overloaded")
                    || (lower.contains("429") && lower.contains("exceeded"))
                {
                    tracing::warn!(
                        "batch: provider {} esaurito/rate-limited → marcato broken per il resto del batch",
                        provider
                    );
                    // Marca come broken: non verrà ritentato nei template successivi
                    broken_providers.insert(provider.clone());
                    continue;
                }
                tracing::debug!("batch: provider {} OK", provider);
                return Ok(v);
            }
            Err(e) => {
                // In caso di errore, rilascia la riserva (non conteggiare).
                if let Some(res) = &reservation {
                    billing::release_usage(db, res, "provider_error").await;
                }
                tracing::warn!(
                    "batch: provider {} errore gRPC: {}, marcato broken",
                    provider,
                    e
                );
                broken_providers.insert(provider.clone());
                continue;
            }
        }
    }

    Err(format!(
        "Tutti i provider admin ({}) hanno fallito. Controlla le API key in admin.",
        providers.join(", ")
    ))
}

/// POST /api/prompt-templates/:key/ai-suggest
/// Genera un nuovo contenuto per il prompt usando un LLM, con contesto d'uso preinserito.
pub async fn ai_suggest_handler(
    State(state): State<crate::AppState>,
    Path(key): Path<String>,
    Json(req): Json<AiSuggestReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let template = sqlx::query_as::<_, PromptTemplate>(
        "SELECT id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context FROM nexus_prompt_templates WHERE key = $1"
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))?
    .ok_or((StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "template non trovato"}))))?;

    // Default provider/model dal PUNTO UNICO tier-only (regola L/G).
    let (purpose_provider, purpose_model) =
        crate::internal_routing::resolve_purpose_model(&state, "admin_fallback_default")
            .await
            .into_model("admin_fallback_default")
            .map_err(|m| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": m})),
                )
            })?;
    let provider: String = req
        .provider
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or(purpose_provider);
    let model: String = req
        .model
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or(purpose_model);

    let usage_ctx = template
        .usage_context
        .as_deref()
        .unwrap_or("(nessun contesto d'uso documentato per questo prompt)");

    // Meta-prompt dal DB (system.ai_suggest_meta_prompt, mig 0445); fallback al
    // default builtin se DB down. {{content}} per ultimo: il content puo'
    // contenere placeholder e non va corrotto dai replace dei metadati.
    let meta_prompt =
        get_template_or_default(&state.db, &state.template_cache, "system.ai_suggest_meta_prompt")
            .await
            .replace("{{usage_ctx}}", usage_ctx)
            .replace("{{key}}", &template.key)
            .replace("{{category}}", &template.category)
            .replace("{{title}}", &template.title)
            .replace("{{instruction}}", req.instruction.trim())
            .replace("{{content}}", &template.content);

    let result = state
        .orchestrator
        .neural
        .generate_completion(provider.as_str(), model.as_str(), &meta_prompt)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
        })?;

    let suggestion = result["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_string();

    // --- Tool suggestion automatica ---
    let suggested_tools: Vec<PromptMcpTool> = {
        let rows = sqlx::query(
            r#"SELECT DISTINCT
                mcp_tools.tool_name as name,
                mcp_servers.name as server_name,
                mcp_tools.description
            FROM mcp_server_tools as mcp_tools
            JOIN mcp_servers ON mcp_tools.server_id = mcp_servers.id
            WHERE mcp_servers.enabled = true
            ORDER BY mcp_servers.name, mcp_tools.tool_name"#,
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        let tools_list = rows
            .iter()
            .map(|r| {
                let name: String = r.get("name");
                let desc: Option<String> = r.try_get("description").ok().flatten();
                format!("- {}: {}", name, desc.as_deref().unwrap_or(""))
            })
            .collect::<Vec<_>>()
            .join("\n");

        if tools_list.is_empty() {
            vec![]
        } else {
            let tool_prompt = get_template_or_default(
                &state.db,
                &state.template_cache,
                "system.tool_selection_single_prompt",
            )
            .await
            .replace("{{tools_list}}", &tools_list)
            .replace("{{content}}", &suggestion);

            // Usa lo stesso provider/model della richiesta principale per la tool suggestion
            let tool_result = state
                .orchestrator
                .neural
                .generate_completion(provider.as_str(), model.as_str(), &tool_prompt)
                .await
                .unwrap_or_default();

            let tool_names: Vec<String> = tool_result["content"]
                .as_str()
                .map(|s| {
                    let s = s.trim();
                    let start = s.find('[').unwrap_or(0);
                    let end = s.rfind(']').map(|i| i + 1).unwrap_or(s.len());
                    serde_json::from_str::<Vec<String>>(&s[start..end]).unwrap_or_default()
                })
                .unwrap_or_default();

            let tool_map: std::collections::HashMap<String, String> = rows
                .iter()
                .map(|r| {
                    (
                        r.get::<String, _>("name"),
                        r.get::<String, _>("server_name"),
                    )
                })
                .collect();

            let prompt_tools: Vec<PromptMcpTool> = tool_names
                .iter()
                .filter_map(|name| {
                    tool_map.get(name).map(|server| PromptMcpTool {
                        tool_name: name.clone(),
                        tool_server: server.clone(),
                        usage_context: None,
                    })
                })
                .collect();

            if !prompt_tools.is_empty() {
                let tools_json = serde_json::to_value(&prompt_tools).unwrap_or_default();
                let _ = sqlx::query(
                    "UPDATE nexus_prompt_templates SET mcp_tools_json=$1, updated_at=NOW() WHERE key=$2",
                )
                .bind(tools_json)
                .bind(&key)
                .execute(&state.db)
                .await;
            }

            prompt_tools
        }
    };

    Ok(Json(serde_json::json!({
        "suggestion": suggestion,
        "provider": provider,
        "model": model,
        "suggested_tools": suggested_tools,
    })))
}

async fn batch_assign_tools_impl(
    State(state): State<crate::AppState>,
    billing_user_id: uuid::Uuid,
    billing_project_id: uuid::Uuid,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Marker “hard” per rendere verificabile che il job è partito e che scrive sul DB giusto.
    // Non dipende da orchestrator_runs/run_id e non blocca il job se fallisce.
    {
        let marker_id = uuid::Uuid::new_v4();
        if let Err(e) = sqlx::query(
            r#"
            INSERT INTO ai_usage_ledger (
                id, user_id, project_id, provider, model,
                prompt_tokens, completion_tokens, total_tokens,
                input_cost, output_cost, total_cost, currency,
                status, details
            ) VALUES ($1, $2, $3, 'internal', 'batch_assign_tools_job', 0, 0, 0, 0, 0, 0, 'EUR', 'reserved', $4)
            "#,
        )
        .bind(marker_id)
        .bind(billing_user_id)
        .bind(billing_project_id)
        .bind(serde_json::json!({
            "feature": "batch_assign_tools",
            "event": "job_started",
        }))
        .execute(&state.db)
        .await
        {
            tracing::error!("batch_assign_tools: marker insert FAILED: {e}");
        } else {
            tracing::info!("batch_assign_tools: marker inserted ledger_id={marker_id}");
        }
    }

    let templates = sqlx::query(
        "SELECT key, title, content, category FROM nexus_prompt_templates WHERE is_active = true ORDER BY category, key",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))?;

    // Carica TUTTI i tool MCP abilitati (inclusi server esterni gestiti) con descrizioni troncate.
    let tool_rows = sqlx::query(
        r#"SELECT DISTINCT
            mcp_tools.tool_name as name,
            mcp_servers.name as server_name,
            mcp_tools.description
        FROM mcp_server_tools as mcp_tools
        JOIN mcp_servers ON mcp_tools.server_id = mcp_servers.id
        WHERE mcp_servers.enabled = true
        ORDER BY mcp_servers.name, mcp_tools.tool_name"#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    if tool_rows.is_empty() {
        return Ok(Json(serde_json::json!({
            "processed": 0, "assigned": 0, "skipped": templates.len(), "errors": 0,
            "message": "Nessun tool MCP disponibile"
        })));
    }

    // Token/costo: NON includere descrizioni qui (sono tantissime e fanno esplodere il prompt).
    // Lista minima, disambiguata: "server::tool".
    let tools_list = tool_rows
        .iter()
        .map(|r| {
            let name: String = r.get("name");
            let server: String = r.get("server_name");
            format!("- {}::{}", server, name)
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Mappa per lookup:
    // - by_pair: (server, tool_name) -> true
    // - by_name: tool_name -> set(server) per gestire collisioni e richiedere disambiguazione quando serve.
    let mut tool_by_pair: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut tool_servers_by_name: std::collections::HashMap<
        String,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();
    for r in &tool_rows {
        let name: String = r.get("name");
        let server: String = r.get("server_name");
        tool_by_pair.insert((server.clone(), name.clone()));
        tool_servers_by_name.entry(name).or_default().insert(server);
    }

    // Set dei provider che hanno fallito - evita di ritentarli per ogni template
    let mut broken_providers: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut processed = 0usize;
    let mut assigned = 0usize;
    let mut errors = 0usize;
    let mut total_tools_selected = 0usize;
    let mut results: Vec<serde_json::Value> = Vec::new();

    for row in &templates {
        let key: String = row.get("key");
        let title: String = row.get("title");
        let category: String = row.get("category");
        let content_text: String = row.get("content");
        let content_preview: String = content_text.chars().take(400).collect();

        // Obiettivo: assegnare SOLO i tool effettivamente necessari, minimizzando token/costo.
        // Logica adattiva:
        // - fino a BASE_MAX accettiamo anche tool senza "usage_context"
        // - oltre BASE_MAX, accettiamo solo tool con usage_context (giustificazione) per evitare over-assign.
        // - HARD_MAX resta un limite di sicurezza.
        const BASE_MAX: usize = 3;
        const HARD_MAX: usize = 8;

        // Meta-prompt dal DB (system.batch_tool_assignment_prompt, mig 0445);
        // fallback al default builtin se DB down. {{role}} per ultimo (l'estratto
        // del prompt template puo' contenere placeholder).
        let tool_prompt = get_template_or_default(
            &state.db,
            &state.template_cache,
            "system.batch_tool_assignment_prompt",
        )
        .await
        .replace("{{key}}", &key)
        .replace("{{title}}", &title)
        .replace("{{category}}", &category)
        .replace("{{tools_list}}", &tools_list)
        .replace("{{base_max}}", &BASE_MAX.to_string())
        .replace("{{hard_max}}", &HARD_MAX.to_string())
        .replace("{{role}}", &content_preview);

        processed += 1;
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        let matrix_arc = match state.orchestrator.routing_matrix.current_async().await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(
                    "regenerate_tool_suggestions: routing_matrix non disponibile ({e}), skip"
                );
                continue;
            }
        };
        let tool_result = generate_with_admin_fallback(
            &state.orchestrator.neural,
            &state.db,
            &matrix_arc,
            &tool_prompt,
            &mut broken_providers,
            billing_user_id,
            billing_project_id,
        )
        .await;

        match tool_result {
            Ok(v) => {
                let raw = v["content"].as_str().unwrap_or("[]");
                let start = raw.find('[').unwrap_or(0);
                let end = raw.rfind(']').map(|i| i + 1).unwrap_or(raw.len());
                let json_slice = &raw[start..end];
                match serde_json::from_str::<serde_json::Value>(json_slice) {
                    // Match con if-let invece di guardia is_array() + unwrap successivo.
                    Ok(serde_json::Value::Array(ref arr_ref_owned)) => {
                        let arr_ref = arr_ref_owned;
                        // Parsing robusto:
                        // - accetta ["tool_name", ...]
                        // - accetta [{tool_name, usage_context?}, ...]
                        // - ignora tool non presenti in tool_map
                        // - de-duplica
                        let mut seen: std::collections::HashSet<String> =
                            std::collections::HashSet::new();
                        let mut prompt_tools: Vec<PromptMcpTool> = Vec::new();

                        for item in arr_ref {
                            // 1) String: "tool" oppure "server::tool"
                            // 2) Object: {tool_name, tool_server?, usage_context?}
                            let mut name: Option<String> = None;
                            let mut server: Option<String> = None;
                            let mut usage_ctx_opt: Option<String> = None;

                            if let Some(s) = item.as_str() {
                                let s = s.trim();
                                if let Some((srv, nm)) = s.split_once("::") {
                                    let srv = srv.trim();
                                    let nm = nm.trim();
                                    if !srv.is_empty() && !nm.is_empty() {
                                        server = Some(srv.to_string());
                                        name = Some(nm.to_string());
                                    }
                                } else if !s.is_empty() {
                                    name = Some(s.to_string());
                                }
                            } else if let Some(obj) = item.as_object() {
                                name = obj
                                    .get("tool_name")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty());
                                server = obj
                                    .get("tool_server")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty());
                                usage_ctx_opt = obj
                                    .get("usage_context")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.trim())
                                    .filter(|s| !s.is_empty())
                                    .map(|s| s.to_string());
                            }

                            let Some(tool_name) = name else {
                                continue;
                            };

                            // Se server non specificato:
                            // - accetta solo se il tool_name è univoco tra i server
                            // - se ambiguo, richiede "server::tool" o tool_server nel JSON.
                            let tool_server = if let Some(srv) = server {
                                srv
                            } else {
                                let servers = tool_servers_by_name.get(&tool_name);
                                match servers {
                                    Some(s) if s.len() == 1 => {
                                        s.iter().next().cloned().unwrap_or_default()
                                    }
                                    _ => continue, // ambiguo o inesistente
                                }
                            };

                            if !tool_by_pair.contains(&(tool_server.clone(), tool_name.clone())) {
                                continue;
                            }

                            let dedup_key = format!("{}::{}", tool_server, tool_name);
                            if !seen.insert(dedup_key) {
                                continue;
                            }

                            // Oltre BASE_MAX accettiamo solo tool con usage_context.
                            if prompt_tools.len() >= BASE_MAX && usage_ctx_opt.is_none() {
                                continue;
                            }

                            prompt_tools.push(PromptMcpTool {
                                tool_name,
                                tool_server,
                                usage_context: usage_ctx_opt,
                            });

                            if prompt_tools.len() >= HARD_MAX {
                                break;
                            }
                        }

                        // Token saver “hard”: se il catalogo tool è grande, non assegnare tool specifici.
                        // Assegna solo i 2 meta-tool builtin che permettono discovery+call runtime
                        // (riduce enormemente il payload tools_json inviato al provider nei turni agente).
                        if tool_rows.len() >= 80 && !prompt_tools.is_empty() {
                            // Un server MCP deve esporre entrambi i meta-tool (ordine HashSet non deterministico).
                            let servers: std::collections::HashSet<String> =
                                tool_by_pair.iter().map(|(srv, _)| srv.clone()).collect();
                            let mut meta_server: Option<String> = None;
                            for srv in servers {
                                let search_pair =
                                    (srv.clone(), "nexus_mcp_tool_search".to_string());
                                let call_pair = (srv.clone(), "nexus_mcp_tool_call".to_string());
                                if tool_by_pair.contains(&search_pair)
                                    && tool_by_pair.contains(&call_pair)
                                {
                                    meta_server = Some(srv);
                                    break;
                                }
                            }
                            if let Some(srv) = meta_server {
                                prompt_tools = vec![
                                    PromptMcpTool {
                                        tool_name: "nexus_mcp_tool_search".to_string(),
                                        tool_server: srv.clone(),
                                        usage_context: Some("Cerca tool MCP disponibili solo quando servono (riduce token).".to_string()),
                                    },
                                    PromptMcpTool {
                                        tool_name: "nexus_mcp_tool_call".to_string(),
                                        tool_server: srv,
                                        usage_context: Some("Invoca un tool MCP specifico (server_id + tool_name).".to_string()),
                                    },
                                ];
                            }
                        }

                        let count = prompt_tools.len();
                        total_tools_selected += count;
                        let tools_json = serde_json::to_value(&prompt_tools).unwrap_or_default();
                        let _ = sqlx::query(
                            "UPDATE nexus_prompt_templates SET mcp_tools_json=$1, updated_at=NOW() WHERE key=$2"
                        )
                        .bind(tools_json)
                        .bind(&key)
                        .execute(&state.db)
                        .await;

                        if count > 0 {
                            assigned += 1;
                        }
                        results.push(
                            serde_json::json!({"key": &key, "tools_count": count, "status": "ok"}),
                        );
                    }
                    _ => {
                        errors += 1;
                        let snippet: String = raw.chars().take(80).collect();
                        tracing::warn!("batch_assign: parse error per {}: {:?}", &key, snippet);
                        results.push(serde_json::json!({"key": &key, "status": "parse_error"}));
                    }
                }
            }
            Err(e) => {
                errors += 1;
                tracing::warn!("batch_assign: tutti i provider falliti per {}: {}", &key, e);
                results.push(serde_json::json!({"key": &key, "status": "llm_error", "error": e}));
            }
        }
    }

    let avg_tools = if processed > 0 {
        (total_tools_selected as f64) / (processed as f64)
    } else {
        0.0
    };
    Ok(Json(serde_json::json!({
        "status": "completed",
        "processed": processed,
        "assigned": assigned,
        "skipped": processed - assigned - errors,
        "errors": errors,
        "avg_tools_per_template": avg_tools,
        "base_max_tools_per_template": 3,
        "hard_max_tools_per_template": 8,
        "results": results,
    })))
}

/// POST /api/admin/prompt-templates/batch-assign-tools
///
/// Endpoint "admin": avvia in background e ritorna subito (evita blocchi UI).
pub async fn batch_assign_tools_handler(
    State(state): State<crate::AppState>,
    Extension(claims): Extension<crate::auth::Claims>,
) -> impl IntoResponse {
    use std::sync::atomic::{AtomicBool, Ordering};

    static RUNNING: AtomicBool = AtomicBool::new(false);
    static PENDING: AtomicBool = AtomicBool::new(false);

    if RUNNING.swap(true, Ordering::SeqCst) {
        PENDING.store(true, Ordering::SeqCst);
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "queued": false,
                "running": true,
                "pending": true
            })),
        )
            .into_response();
    }

    let state_clone = state.clone();
    let claims_sub = claims.sub.clone();
    tokio::spawn(async move {
        // Billing: usa l'utente autenticato + progetto più recente (necessari per FK).
        let user_id = match uuid::Uuid::parse_str(&claims_sub) {
            Ok(u) => u,
            Err(_) => sqlx::query_scalar::<_, uuid::Uuid>(
                "SELECT id FROM users ORDER BY created_at DESC LIMIT 1",
            )
            .fetch_one(&state_clone.db)
            .await
            .unwrap_or_else(|_| {
                tracing::error!(
                    "batch: impossibile risolvere user_id per billing (claims non UUID e nessun utente?)"
                );
                uuid::Uuid::nil()
            }),
        };

        let project_id = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT id FROM projects ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(&state_clone.db)
        .await
        .unwrap_or_else(|_| {
            tracing::error!(
                "batch: impossibile risolvere project_id per billing (nessun progetto?)"
            );
            uuid::Uuid::nil()
        });

        if user_id.is_nil() || project_id.is_nil() {
            // Senza FK valide non possiamo scrivere su ledger: abort job.
            RUNNING.store(false, Ordering::SeqCst);
            return;
        }

        let _ = batch_assign_tools_impl(State(state_clone), user_id, project_id).await;
        RUNNING.store(false, Ordering::SeqCst);
        if PENDING.swap(false, Ordering::SeqCst) {
            let state_clone2 = state.clone();
            RUNNING.store(true, Ordering::SeqCst);
            tokio::spawn(async move {
                let user_id2 = sqlx::query_scalar::<_, uuid::Uuid>(
                    "SELECT id FROM users ORDER BY created_at DESC LIMIT 1",
                )
                .fetch_one(&state_clone2.db)
                .await
                .unwrap_or(uuid::Uuid::nil());

                let project_id2 = sqlx::query_scalar::<_, uuid::Uuid>(
                    "SELECT id FROM projects ORDER BY created_at DESC LIMIT 1",
                )
                .fetch_one(&state_clone2.db)
                .await
                .unwrap_or(uuid::Uuid::nil());

                if !user_id2.is_nil() && !project_id2.is_nil() {
                    let _ =
                        batch_assign_tools_impl(State(state_clone2), user_id2, project_id2).await;
                }
                RUNNING.store(false, Ordering::SeqCst);
            });
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "queued": true,
            "running": true,
            "pending": false
        })),
    )
        .into_response()
}

/// POST /api/internal/prompt-templates/batch-assign-tools
///
/// Versione "internal" (no auth) della batch tool-assignment.
/// Usata da servizi interni (es. plugin-service) quando cambia il parco MCP
/// disponibile (install/disable/delete) e serve riallineare `mcp_tools_json`
/// su TUTTI i prompt template minimizzando i tool assegnati.
pub async fn internal_batch_assign_tools_handler(
    State(state): State<crate::AppState>,
) -> impl IntoResponse {
    // Evita che più trigger (create/delete/toggle/test MCP) lancino batch paralleli:
    // - se già in esecuzione, marca "pending" e ritorna subito
    // - al termine, se pending=true, rilancia una volta
    static RUNNING: AtomicBool = AtomicBool::new(false);
    static PENDING: AtomicBool = AtomicBool::new(false);

    if RUNNING.swap(true, Ordering::SeqCst) {
        PENDING.store(true, Ordering::SeqCst);
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "queued": false,
                "running": true,
                "pending": true
            })),
        )
            .into_response();
    }

    let state_clone = state.clone();
    tokio::spawn(async move {
        // Trigger interno: usa ultimo user/progetto come “contabilità di sistema”.
        let user_id = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT id FROM users ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&state_clone.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(uuid::Uuid::new_v4);

        let project_id = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT id FROM projects ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&state_clone.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(uuid::Uuid::new_v4);

        let _ = batch_assign_tools_impl(State(state_clone), user_id, project_id).await;
        RUNNING.store(false, Ordering::SeqCst);
        if PENDING.swap(false, Ordering::SeqCst) {
            // rilancia una sola volta (se arrivati altri trigger durante l'esecuzione)
            let state_clone2 = state.clone();
            RUNNING.store(true, Ordering::SeqCst);
            tokio::spawn(async move {
                let user_id2 = sqlx::query_scalar::<_, uuid::Uuid>(
                    "SELECT id FROM users ORDER BY created_at DESC LIMIT 1",
                )
                .fetch_optional(&state_clone2.db)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(uuid::Uuid::new_v4);

                let project_id2 = sqlx::query_scalar::<_, uuid::Uuid>(
                    "SELECT id FROM projects ORDER BY created_at DESC LIMIT 1",
                )
                .fetch_optional(&state_clone2.db)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(uuid::Uuid::new_v4);

                let _ = batch_assign_tools_impl(State(state_clone2), user_id2, project_id2).await;
                RUNNING.store(false, Ordering::SeqCst);
            });
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "queued": true,
            "running": true,
            "pending": false
        })),
    )
        .into_response()
}
/// GET /api/admin/prompt-templates/:key/tools
pub async fn get_prompt_tools_handler(
    State(state): State<crate::AppState>,
    axum::extract::Path(key): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let row = sqlx::query("SELECT mcp_tools_json FROM nexus_prompt_templates WHERE key = $1")
        .bind(&key)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
        })?;

    let assigned_tools: Vec<PromptMcpTool> = row
        .and_then(|r| r.try_get::<serde_json::Value, _>("mcp_tools_json").ok())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    let available_rows = sqlx::query(
        r#"SELECT mcp_tools.tool_name as name, mcp_servers.name as server, mcp_tools.description
           FROM mcp_server_tools as mcp_tools
           JOIN mcp_servers ON mcp_tools.server_id = mcp_servers.id
           WHERE mcp_servers.enabled = true
           ORDER BY mcp_servers.name, mcp_tools.tool_name"#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let available_tools: Vec<serde_json::Value> = available_rows
        .iter()
        .map(|r| {
            let name: String = r.get("name");
            let server: String = r.get("server");
            let desc: Option<String> = r.try_get("description").ok().flatten();
            serde_json::json!({ "name": name, "server": server, "description": desc })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "assigned_tools": assigned_tools,
        "suggested_tools": [],
        "available_tools": available_tools,
    })))
}

/// PUT /api/admin/prompt-templates/:key/tools
pub async fn update_prompt_tools_handler(
    State(state): State<crate::AppState>,
    axum::extract::Path(key): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let tools = body
        .get("assigned_tools")
        .cloned()
        .unwrap_or(serde_json::json!([]));
    sqlx::query(
        "UPDATE nexus_prompt_templates SET mcp_tools_json = $1, updated_at = NOW() WHERE key = $2",
    )
    .bind(&tools)
    .bind(&key)
    .execute(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
    })?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// GET /api/admin/available-mcp-tools
pub async fn available_mcp_tools_handler(
    State(state): State<crate::AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let rows = sqlx::query(
        r#"SELECT mcp_tools.tool_name as name, mcp_servers.name as server, mcp_tools.description
           FROM mcp_server_tools as mcp_tools
           JOIN mcp_servers ON mcp_tools.server_id = mcp_servers.id
           WHERE mcp_servers.enabled = true
           ORDER BY mcp_servers.name, mcp_tools.tool_name"#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let tools: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let name: String = r.get("name");
            let server: String = r.get("server");
            let desc: Option<String> = r.try_get("description").ok().flatten();
            serde_json::json!({ "name": name, "server": server, "description": desc })
        })
        .collect();

    Ok(Json(serde_json::json!(tools)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_cache_get_miss_restituisce_none() {
        let cache = TemplateCache::new();
        assert!(cache.get("chiave_assente").is_none());
    }

    #[test]
    fn template_cache_set_e_get() {
        let cache = TemplateCache::new();
        cache.set("k".to_string(), "valore".to_string());
        assert_eq!(cache.get("k"), Some("valore".to_string()));
    }

    #[test]
    fn template_cache_invalidate() {
        let cache = TemplateCache::new();
        cache.set("k".to_string(), "v".to_string());
        cache.invalidate("k");
        assert!(cache.get("k").is_none());
    }
}
