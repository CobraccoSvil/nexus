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

/// Sentinel e chiusura del blocco direttiva del consiglio (mig 0549), per lo strip
/// deterministico quando il task e' sotto la soglia di complessita'.
const COUNCIL_DIRECTIVE_SENTINEL: &str = "<!-- 0549:council_advisory -->";
const COUNCIL_DIRECTIVE_END: &str = "</consiglio_analisi>";

/// Rimuove il blocco `<consiglio_analisi>` (mig 0549) da un system prompt. PURA.
/// Se il blocco non c'e' (sentinel o chiusura assenti) ritorna il prompt invariato.
/// Toglie anche i newline che precedono il sentinel (l'append era `\n\n`), cosi'
/// non resta spaziatura orfana. PUNTO UNICO dello strip (regola L).
pub fn strip_council_directive(prompt: &str) -> String {
    let Some(start) = prompt.find(COUNCIL_DIRECTIVE_SENTINEL) else {
        return prompt.to_string();
    };
    let Some(end_rel) = prompt[start..].find(COUNCIL_DIRECTIVE_END) else {
        return prompt.to_string();
    };
    let end = start + end_rel + COUNCIL_DIRECTIVE_END.len();
    let head = prompt[..start].trim_end_matches('\n');
    let mut out = String::with_capacity(head.len() + prompt.len().saturating_sub(end));
    out.push_str(head);
    out.push_str(&prompt[end..]);
    out
}

/// Conta quante keyword di AMBITO SENSIBILE il testo tocca (case-insensitive,
/// substring). PURA. Il consiglio serve sui task a rischio di DOMINIO — auth/
/// sicurezza, pagamenti, schema/migrazioni DB, azioni distruttive, privacy — non
/// in base alla "quantita' di lavoro" (la metrica del gate precedente,
/// `estimate_prompt_complexity`, pesava keyword di AZIONE come build/deploy/
/// fullstack e mancava i task di sicurezza: un "aggiungi 2FA" otteneva score ~2).
/// PUNTO UNICO della detection d'ambito sensibile (regola L). Keyword vuote/spazi
/// ignorate.
pub fn count_sensitive_domain_hits(text: &str, keywords: &[String]) -> usize {
    let lower = text.to_lowercase();
    keywords
        .iter()
        .filter_map(|k| {
            let k = k.trim().to_lowercase();
            if k.is_empty() {
                None
            } else {
                Some(k)
            }
        })
        .filter(|k| lower.contains(k.as_str()))
        .count()
}

/// Gate ad AMBITO SENSIBILE del consiglio (regola G/H/L, DB-driven): il consiglio
/// si attiva quando il task tocca ambiti a rischio (auth/sicurezza, pagamenti,
/// schema/migrazioni DB, azioni distruttive, privacy), rilevati dalle keyword di
/// dominio in `orchestrator.council_trigger_keywords` (CSV). Se le keyword toccate
/// sono >= `orchestrator.council_min_trigger_hits` (default 1) la direttiva
/// <consiglio_analisi> resta; altrimenti viene RIMOSSA (task ordinario -> percorso
/// agentico diretto). Deterministico: keyword di DOMINIO, mai la "quantita' di
/// lavoro". Config assente (DB hiccup) -> fail-open verso il consiglio (meglio un
/// consiglio di troppo che perderne uno su un task sensibile). PUNTO UNICO del
/// gate; i call site che costruiscono il system prompt agentico delegano qui.
pub async fn gate_council_directive(db: &PgPool, prompt: String, user_text: &str) -> String {
    if council_triggered_for(db, user_text).await {
        prompt
    } else {
        strip_council_directive(&prompt)
    }
}

/// PUNTO UNICO (regola L) della decisione "il consiglio deve attivarsi per questo
/// testo?": legge le keyword d'ambito sensibile (`orchestrator.council_trigger_
/// keywords`, CSV) e la soglia (`orchestrator.council_min_trigger_hits`) e ritorna
/// `true` se il testo tocca >= soglia keyword. Delega la conta al puro
/// [`count_sensitive_domain_hits`]. Sia il gate della direttiva in-prompt
/// ([`gate_council_directive`]) sia l'ATTIVAZIONE programmatica del pre-step del
/// consiglio (`spawn_agent_run`) delegano qui: una sola definizione dell'ambito
/// sensibile. Fail-open: config assente (DB hiccup) -> `true` (meglio un consiglio di
/// troppo che perderne uno su un task sensibile).
pub async fn council_triggered_for(db: &PgPool, user_text: &str) -> bool {
    let keywords: Vec<String> =
        match nexus_auth::get_setting(db, "orchestrator.council_trigger_keywords").await {
            Some(csv) => csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            None => return true, // fail-open: nessuna config -> considera attivo.
        };
    if keywords.is_empty() {
        return true;
    }
    let min_hits = nexus_auth::get_setting(db, "orchestrator.council_min_trigger_hits")
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    count_sensitive_domain_hits(user_text, &keywords) >= min_hits
}

#[cfg(test)]
mod council_gate_tests {
    use super::{count_sensitive_domain_hits, strip_council_directive};

    #[test]
    fn count_hits_rileva_ambiti_sensibili_non_quantita_lavoro() {
        let kw = vec![
            "autenticazione".to_string(),
            "login".to_string(),
            "otp".to_string(),
            "sicurezza".to_string(),
            "pagamento".to_string(),
        ];
        // Il task 2FA del test UI: 4 ambiti sensibili toccati -> consiglio attivo.
        let task = "Aggiungi l'autenticazione a due fattori via email per il login \
                    con codice OTP e gestisci la sicurezza";
        assert_eq!(count_sensitive_domain_hits(task, &kw), 4);
        // Task banale: 0 hit -> niente consiglio.
        assert_eq!(
            count_sensitive_domain_hits("correggi il typo nel bottone salva", &kw),
            0
        );
        // Keyword vuote/spazi ignorate.
        assert_eq!(
            count_sensitive_domain_hits(
                "login",
                &["".to_string(), "  ".to_string(), "login".to_string()]
            ),
            1
        );
    }

    #[test]
    fn strip_rimuove_il_blocco_direttiva() {
        let p = "PROMPT BASE.\n\n<!-- 0549:council_advisory -->\n<consiglio_analisi>\nblah\n</consiglio_analisi>";
        let out = strip_council_directive(p);
        assert_eq!(out, "PROMPT BASE.");
        assert!(!out.contains("consiglio_analisi"));
    }

    #[test]
    fn strip_conserva_coda_dopo_il_blocco() {
        let p = "HEAD\n\n<!-- 0549:council_advisory -->\n<consiglio_analisi>\nX\n</consiglio_analisi>\n\nTAIL";
        assert_eq!(strip_council_directive(p), "HEAD\n\nTAIL");
    }

    #[test]
    fn strip_no_op_senza_blocco() {
        let p = "PROMPT senza direttiva";
        assert_eq!(strip_council_directive(p), p);
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

/// Aggiorna un template esistente: salva la history poi incrementa la versione.
/// Estratto da `upsert_template_handler` per mantenere l'handler sotto soglia
/// (comportamento invariato).
async fn update_existing_template(
    db: &PgPool,
    cur_id: i32,
    key: &str,
    req: &UpsertTemplateReq,
    updated_by: &str,
) -> Result<PromptTemplate, StatusCode> {
    // Save history
    let _ = sqlx::query(
        "INSERT INTO nexus_prompt_template_history (template_id, content, version, changed_by, change_note) SELECT id, content, version, $2, $3 FROM nexus_prompt_templates WHERE id = $1"
    )
    .bind(cur_id)
    .bind(updated_by)
    .bind(&req.change_note)
    .execute(db)
    .await;

    // Update
    sqlx::query_as::<_, PromptTemplate>(
        "UPDATE nexus_prompt_templates SET content=$1, version=version+1, updated_by=$2, updated_at=NOW(), title=COALESCE($3, title) WHERE key=$4 RETURNING id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context"
    )
    .bind(&req.content)
    .bind(updated_by)
    .bind(&req.title)
    .bind(key)
    .fetch_one(db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Inserisce un nuovo template (categoria 'system'). Estratto da
/// `upsert_template_handler` (comportamento invariato).
async fn insert_new_template(
    db: &PgPool,
    key: &str,
    req: UpsertTemplateReq,
    updated_by: &str,
) -> Result<PromptTemplate, StatusCode> {
    sqlx::query_as::<_, PromptTemplate>(
        "INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES ($1, 'system', $2, $3, $4) RETURNING id, key, category, title, content, is_active, version, updated_by, updated_at, usage_context"
    )
    .bind(key)
    .bind(req.title.unwrap_or_else(|| key.to_string()))
    .bind(&req.content)
    .bind(updated_by)
    .fetch_one(db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// PUT /api/prompt-templates/:key
pub async fn upsert_template_handler(
    State(state): State<crate::AppState>,
    Path(key): Path<String>,
    Json(req): Json<UpsertTemplateReq>,
) -> Result<Json<PromptTemplate>, StatusCode> {
    let updated_by = req.updated_by.clone().unwrap_or_else(|| "user".to_string());

    // Get current version
    let current = sqlx::query("SELECT id, version FROM nexus_prompt_templates WHERE key = $1")
        .bind(&key)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|row| (row.get::<i32, _>("id"), row.get::<i32, _>("version")));

    let template = if let Some((cur_id, _)) = current {
        update_existing_template(&state.db, cur_id, &key, &req, &updated_by).await?
    } else {
        insert_new_template(&state.db, &key, req, &updated_by).await?
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

/// Persiste la suggestion nexus nella history del template. Estratto da
/// `generate_nexus_suggestion` per tenere la funzione sotto soglia
/// (comportamento invariato).
async fn persist_nexus_suggestion(
    db: &PgPool,
    tmpl_id: i32,
    tmpl_content: &str,
    examples_text: &[String],
) -> anyhow::Result<()> {
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

    persist_nexus_suggestion(db, tmpl_id, &tmpl_content, &examples_text).await
}

/// Vero se il contenuto della risposta segnala esaurimento quota/credito/rate.
/// NB: euristica testuale storica (comportamento invariato); mantenuta identica
/// nell'estrazione da `generate_with_admin_fallback`. La classificazione
/// strutturata alla fonte (regola M) resta un miglioramento fuori scope qui.
fn response_signals_exhausted(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.contains("credit balance")
        || lower.contains("too low")
        || lower.contains("quota")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("529")
        || lower.contains("overloaded")
        || (lower.contains("429") && lower.contains("exceeded"))
}

/// Carica e filtra l'ordine provider dall'admin (settings.provider_hierarchy),
/// escludendo quelli gia' marcati broken. Estratto da
/// `generate_with_admin_fallback` (comportamento invariato).
async fn load_admin_provider_order(
    db: &PgPool,
    broken_providers: &std::collections::HashSet<String>,
) -> Result<Vec<String>, String> {
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
    Ok(providers)
}

/// Esito di un singolo tentativo provider dentro `generate_with_admin_fallback`.
enum ProviderAttempt {
    /// Risposta valida da restituire subito.
    Ok(serde_json::Value),
    /// Provider da marcare broken e saltare (esaurito o errore).
    Broken,
}

/// Esegue un singolo tentativo su un provider: riserva billing, chiama il
/// modello, finalizza/rilascia il costo e classifica l'esito. Estratto da
/// `generate_with_admin_fallback` (comportamento invariato).
/// Finalizza il costo di un tentativo batch riuscito con i token reali (o la
/// stima). No-op se non c'era riserva. Estratto da `try_provider_once`
/// (comportamento invariato).
async fn finalize_batch_usage(
    db: &PgPool,
    reservation: &Option<crate::billing::UsageReservation>,
    v: &serde_json::Value,
    prompt_tokens: i32,
    estimated_completion_tokens: i32,
) {
    // Finalizza il costo con i token reali (se presenti), altrimenti usa stima.
    let usage_numbers =
        crate::billing::extract_usage_numbers(v, prompt_tokens, estimated_completion_tokens);
    if let Some(res) = reservation {
        if let Err(e) =
            crate::billing::finalize_usage(db, res, uuid::Uuid::new_v4(), &usage_numbers).await
        {
            tracing::error!("batch: billing finalize FAILED: {e}");
        }
    }
}

/// Riserva il billing per un tentativo batch (best-effort: `None` se fallisce,
/// il tentativo prosegue comunque). Estratto da `try_provider_once`
/// (comportamento invariato).
async fn reserve_batch_usage(
    db: &PgPool,
    billing_user_id: uuid::Uuid,
    billing_project_id: uuid::Uuid,
    provider: &str,
    model: &str,
    prompt_tokens: i32,
    estimated_completion_tokens: i32,
) -> Option<crate::billing::UsageReservation> {
    match crate::billing::reserve_usage(
        db,
        billing_user_id,
        billing_project_id,
        provider,
        model,
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
    }
}

async fn try_provider_once(
    neural: &crate::orchestrator::NeuralCoreClient,
    db: &PgPool,
    routing_matrix: &crate::routing_matrix::RoutingMatrix,
    prompt: &str,
    provider: &str,
    billing_user_id: uuid::Uuid,
    billing_project_id: uuid::Uuid,
) -> ProviderAttempt {
    use crate::orchestrator::default_model_for_provider;

    let model = default_model_for_provider(routing_matrix, provider);
    // Billing: riserva prima di chiamare il provider, finalizza dopo.
    // Nota: qui non abbiamo un token_budget esplicito; stimiamo un upper bound.
    let prompt_tokens = mcp_token::count_tokens(prompt) as i32;
    let estimated_completion_tokens = 800i32;
    let reservation = reserve_batch_usage(
        db,
        billing_user_id,
        billing_project_id,
        provider,
        &model,
        prompt_tokens,
        estimated_completion_tokens,
    )
    .await;

    match neural.generate_completion(provider, &model, prompt).await {
        Ok(v) => {
            finalize_batch_usage(
                db,
                &reservation,
                &v,
                prompt_tokens,
                estimated_completion_tokens,
            )
            .await;
            classify_successful_generation(v, provider)
        }
        Err(e) => {
            // In caso di errore, rilascia la riserva (non conteggiare).
            if let Some(res) = &reservation {
                crate::billing::release_usage(db, res, "provider_error").await;
            }
            tracing::warn!(
                "batch: provider {} errore gRPC: {}, marcato broken",
                provider,
                e
            );
            ProviderAttempt::Broken
        }
    }
}

/// Classifica una risposta LLM riuscita: `Broken` se segnala esaurimento
/// quota/credito/rate, altrimenti `Ok`. Estratto da `try_provider_once`
/// (comportamento invariato).
fn classify_successful_generation(v: serde_json::Value, provider: &str) -> ProviderAttempt {
    let content = v["content"].as_str().unwrap_or("");
    // Controlla se la risposta è un errore di quota/credito/rate limit
    if response_signals_exhausted(content) {
        tracing::warn!(
            "batch: provider {} esaurito/rate-limited → marcato broken per il resto del batch",
            provider
        );
        return ProviderAttempt::Broken;
    }
    tracing::debug!("batch: provider {} OK", provider);
    ProviderAttempt::Ok(v)
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
    // Carica l'ordine provider dall'admin (stesso campo usato dall'agent loop)
    let providers = load_admin_provider_order(db, broken_providers).await?;

    for provider in &providers {
        match try_provider_once(
            neural,
            db,
            routing_matrix,
            prompt,
            provider,
            billing_user_id,
            billing_project_id,
        )
        .await
        {
            ProviderAttempt::Ok(v) => return Ok(v),
            ProviderAttempt::Broken => {
                // Marca come broken: non verrà ritentato nei template successivi
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

/// Risolve provider/model per la ai-suggest: default dal PUNTO UNICO tier-only
/// (regola L/G), con override opzionale dai campi della richiesta. Estratto da
/// `ai_suggest_handler` (comportamento invariato).
async fn resolve_ai_suggest_provider_model(
    state: &crate::AppState,
    req: &AiSuggestReq,
) -> Result<(String, String), (StatusCode, Json<serde_json::Value>)> {
    let (purpose_provider, purpose_model) =
        crate::internal_routing::resolve_purpose_model(state, "admin_fallback_default")
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
    Ok((provider, model))
}

/// Costruisce il meta-prompt (system.ai_suggest_meta_prompt, mig 0445), chiama
/// l'LLM e restituisce la suggestion ripulita. Estratto da `ai_suggest_handler`
/// (comportamento invariato).
async fn generate_ai_suggestion_text(
    state: &crate::AppState,
    template: &PromptTemplate,
    req: &AiSuggestReq,
    provider: &str,
    model: &str,
) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    let usage_ctx = template
        .usage_context
        .as_deref()
        .unwrap_or("(nessun contesto d'uso documentato per questo prompt)");

    // Meta-prompt dal DB (system.ai_suggest_meta_prompt, mig 0445); fallback al
    // default builtin se DB down. {{content}} per ultimo: il content puo'
    // contenere placeholder e non va corrotto dai replace dei metadati.
    let meta_prompt = get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.ai_suggest_meta_prompt",
    )
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
        .generate_completion(provider, model, &meta_prompt)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
        })?;

    Ok(result["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_string())
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

    let (provider, model) = resolve_ai_suggest_provider_model(&state, &req).await?;

    let suggestion =
        generate_ai_suggestion_text(&state, &template, &req, &provider, &model).await?;

    // --- Tool suggestion automatica ---
    let suggested_tools =
        suggest_tools_for_template(&state, &key, &suggestion, &provider, &model).await;

    Ok(Json(serde_json::json!({
        "suggestion": suggestion,
        "provider": provider,
        "model": model,
        "suggested_tools": suggested_tools,
    })))
}

/// Carica i tool MCP dei server abilitati (nome, server, descrizione).
/// Punto unico della query ripetuta negli handler prompt-templates (regola L);
/// su errore ritorna una lista vuota (comportamento invariato).
async fn fetch_enabled_mcp_tools(db: &PgPool) -> Vec<sqlx::postgres::PgRow> {
    sqlx::query(
        r#"SELECT DISTINCT
            mcp_tools.tool_name as name,
            mcp_servers.name as server_name,
            mcp_tools.description
        FROM mcp_server_tools as mcp_tools
        JOIN mcp_servers ON mcp_tools.server_id = mcp_servers.id
        WHERE mcp_servers.enabled = true
        ORDER BY mcp_servers.name, mcp_tools.tool_name"#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// Estrae la lista di nomi tool da una risposta LLM che contiene un array JSON
/// (eventualmente incorniciato da prosa). Estratto da `suggest_tools_for_template`
/// (comportamento invariato).
fn parse_tool_names(tool_result: &serde_json::Value) -> Vec<String> {
    tool_result["content"]
        .as_str()
        .map(|s| {
            let s = s.trim();
            let start = s.find('[').unwrap_or(0);
            let end = s.rfind(']').map(|i| i + 1).unwrap_or(s.len());
            serde_json::from_str::<Vec<String>>(&s[start..end]).unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Persiste i tool assegnati su `mcp_tools_json` (no-op se vuoti). Estratto da
/// `suggest_tools_for_template` (comportamento invariato).
async fn persist_assigned_tools(db: &PgPool, key: &str, prompt_tools: &[PromptMcpTool]) {
    if prompt_tools.is_empty() {
        return;
    }
    let tools_json = serde_json::to_value(prompt_tools).unwrap_or_default();
    let _ = sqlx::query(
        "UPDATE nexus_prompt_templates SET mcp_tools_json=$1, updated_at=NOW() WHERE key=$2",
    )
    .bind(tools_json)
    .bind(key)
    .execute(db)
    .await;
}

/// Formatta le righe tool in una lista "- name: desc" (una per riga). Estratto
/// da `suggest_tools_for_template` (comportamento invariato).
fn format_tools_list(rows: &[sqlx::postgres::PgRow]) -> String {
    rows.iter()
        .map(|r| {
            let name: String = r.get("name");
            let desc: Option<String> = r.try_get("description").ok().flatten();
            format!("- {}: {}", name, desc.as_deref().unwrap_or(""))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mappa i nomi tool selezionati sui rispettivi server noti, scartando quelli
/// non presenti. Estratto da `suggest_tools_for_template` (comportamento
/// invariato).
fn resolve_selected_tools(
    rows: &[sqlx::postgres::PgRow],
    tool_names: &[String],
) -> Vec<PromptMcpTool> {
    let tool_map: std::collections::HashMap<String, String> = rows
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("name"),
                r.get::<String, _>("server_name"),
            )
        })
        .collect();

    tool_names
        .iter()
        .filter_map(|name| {
            tool_map.get(name).map(|server| PromptMcpTool {
                tool_name: name.clone(),
                tool_server: server.clone(),
                usage_context: None,
            })
        })
        .collect()
}

/// Seleziona via LLM i tool MCP piu' adatti al contenuto suggerito e li
/// persiste su `mcp_tools_json`. Estratto da `ai_suggest_handler`
/// (comportamento invariato).
async fn suggest_tools_for_template(
    state: &crate::AppState,
    key: &str,
    suggestion: &str,
    provider: &str,
    model: &str,
) -> Vec<PromptMcpTool> {
    let rows = fetch_enabled_mcp_tools(&state.db).await;

    let tools_list = format_tools_list(&rows);

    if tools_list.is_empty() {
        return vec![];
    }

    let tool_prompt = get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.tool_selection_single_prompt",
    )
    .await
    .replace("{{tools_list}}", &tools_list)
    .replace("{{content}}", suggestion);

    // Usa lo stesso provider/model della richiesta principale per la tool suggestion
    let tool_result = state
        .orchestrator
        .neural
        .generate_completion(provider, model, &tool_prompt)
        .await
        .unwrap_or_default();

    let tool_names = parse_tool_names(&tool_result);
    let prompt_tools = resolve_selected_tools(&rows, &tool_names);

    persist_assigned_tools(&state.db, key, &prompt_tools).await;

    prompt_tools
}

/// Catalogo dei tool MCP abilitati con i lookup usati dal batch: lista testuale
/// disambiguata, coppie (server, tool) e mappa tool_name -> set(server).
/// Estratto da `batch_assign_tools_impl` (comportamento invariato).
struct ToolCatalog {
    /// Lista minima "- server::tool" per il prompt.
    tools_list: String,
    /// Coppie (server, tool_name) esistenti.
    by_pair: std::collections::HashSet<(String, String)>,
    /// tool_name -> insieme dei server che lo espongono (disambiguazione).
    servers_by_name: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Numero di tool nel catalogo (per la soglia token-saver).
    total: usize,
}

/// Inserisce il marker "job started" sul ledger. Best-effort: non blocca il job
/// se fallisce. Estratto da `batch_assign_tools_impl` (comportamento invariato).
async fn insert_batch_job_marker(
    db: &PgPool,
    billing_user_id: uuid::Uuid,
    billing_project_id: uuid::Uuid,
) {
    // Marker “hard” per rendere verificabile che il job è partito e che scrive sul DB giusto.
    // Non dipende da orchestrator_runs/run_id e non blocca il job se fallisce.
    //
    // La currency viene dal punto unico (regola G): era hardcoded a 'EUR' mentre la
    // piattaforma e' su USD — quarto scrittore del ledger, e l'unico rimasto a
    // dichiarare una valuta di propria iniziativa. Riga a costo 0, quindi la valuta
    // e' vacua, ma "un solo punto per la currency" o e' vero o non lo e'.
    let marker_id = uuid::Uuid::new_v4();
    let currency = match nexus_pricing::platform_currency(db).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("batch_assign_tools: marker insert saltato, currency non configurata: {e}");
            return;
        }
    };
    if let Err(e) = sqlx::query(
        r#"
        INSERT INTO ai_usage_ledger (
            id, user_id, project_id, provider, model,
            prompt_tokens, completion_tokens, total_tokens,
            input_cost, output_cost, total_cost, currency,
            status, details
        ) VALUES ($1, $2, $3, 'internal', 'batch_assign_tools_job', 0, 0, 0, 0, 0, 0, $5, 'reserved', $4)
        "#,
    )
    .bind(marker_id)
    .bind(billing_user_id)
    .bind(billing_project_id)
    .bind(serde_json::json!({
        "feature": "batch_assign_tools",
        "event": "job_started",
    }))
    .bind(&currency)
    .execute(db)
    .await
    {
        tracing::error!("batch_assign_tools: marker insert FAILED: {e}");
    } else {
        tracing::info!("batch_assign_tools: marker inserted ledger_id={marker_id}");
    }
}

/// Costruisce `ToolCatalog` dalle righe tool. Estratto da
/// `batch_assign_tools_impl` (comportamento invariato).
fn build_tool_catalog(tool_rows: &[sqlx::postgres::PgRow]) -> ToolCatalog {
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
    let mut by_pair: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut servers_by_name: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for r in tool_rows {
        let name: String = r.get("name");
        let server: String = r.get("server_name");
        by_pair.insert((server.clone(), name.clone()));
        servers_by_name.entry(name).or_default().insert(server);
    }

    ToolCatalog {
        tools_list,
        by_pair,
        servers_by_name,
        total: tool_rows.len(),
    }
}

/// Item grezzo estratto da un elemento dell'array LLM: nome tool, server
/// (opzionale) e usage_context (opzionale). Estratto da `batch_assign_tools_impl`.
struct ParsedToolItem {
    name: Option<String>,
    server: Option<String>,
    usage_ctx: Option<String>,
}

/// Interpreta un singolo elemento JSON della selezione tool (stringa
/// "tool"/"server::tool" oppure oggetto {tool_name, tool_server?, usage_context?}).
/// Estratto da `batch_assign_tools_impl` (comportamento invariato).
fn parse_tool_selection_item(item: &serde_json::Value) -> ParsedToolItem {
    // 1) String: "tool" oppure "server::tool"
    // 2) Object: {tool_name, tool_server?, usage_context?}
    let mut name: Option<String> = None;
    let mut server: Option<String> = None;
    let mut usage_ctx: Option<String> = None;

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
        usage_ctx = obj
            .get("usage_context")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
    }

    ParsedToolItem {
        name,
        server,
        usage_ctx,
    }
}

/// Risolve il server per un tool selezionato: usa quello esplicito, oppure
/// l'unico server che espone il tool_name. `None` se ambiguo o inesistente.
/// Estratto da `batch_assign_tools_impl` (comportamento invariato).
fn resolve_tool_server(
    catalog: &ToolCatalog,
    tool_name: &str,
    server: Option<String>,
) -> Option<String> {
    // Se server non specificato:
    // - accetta solo se il tool_name è univoco tra i server
    // - se ambiguo, richiede "server::tool" o tool_server nel JSON.
    if let Some(srv) = server {
        return Some(srv);
    }
    match catalog.servers_by_name.get(tool_name) {
        Some(s) if s.len() == 1 => s.iter().next().cloned(),
        _ => None, // ambiguo o inesistente
    }
}

/// Costruisce la lista di tool validi da un array JSON, applicando dedup e i
/// limiti BASE_MAX/HARD_MAX. Estratto da `batch_assign_tools_impl`
/// (comportamento invariato).
fn select_tools_from_array(
    arr: &[serde_json::Value],
    catalog: &ToolCatalog,
    base_max: usize,
    hard_max: usize,
) -> Vec<PromptMcpTool> {
    // Parsing robusto:
    // - accetta ["tool_name", ...]
    // - accetta [{tool_name, usage_context?}, ...]
    // - ignora tool non presenti in catalog
    // - de-duplica
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prompt_tools: Vec<PromptMcpTool> = Vec::new();

    for item in arr {
        let parsed = parse_tool_selection_item(item);
        let Some(tool_name) = parsed.name else {
            continue;
        };
        let Some(tool_server) = resolve_tool_server(catalog, &tool_name, parsed.server) else {
            continue;
        };

        if !catalog
            .by_pair
            .contains(&(tool_server.clone(), tool_name.clone()))
        {
            continue;
        }

        let dedup_key = format!("{}::{}", tool_server, tool_name);
        if !seen.insert(dedup_key) {
            continue;
        }

        // Oltre BASE_MAX accettiamo solo tool con usage_context.
        if prompt_tools.len() >= base_max && parsed.usage_ctx.is_none() {
            continue;
        }

        prompt_tools.push(PromptMcpTool {
            tool_name,
            tool_server,
            usage_context: parsed.usage_ctx,
        });

        if prompt_tools.len() >= hard_max {
            break;
        }
    }

    prompt_tools
}

/// Token saver "hard": con catalogo grande (>=80 tool) rimpiazza la selezione
/// specifica con i 2 meta-tool builtin di discovery/call, se un server li espone
/// entrambi. Estratto da `batch_assign_tools_impl` (comportamento invariato).
fn maybe_collapse_to_meta_tools(
    prompt_tools: Vec<PromptMcpTool>,
    catalog: &ToolCatalog,
) -> Vec<PromptMcpTool> {
    // Token saver “hard”: se il catalogo tool è grande, non assegnare tool specifici.
    // Assegna solo i 2 meta-tool builtin che permettono discovery+call runtime
    // (riduce enormemente il payload tools_json inviato al provider nei turni agente).
    if catalog.total < 80 || prompt_tools.is_empty() {
        return prompt_tools;
    }
    // Un server MCP deve esporre entrambi i meta-tool (ordine HashSet non deterministico).
    let servers: std::collections::HashSet<String> =
        catalog.by_pair.iter().map(|(srv, _)| srv.clone()).collect();
    let mut meta_server: Option<String> = None;
    for srv in servers {
        let search_pair = (srv.clone(), "nexus_mcp_tool_search".to_string());
        let call_pair = (srv.clone(), "nexus_mcp_tool_call".to_string());
        if catalog.by_pair.contains(&search_pair) && catalog.by_pair.contains(&call_pair) {
            meta_server = Some(srv);
            break;
        }
    }
    let Some(srv) = meta_server else {
        return prompt_tools;
    };
    vec![
        PromptMcpTool {
            tool_name: "nexus_mcp_tool_search".to_string(),
            tool_server: srv.clone(),
            usage_context: Some(
                "Cerca tool MCP disponibili solo quando servono (riduce token).".to_string(),
            ),
        },
        PromptMcpTool {
            tool_name: "nexus_mcp_tool_call".to_string(),
            tool_server: srv,
            usage_context: Some(
                "Invoca un tool MCP specifico (server_id + tool_name).".to_string(),
            ),
        },
    ]
}

/// Esito dell'elaborazione della risposta LLM per un singolo template.
struct TemplateOutcome {
    tools_selected: usize,
    assigned: bool,
    errored: bool,
    result: serde_json::Value,
}

impl TemplateOutcome {
    /// Esito di errore (nessun tool assegnato). Il `result` porta lo `status`
    /// e l'eventuale messaggio d'errore.
    fn error(result: serde_json::Value) -> Self {
        TemplateOutcome {
            tools_selected: 0,
            assigned: false,
            errored: true,
            result,
        }
    }
}

/// Estrae l'array JSON incorniciato da eventuale prosa nel testo grezzo.
/// `None` se il contenuto non e' un array JSON valido. Estratto da
/// `process_template_response` (comportamento invariato).
fn extract_json_array(raw: &str) -> Option<Vec<serde_json::Value>> {
    let start = raw.find('[').unwrap_or(0);
    let end = raw.rfind(']').map(|i| i + 1).unwrap_or(raw.len());
    let json_slice = &raw[start..end];
    // Match con if-let invece di guardia is_array() + unwrap successivo.
    match serde_json::from_str::<serde_json::Value>(json_slice) {
        Ok(serde_json::Value::Array(arr)) => Some(arr),
        _ => None,
    }
}

/// Elabora la risposta `generate_with_admin_fallback` per un template: parsa
/// l'array, seleziona i tool, applica il token-saver e persiste. Estratto da
/// `batch_assign_tools_impl` (comportamento invariato).
async fn process_template_response(
    db: &PgPool,
    key: &str,
    catalog: &ToolCatalog,
    base_max: usize,
    hard_max: usize,
    tool_result: Result<serde_json::Value, String>,
) -> TemplateOutcome {
    let v = match tool_result {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("batch_assign: tutti i provider falliti per {}: {}", key, e);
            return TemplateOutcome::error(
                serde_json::json!({"key": key, "status": "llm_error", "error": e}),
            );
        }
    };

    let raw = v["content"].as_str().unwrap_or("[]");
    let Some(arr) = extract_json_array(raw) else {
        let snippet: String = raw.chars().take(80).collect();
        tracing::warn!("batch_assign: parse error per {}: {:?}", key, snippet);
        return TemplateOutcome::error(serde_json::json!({"key": key, "status": "parse_error"}));
    };

    let prompt_tools = select_tools_from_array(&arr, catalog, base_max, hard_max);
    let prompt_tools = maybe_collapse_to_meta_tools(prompt_tools, catalog);

    let count = prompt_tools.len();
    persist_assigned_tools(db, key, &prompt_tools).await;

    TemplateOutcome {
        tools_selected: count,
        assigned: count > 0,
        errored: false,
        result: serde_json::json!({"key": key, "tools_count": count, "status": "ok"}),
    }
}

// Limiti adattivi per l'assegnazione tool per template:
// - fino a BASE_MAX accettiamo anche tool senza "usage_context"
// - oltre BASE_MAX, accettiamo solo tool con usage_context (giustificazione) per evitare over-assign.
// - HARD_MAX resta un limite di sicurezza.
const BATCH_BASE_MAX: usize = 3;
const BATCH_HARD_MAX: usize = 8;

/// Elabora un singolo template: costruisce il prompt, seleziona il provider,
/// genera e persiste i tool. `None` se la routing matrix non e' disponibile
/// (template saltato). Estratto da `batch_assign_tools_impl` (comportamento
/// invariato).
/// Costruisce il meta-prompt di assegnazione tool per un template
/// (system.batch_tool_assignment_prompt, mig 0445). {{role}} per ultimo:
/// l'estratto del prompt template puo' contenere placeholder. Estratto da
/// `assign_tools_for_one_template` (comportamento invariato).
async fn build_batch_tool_prompt(
    state: &crate::AppState,
    key: &str,
    title: &str,
    category: &str,
    content_preview: &str,
    catalog: &ToolCatalog,
) -> String {
    get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.batch_tool_assignment_prompt",
    )
    .await
    .replace("{{key}}", key)
    .replace("{{title}}", title)
    .replace("{{category}}", category)
    .replace("{{tools_list}}", &catalog.tools_list)
    .replace("{{base_max}}", &BATCH_BASE_MAX.to_string())
    .replace("{{hard_max}}", &BATCH_HARD_MAX.to_string())
    .replace("{{role}}", content_preview)
}

async fn assign_tools_for_one_template(
    state: &crate::AppState,
    row: &sqlx::postgres::PgRow,
    catalog: &ToolCatalog,
    broken_providers: &mut std::collections::HashSet<String>,
    billing_user_id: uuid::Uuid,
    billing_project_id: uuid::Uuid,
) -> Option<TemplateOutcome> {
    let key: String = row.get("key");
    let title: String = row.get("title");
    let category: String = row.get("category");
    let content_text: String = row.get("content");
    let content_preview: String = content_text.chars().take(400).collect();

    let tool_prompt =
        build_batch_tool_prompt(state, &key, &title, &category, &content_preview, catalog).await;

    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let matrix_arc = match state.orchestrator.routing_matrix.current_async().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(
                "regenerate_tool_suggestions: routing_matrix non disponibile ({e}), skip"
            );
            return None;
        }
    };
    let tool_result = generate_with_admin_fallback(
        &state.orchestrator.neural,
        &state.db,
        &matrix_arc,
        &tool_prompt,
        broken_providers,
        billing_user_id,
        billing_project_id,
    )
    .await;

    Some(
        process_template_response(
            &state.db,
            &key,
            catalog,
            BATCH_BASE_MAX,
            BATCH_HARD_MAX,
            tool_result,
        )
        .await,
    )
}

/// Accumulatore delle statistiche del batch: contatori + risultati per-template.
/// Estratto da `batch_assign_tools_impl` (comportamento invariato).
#[derive(Default)]
struct BatchStats {
    processed: usize,
    assigned: usize,
    errors: usize,
    total_tools_selected: usize,
    results: Vec<serde_json::Value>,
}

impl BatchStats {
    /// Registra l'esito di un template elaborato.
    fn record(&mut self, outcome: TemplateOutcome) {
        self.total_tools_selected += outcome.tools_selected;
        if outcome.assigned {
            self.assigned += 1;
        }
        if outcome.errored {
            self.errors += 1;
        }
        self.results.push(outcome.result);
    }

    /// Costruisce il JSON riassuntivo finale del batch.
    fn into_summary(self) -> Json<serde_json::Value> {
        let avg_tools = if self.processed > 0 {
            (self.total_tools_selected as f64) / (self.processed as f64)
        } else {
            0.0
        };
        Json(serde_json::json!({
            "status": "completed",
            "processed": self.processed,
            "assigned": self.assigned,
            "skipped": self.processed - self.assigned - self.errors,
            "errors": self.errors,
            "avg_tools_per_template": avg_tools,
            "base_max_tools_per_template": 3,
            "hard_max_tools_per_template": 8,
            "results": self.results,
        }))
    }
}

async fn batch_assign_tools_impl(
    State(state): State<crate::AppState>,
    billing_user_id: uuid::Uuid,
    billing_project_id: uuid::Uuid,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    insert_batch_job_marker(&state.db, billing_user_id, billing_project_id).await;

    let templates = sqlx::query(
        "SELECT key, title, content, category FROM nexus_prompt_templates WHERE is_active = true ORDER BY category, key",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))))?;

    // Carica TUTTI i tool MCP abilitati (inclusi server esterni gestiti) con descrizioni troncate.
    let tool_rows = fetch_enabled_mcp_tools(&state.db).await;

    if tool_rows.is_empty() {
        return Ok(Json(serde_json::json!({
            "processed": 0, "assigned": 0, "skipped": templates.len(), "errors": 0,
            "message": "Nessun tool MCP disponibile"
        })));
    }

    let catalog = build_tool_catalog(&tool_rows);

    // Set dei provider che hanno fallito - evita di ritentarli per ogni template
    let mut broken_providers: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stats = BatchStats::default();

    for row in &templates {
        stats.processed += 1;
        let Some(outcome) = assign_tools_for_one_template(
            &state,
            row,
            &catalog,
            &mut broken_providers,
            billing_user_id,
            billing_project_id,
        )
        .await
        else {
            continue;
        };
        stats.record(outcome);
    }

    Ok(stats.into_summary())
}

/// Risposta "gia' in esecuzione" per gli handler batch (HTTP 202). Punto unico
/// del payload duplicato tra admin e internal (regola L, comportamento invariato).
fn batch_already_running_response() -> axum::response::Response {
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "queued": false,
            "running": true,
            "pending": true
        })),
    )
        .into_response()
}

/// Risposta "job accodato" per gli handler batch (HTTP 202).
fn batch_queued_response() -> axum::response::Response {
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

/// Risolve user_id/project_id per il primo run "admin": preferisce il subject
/// dei claims, poi l'ultimo utente/progetto; su fallimento logga e ritorna nil.
/// Estratto da `batch_assign_tools_handler` (comportamento invariato).
async fn resolve_billing_ids_admin_first(
    db: &PgPool,
    claims_sub: &str,
) -> (uuid::Uuid, uuid::Uuid) {
    // Billing: usa l'utente autenticato + progetto più recente (necessari per FK).
    let user_id = match uuid::Uuid::parse_str(claims_sub) {
        Ok(u) => u,
        Err(_) => sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT id FROM users ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(db)
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
    .fetch_one(db)
    .await
    .unwrap_or_else(|_| {
        tracing::error!("batch: impossibile risolvere project_id per billing (nessun progetto?)");
        uuid::Uuid::nil()
    });

    (user_id, project_id)
}

/// Risolve user_id/project_id come ultimo utente/progetto, nil su fallimento
/// (nessun log). Usato dal re-run "admin". Comportamento invariato.
async fn resolve_billing_ids_latest_or_nil(db: &PgPool) -> (uuid::Uuid, uuid::Uuid) {
    let user_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM users ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(db)
    .await
    .unwrap_or(uuid::Uuid::nil());
    let project_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM projects ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(db)
    .await
    .unwrap_or(uuid::Uuid::nil());
    (user_id, project_id)
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
        return batch_already_running_response();
    }

    let state_clone = state.clone();
    let claims_sub = claims.sub.clone();
    tokio::spawn(async move {
        let (user_id, project_id) =
            resolve_billing_ids_admin_first(&state_clone.db, &claims_sub).await;

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
                let (user_id2, project_id2) =
                    resolve_billing_ids_latest_or_nil(&state_clone2.db).await;

                if !user_id2.is_nil() && !project_id2.is_nil() {
                    let _ =
                        batch_assign_tools_impl(State(state_clone2), user_id2, project_id2).await;
                }
                RUNNING.store(false, Ordering::SeqCst);
            });
        }
    });

    batch_queued_response()
}

/// POST /api/internal/prompt-templates/batch-assign-tools
///
/// Versione "internal" (no auth) della batch tool-assignment.
/// Usata da servizi interni (es. plugin-service) quando cambia il parco MCP
/// disponibile (install/disable/delete) e serve riallineare `mcp_tools_json`
/// su TUTTI i prompt template minimizzando i tool assegnati.
/// Risolve user_id/project_id per i trigger interni: ultimo utente/progetto,
/// con un UUID nuovo come "contabilità di sistema" se assenti. Estratto da
/// `internal_batch_assign_tools_handler` (comportamento invariato).
async fn resolve_billing_ids_internal(db: &PgPool) -> (uuid::Uuid, uuid::Uuid) {
    // Trigger interno: usa ultimo user/progetto come “contabilità di sistema”.
    let user_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM users ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(uuid::Uuid::new_v4);

    let project_id = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM projects ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(uuid::Uuid::new_v4);

    (user_id, project_id)
}

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
        return batch_already_running_response();
    }

    let state_clone = state.clone();
    tokio::spawn(async move {
        let (user_id, project_id) = resolve_billing_ids_internal(&state_clone.db).await;

        let _ = batch_assign_tools_impl(State(state_clone), user_id, project_id).await;
        RUNNING.store(false, Ordering::SeqCst);
        if PENDING.swap(false, Ordering::SeqCst) {
            // rilancia una sola volta (se arrivati altri trigger durante l'esecuzione)
            let state_clone2 = state.clone();
            RUNNING.store(true, Ordering::SeqCst);
            tokio::spawn(async move {
                let (user_id2, project_id2) = resolve_billing_ids_internal(&state_clone2.db).await;

                let _ = batch_assign_tools_impl(State(state_clone2), user_id2, project_id2).await;
                RUNNING.store(false, Ordering::SeqCst);
            });
        }
    });

    batch_queued_response()
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
