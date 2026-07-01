//! learned_instructions.rs — Livello 2 della continuita' (mig 0412).
//!
//! Distilla regole DURATURE di progetto (convenzioni, preferenze, ambiente)
//! dall'esperienza operativa e le materializza in `nexus_learned_instructions`,
//! da cui il brain le inietta SEMPRE nel system_text (provider-neutro). E'
//! l'analogo dell'auto-memory di Claude Code: mentre il worklog di sessione
//! (mig 0411) e' la storia operativa volatile, qui vivono le lezioni stabili
//! che evitano di ripetere errori attraverso sessioni e progetti.
//!
//! Fonti (deterministiche + episodiche): eventi worklog `error`/`failed_attempt`
//! ricorrenti + wiki_docs `chat_note`/`run_summary`. Il cursore per progetto
//! (`nexus_project_distill_state`) rende il worker idempotente: ogni evidenza e'
//! processata una volta sola, niente loop di spesa LLM.
//!
//! Punto unico di invocazione AI (regola L): `resolve_purpose_model` +
//! `neural.generate_agent_turn`, come `compact_session_core`. Modello e prompt
//! sono DB-driven (regola G/D). Niente payload nei log (regola F).

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::auth::Claims;
use crate::internal_routing::{resolve_purpose_model, PurposeResolution};
use crate::AppState;

const PURPOSE: &str = "learned_instructions_distill";
const DISTILL_TEMPLATE_KEY: &str = "agent.learned_instructions_distill";

// ───────────────────────────────────────────────────────────────────────────
// Settings DB-driven (cache 60s, pattern allineato a code_docs_enricher)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DistillerSettings {
    pub enabled: bool,
    pub interval_secs: u64,
    pub daily_cap: i64,
    pub min_new_signals: i64,
    pub evidence_max_items: i64,
    pub max_active_per_project: i64,
    pub auto_activate_confidence: f64,
}

impl DistillerSettings {
    fn safe_defaults() -> Self {
        Self {
            enabled: true,
            interval_secs: 900,
            daily_cap: 48,
            min_new_signals: 5,
            evidence_max_items: 40,
            max_active_per_project: 30,
            auto_activate_confidence: 0.8,
        }
    }
}

const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(60);

static SETTINGS_CACHE: once_cell::sync::Lazy<RwLock<Option<(DistillerSettings, Instant)>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

pub async fn current_settings(db: &PgPool) -> DistillerSettings {
    {
        let guard = SETTINGS_CACHE.read().await;
        if let Some((v, exp)) = guard.as_ref() {
            if Instant::now() < *exp {
                return v.clone();
            }
        }
    }
    let value = match load_settings(db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "learned_instructions: lettura settings fallita, safe_defaults");
            DistillerSettings::safe_defaults()
        }
    };
    let mut guard = SETTINGS_CACHE.write().await;
    *guard = Some((value.clone(), Instant::now() + SETTINGS_CACHE_TTL));
    value
}

async fn load_settings(db: &PgPool) -> Result<DistillerSettings> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ( \
            'agent.learned_instructions.distiller_enabled', \
            'agent.learned_instructions.distiller_interval_secs', \
            'agent.learned_instructions.daily_cap', \
            'agent.learned_instructions.min_new_signals', \
            'agent.learned_instructions.evidence_max_items', \
            'agent.learned_instructions.max_active_per_project', \
            'agent.learned_instructions.auto_activate_confidence' \
         )",
    )
    .fetch_all(db)
    .await
    .context("SELECT settings agent.learned_instructions.*")?;

    let mut out = DistillerSettings::safe_defaults();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        let raw = raw.trim();
        match key.as_str() {
            "agent.learned_instructions.distiller_enabled" => {
                out.enabled = matches!(raw.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
            }
            "agent.learned_instructions.distiller_interval_secs" => {
                if let Ok(v) = raw.parse::<u64>() {
                    out.interval_secs = v.max(30);
                }
            }
            "agent.learned_instructions.daily_cap" => {
                if let Ok(v) = raw.parse::<i64>() {
                    out.daily_cap = v.max(0);
                }
            }
            "agent.learned_instructions.min_new_signals" => {
                if let Ok(v) = raw.parse::<i64>() {
                    out.min_new_signals = v.max(1);
                }
            }
            "agent.learned_instructions.evidence_max_items" => {
                if let Ok(v) = raw.parse::<i64>() {
                    out.evidence_max_items = v.clamp(1, 200);
                }
            }
            "agent.learned_instructions.max_active_per_project" => {
                if let Ok(v) = raw.parse::<i64>() {
                    out.max_active_per_project = v.max(1);
                }
            }
            "agent.learned_instructions.auto_activate_confidence" => {
                if let Ok(v) = raw.parse::<f64>() {
                    out.auto_activate_confidence = v.clamp(0.0, 1.0);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Cap diurno globale (in-memory: il worker e' un singolo task; al restart si
// riazzera, accettabile per un limite di costo best-effort).
// ───────────────────────────────────────────────────────────────────────────

static DAILY_COUNT: once_cell::sync::Lazy<RwLock<(String, i64)>> =
    once_cell::sync::Lazy::new(|| RwLock::new((String::new(), 0)));

async fn daily_cap_reached(cap: i64, today: &str) -> bool {
    if cap <= 0 {
        return false;
    }
    let guard = DAILY_COUNT.read().await;
    guard.0 == today && guard.1 >= cap
}

async fn record_distill_call(today: &str) {
    let mut guard = DAILY_COUNT.write().await;
    if guard.0 != today {
        *guard = (today.to_string(), 0);
    }
    guard.1 += 1;
}

// ───────────────────────────────────────────────────────────────────────────
// Entry-point del worker
// ───────────────────────────────────────────────────────────────────────────

/// Avvia il loop in background. Delay iniziale 120s: lascia maturare il worklog
/// (i run terminati hanno gia' fatto ingest) prima di distillare.
pub fn spawn_learned_instructions_distiller(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(120)).await;
        let init = current_settings(&state.db).await;
        tracing::info!(
            enabled = init.enabled,
            interval_secs = init.interval_secs,
            daily_cap = init.daily_cap,
            "learned_instructions: distiller avviato"
        );
        loop {
            let settings = current_settings(&state.db).await;
            if !settings.enabled {
                tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
                continue;
            }
            match scan_and_distill(&state, &settings).await {
                Ok(n) if n > 0 => {
                    tracing::info!(projects = n, "learned_instructions: giro completato");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "learned_instructions: giro fallito");
                }
            }
            tokio::time::sleep(Duration::from_secs(settings.interval_secs)).await;
        }
    });
}

/// Seleziona i progetti con abbastanza nuovi segnali worklog oltre il cursore e
/// li distilla, rispettando il cap diurno. Ritorna quanti progetti distillati.
async fn scan_and_distill(state: &AppState, settings: &DistillerSettings) -> Result<usize> {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    if daily_cap_reached(settings.daily_cap, &today).await {
        tracing::debug!("learned_instructions: cap diurno raggiunto, skip giro");
        return Ok(0);
    }

    // Progetti con >= min_new_signals nuovi eventi error/failed_attempt oltre il
    // cursore (trigger worklog-driven; i wiki_docs sono evidenza aggiuntiva).
    //
    // Separazione DB (flag ON): nexus_session_worklog_events e' per-progetto,
    // nexus_project_distill_state (cursore) vive nel meta. Il JOIN originale e'
    // cross-DB e non eseguibile su un solo pool: si spezza. Itera i progetti,
    // per ognuno leggi il cursore dal meta e conta i segnali sul pool del
    // progetto; seleziona i candidati oltre soglia (top 20 per conteggio).
    let mut candidates: Vec<(Uuid, i64)> = Vec::new();
    for project_id in crate::project_db_routes::list_all_project_ids(&state.db).await {
        let worklog_cursor: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT last_worklog_cursor FROM nexus_project_distill_state WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .context("SELECT cursore distill (discovery)")?
        .flatten();

        let proj_pool =
            crate::project_db_routes::project_data_pool_from(&state.db, project_id).await;
        let cnt: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_session_worklog_events \
             WHERE kind IN ('error', 'failed_attempt') \
               AND project_id = $1 \
               AND ($2::timestamptz IS NULL OR created_at > $2)",
        )
        .bind(project_id)
        .bind(worklog_cursor)
        .fetch_one(&proj_pool)
        .await
        .context("COUNT segnali worklog da distillare")?;

        if cnt >= settings.min_new_signals {
            candidates.push((project_id, cnt));
        }
    }
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.truncate(20);

    let mut done = 0usize;
    for (project_id, _cnt) in candidates {
        if daily_cap_reached(settings.daily_cap, &today).await {
            break;
        }
        match distill_project(state, settings, project_id).await {
            Ok(applied) => {
                record_distill_call(&today).await;
                if applied {
                    done += 1;
                }
            }
            Err(e) => {
                tracing::warn!(%project_id, error = %e, "learned_instructions: distillazione progetto fallita");
                // Purpose non configurato: inutile insistere sugli altri progetti.
                if e.to_string().contains("purpose") {
                    break;
                }
            }
        }
    }
    Ok(done)
}

/// Distilla un singolo progetto: raccoglie evidenza nuova, chiama l'LLM, applica
/// le operazioni e avanza il cursore. Ritorna Ok(true) se ha applicato qualcosa.
async fn distill_project(
    state: &AppState,
    settings: &DistillerSettings,
    project_id: Uuid,
) -> Result<bool> {
    // Cursori correnti.
    let cursor_row = sqlx::query(
        "SELECT last_worklog_cursor, last_wiki_cursor \
         FROM nexus_project_distill_state WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .context("SELECT cursore distill")?;
    let worklog_cursor: Option<chrono::DateTime<chrono::Utc>> = cursor_row
        .as_ref()
        .and_then(|r| r.try_get("last_worklog_cursor").ok());
    let wiki_cursor: Option<chrono::DateTime<chrono::Utc>> = cursor_row
        .as_ref()
        .and_then(|r| r.try_get("last_wiki_cursor").ok());

    // Separazione DB: nexus_session_worklog_events e' una tabella per-progetto,
    // instrada sul pool del progetto (a flag OFF ritorna il meta-DB).
    let proj_pool = crate::project_db_routes::project_data_pool_from(&state.db, project_id).await;

    // Evidenza worklog (segnali deterministici di cosa va storto ripetutamente).
    let wl_rows = sqlx::query(
        "SELECT kind, payload, created_at FROM nexus_session_worklog_events \
         WHERE project_id = $1 AND kind IN ('error', 'failed_attempt') \
           AND ($2::timestamptz IS NULL OR created_at > $2) \
         ORDER BY created_at DESC LIMIT $3",
    )
    .bind(project_id)
    .bind(worklog_cursor)
    .bind(settings.evidence_max_items)
    .fetch_all(&proj_pool)
    .await
    .context("SELECT evidenza worklog")?;

    // Evidenza episodica (note di chat e resoconti run).
    let wiki_rows = sqlx::query(
        "SELECT title, body_md, created_at FROM wiki_docs \
         WHERE project_id = $1 AND scope = 'project' \
           AND kind IN ('chat_note', 'run_summary') \
           AND ($2::timestamptz IS NULL OR created_at > $2) \
         ORDER BY created_at DESC LIMIT $3",
    )
    .bind(project_id)
    .bind(wiki_cursor)
    .bind(settings.evidence_max_items)
    .fetch_all(&state.db)
    .await
    .context("SELECT evidenza wiki_docs")?;

    if wl_rows.is_empty() && wiki_rows.is_empty() {
        return Ok(false);
    }

    // Avanzamento cursore: i max(created_at) letti (anche se l'LLM non produce
    // nulla, l'evidenza e' "vista" e non va riprocessata in loop).
    let mut new_wl_cursor = worklog_cursor;
    let mut evidence = String::new();
    for r in &wl_rows {
        let kind: String = r.try_get("kind").unwrap_or_default();
        let payload: Value = r.try_get("payload").unwrap_or(Value::Null);
        let ts: Option<chrono::DateTime<chrono::Utc>> = r.try_get("created_at").ok();
        if let Some(ts) = ts {
            if new_wl_cursor.is_none_or(|c| ts > c) {
                new_wl_cursor = Some(ts);
            }
        }
        let detail = payload.get("detail").and_then(Value::as_str).unwrap_or("");
        let tool = payload.get("tool").and_then(Value::as_str).unwrap_or("");
        let excerpt = payload.get("excerpt").and_then(Value::as_str).unwrap_or("");
        evidence.push_str(&format!(
            "- [{kind}] tool={tool} {detail} {}\n",
            excerpt.chars().take(160).collect::<String>()
        ));
    }
    let mut new_wiki_cursor = wiki_cursor;
    for r in &wiki_rows {
        let title: String = r.try_get("title").unwrap_or_default();
        let body: String = r.try_get("body_md").unwrap_or_default();
        let ts: Option<chrono::DateTime<chrono::Utc>> = r.try_get("created_at").ok();
        if let Some(ts) = ts {
            if new_wiki_cursor.is_none_or(|c| ts > c) {
                new_wiki_cursor = Some(ts);
            }
        }
        evidence.push_str(&format!(
            "- [nota] {title}: {}\n",
            body.chars().take(300).collect::<String>()
        ));
    }

    // Regole gia' note (per il dedup semantico lato LLM) e nome progetto.
    let known = sqlx::query(
        "SELECT id, category, rule_text FROM nexus_learned_instructions \
         WHERE project_id = $1 AND status IN ('active', 'proposed') \
         ORDER BY updated_at DESC LIMIT 60",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .context("SELECT regole note")?;
    let known_json: Vec<Value> = known
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
                "category": r.try_get::<String, _>("category").unwrap_or_default(),
                "rule_text": r.try_get::<String, _>("rule_text").unwrap_or_default(),
            })
        })
        .collect();
    let project_name: String = sqlx::query_scalar("SELECT name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| project_id.to_string());

    // Risolve provider/modello dal PUNTO UNICO (regola G/L).
    let (provider, model) = match resolve_purpose_model(state, PURPOSE).await {
        PurposeResolution::Resolved { provider, model, .. } => (provider, model),
        PurposeResolution::NoCapableModel { tier } => {
            anyhow::bail!("nessun modello del tier '{tier}' per purpose {PURPOSE}")
        }
        PurposeResolution::NotFound => {
            anyhow::bail!("purpose {PURPOSE} non configurato (applicare mig 0412)")
        }
        PurposeResolution::MatrixUnavailable(e) => {
            anyhow::bail!("routing matrix non disponibile: {e}")
        }
    };

    // Prompt da template DB (regola D), placeholder sostituiti.
    let template =
        nexus_types::get_template_or_default(&state.db, &state.template_cache, DISTILL_TEMPLATE_KEY)
            .await;
    if template.trim().is_empty() {
        anyhow::bail!("template {DISTILL_TEMPLATE_KEY} vuoto");
    }
    let prompt = template
        .replace("{{project_name}}", &project_name)
        .replace(
            "{{current_rules_json}}",
            &serde_json::to_string(&known_json).unwrap_or_else(|_| "[]".into()),
        )
        .replace("{{evidence}}", evidence.trim())
        .replace("{{max_active}}", &settings.max_active_per_project.to_string());

    // Niente prompt/evidenza nei log (regola F): solo metadati.
    tracing::info!(%project_id, %provider, %model, wl = wl_rows.len(), wiki = wiki_rows.len(), "learned_instructions: invio LLM");

    let messages_json = serde_json::to_string(&json!([{"role": "user", "content": prompt}]))?;
    let resp = state
        .orchestrator
        .neural
        .generate_agent_turn(&provider, &model, &messages_json, "[]", 1200, "")
        .await
        .context("generate_agent_turn distill")?;
    let content = resp
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    let parsed = nexus_types::llm_json::extract_json_block(&content);
    let operations = parsed
        .as_ref()
        .and_then(|v| v.get("operations"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let applied = apply_operations(state, settings, project_id, &operations).await?;

    // Avanza il cursore SEMPRE (l'evidenza e' stata vista): niente loop.
    sqlx::query(
        "INSERT INTO nexus_project_distill_state \
            (project_id, last_worklog_cursor, last_wiki_cursor, last_run_at, updated_at) \
         VALUES ($1, $2, $3, NOW(), NOW()) \
         ON CONFLICT (project_id) DO UPDATE SET \
            last_worklog_cursor = EXCLUDED.last_worklog_cursor, \
            last_wiki_cursor = EXCLUDED.last_wiki_cursor, \
            last_run_at = NOW(), updated_at = NOW()",
    )
    .bind(project_id)
    .bind(new_wl_cursor)
    .bind(new_wiki_cursor)
    .execute(&state.db)
    .await
    .context("UPSERT cursore distill")?;

    Ok(applied > 0)
}

/// Applica le operazioni del distiller alla tabella. Le regole `manually_edited`
/// non vengono mai toccate dal worker (review umana sovrana). Ritorna il numero
/// di operazioni andate a buon fine.
async fn apply_operations(
    state: &AppState,
    settings: &DistillerSettings,
    project_id: Uuid,
    operations: &[Value],
) -> Result<usize> {
    let mut applied = 0usize;
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_learned_instructions WHERE project_id = $1 AND status = 'active'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let mut active_budget = (settings.max_active_per_project - active_count).max(0);

    for op in operations.iter().take(8) {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
        let res = match kind {
            "add" => {
                let rule_text = op.get("rule_text").and_then(Value::as_str).unwrap_or("").trim();
                if rule_text.is_empty() {
                    continue;
                }
                let category = normalize_category(op.get("category").and_then(Value::as_str));
                let rationale = op.get("rationale").and_then(Value::as_str).unwrap_or("");
                let confidence = op.get("confidence").and_then(Value::as_f64).unwrap_or(0.5).clamp(0.0, 1.0);
                let hash = content_hash(rule_text);
                // active solo se confidence alta E c'e' budget; altrimenti proposed.
                let status = if confidence >= settings.auto_activate_confidence && active_budget > 0 {
                    active_budget -= 1;
                    "active"
                } else {
                    "proposed"
                };
                sqlx::query(
                    "INSERT INTO nexus_learned_instructions \
                        (project_id, category, rule_text, rationale, status, confidence, content_hash, source_kind) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, 'mixed') \
                     ON CONFLICT (project_id, content_hash) DO UPDATE SET \
                        occurrences = nexus_learned_instructions.occurrences + 1, \
                        confidence = GREATEST(nexus_learned_instructions.confidence, EXCLUDED.confidence), \
                        last_seen_at = NOW(), updated_at = NOW() \
                     WHERE nexus_learned_instructions.manually_edited = FALSE",
                )
                .bind(project_id)
                .bind(category)
                .bind(rule_text)
                .bind(rationale)
                .bind(status)
                .bind(confidence)
                .bind(&hash)
                .execute(&state.db)
                .await
            }
            "update" => {
                let Some(id) = op.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()) else {
                    continue;
                };
                let rule_text = op.get("rule_text").and_then(Value::as_str).unwrap_or("").trim();
                if rule_text.is_empty() {
                    continue;
                }
                let rationale = op.get("rationale").and_then(Value::as_str).unwrap_or("");
                let confidence = op.get("confidence").and_then(Value::as_f64).unwrap_or(0.5).clamp(0.0, 1.0);
                let hash = content_hash(rule_text);
                sqlx::query(
                    "UPDATE nexus_learned_instructions \
                     SET rule_text = $3, rationale = $4, confidence = $5, content_hash = $6, \
                         last_seen_at = NOW(), updated_at = NOW(), updated_by = 'distiller' \
                     WHERE id = $1 AND project_id = $2 AND manually_edited = FALSE",
                )
                .bind(id)
                .bind(project_id)
                .bind(rule_text)
                .bind(rationale)
                .bind(confidence)
                .bind(&hash)
                .execute(&state.db)
                .await
            }
            "confirm" => {
                let Some(id) = op.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()) else {
                    continue;
                };
                sqlx::query(
                    "UPDATE nexus_learned_instructions \
                     SET occurrences = occurrences + 1, last_seen_at = NOW() \
                     WHERE id = $1 AND project_id = $2",
                )
                .bind(id)
                .bind(project_id)
                .execute(&state.db)
                .await
            }
            "retire" => {
                let Some(id) = op.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()) else {
                    continue;
                };
                sqlx::query(
                    "UPDATE nexus_learned_instructions \
                     SET status = 'retired', updated_at = NOW(), updated_by = 'distiller' \
                     WHERE id = $1 AND project_id = $2 AND manually_edited = FALSE",
                )
                .bind(id)
                .bind(project_id)
                .execute(&state.db)
                .await
            }
            _ => continue,
        };
        match res {
            Ok(r) if r.rows_affected() > 0 => applied += 1,
            Ok(_) => {}
            Err(e) => tracing::warn!(%project_id, op = kind, error = %e, "learned_instructions: operazione fallita"),
        }
    }
    Ok(applied)
}

fn normalize_category(raw: Option<&str>) -> String {
    match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "convention" => "convention",
        "preference" => "preference",
        "environment" => "environment",
        "tooling" => "tooling",
        "process" => "process",
        _ => "convention",
    }
    .to_string()
}

/// Hash del testo regola NORMALIZZATO (lowercase, spazi collassati): dedup
/// robusto a differenze cosmetiche di formattazione tra distillazioni.
fn content_hash(rule_text: &str) -> String {
    let normalized = rule_text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ───────────────────────────────────────────────────────────────────────────
// Handler admin (review umana delle regole: list / patch / distill-now)
// ───────────────────────────────────────────────────────────────────────────

type AdminResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn bad_request(msg: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
}

const VALID_STATUSES: [&str; 4] = ["proposed", "active", "rejected", "retired"];
const VALID_CATEGORIES: [&str; 5] = ["convention", "preference", "environment", "tooling", "process"];

#[derive(Deserialize)]
pub struct ListQuery {
    pub project_id: String,
    pub status: Option<String>,
}

/// GET /api/admin/learned-instructions?project_id=..&status=..
/// Lista le regole del progetto; `status` opzionale ('all' o assente = tutte).
pub async fn list_learned_instructions(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> AdminResult {
    let pid = Uuid::parse_str(&q.project_id).map_err(|_| bad_request("project_id non valido"))?;
    let status_filter = q
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "all")
        .map(str::to_string);

    let rows = sqlx::query(
        "SELECT id, category, rule_text, rationale, status, confidence, occurrences, \
                manually_edited, created_at, updated_at, last_seen_at \
         FROM nexus_learned_instructions \
         WHERE project_id = $1 AND ($2::text IS NULL OR status = $2) \
         ORDER BY \
            CASE status WHEN 'proposed' THEN 0 WHEN 'active' THEN 1 ELSE 2 END, \
            confidence DESC, updated_at DESC",
    )
    .bind(pid)
    .bind(status_filter.as_deref())
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
                "category": r.try_get::<String, _>("category").unwrap_or_default(),
                "ruleText": r.try_get::<String, _>("rule_text").unwrap_or_default(),
                "rationale": r.try_get::<Option<String>, _>("rationale").ok().flatten(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
                "confidence": r.try_get::<f32, _>("confidence").unwrap_or(0.0),
                "occurrences": r.try_get::<i32, _>("occurrences").unwrap_or(0),
                "manuallyEdited": r.try_get::<bool, _>("manually_edited").unwrap_or(false),
                "createdAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at").map(|t| t.to_rfc3339()).unwrap_or_default(),
                "updatedAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at").map(|t| t.to_rfc3339()).unwrap_or_default(),
            })
        })
        .collect();

    Ok(Json(json!({ "data": items, "total": items.len() })))
}

#[derive(Deserialize)]
pub struct PatchBody {
    pub status: Option<String>,
    pub rule_text: Option<String>,
    pub category: Option<String>,
}

/// PATCH /api/admin/learned-instructions/:id
/// Cambia status (approve/reject) e/o edita testo/categoria. Editare il testo o
/// la categoria marca `manually_edited=true`: il distiller non tocchera' piu' la
/// regola (review umana sovrana, stesso pattern di wiki_docs.manually_edited).
pub async fn patch_learned_instruction(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(body): Json<PatchBody>,
) -> AdminResult {
    let iid = Uuid::parse_str(&id).map_err(|_| bad_request("id non valido"))?;
    if let Some(st) = body.status.as_deref() {
        if !VALID_STATUSES.contains(&st) {
            return Err(bad_request("status non valido"));
        }
    }
    if let Some(cat) = body.category.as_deref() {
        if !VALID_CATEGORIES.contains(&cat) {
            return Err(bad_request("category non valida"));
        }
    }
    let edited = body.rule_text.is_some() || body.category.is_some();
    let new_hash = body.rule_text.as_deref().map(content_hash);

    let res = sqlx::query(
        "UPDATE nexus_learned_instructions SET \
            status = COALESCE($2, status), \
            rule_text = COALESCE($3, rule_text), \
            category = COALESCE($4, category), \
            content_hash = COALESCE($5, content_hash), \
            manually_edited = CASE WHEN $6 THEN TRUE ELSE manually_edited END, \
            updated_by = $7, updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(iid)
    .bind(body.status.as_deref())
    .bind(body.rule_text.as_deref())
    .bind(body.category.as_deref())
    .bind(new_hash.as_deref())
    .bind(edited)
    .bind(&claims.sub)
    .execute(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    if res.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, Json(json!({ "error": "regola non trovata" }))));
    }
    Ok(Json(json!({ "id": iid.to_string(), "status": "updated" })))
}

#[derive(Deserialize)]
pub struct DistillNowBody {
    pub project_id: String,
}

/// POST /api/admin/learned-instructions/distill
/// Trigger manuale della distillazione per un progetto (onboarding / test E2E,
/// niente attesa dell'intervallo del worker). Stesso punto unico del loop.
pub async fn distill_now(
    State(state): State<AppState>,
    Json(body): Json<DistillNowBody>,
) -> AdminResult {
    let pid = Uuid::parse_str(&body.project_id).map_err(|_| bad_request("project_id non valido"))?;
    let settings = current_settings(&state.db).await;
    match distill_project(&state, &settings, pid).await {
        Ok(applied) => Ok(Json(json!({ "ok": true, "applied": applied }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Test (puri, senza DB)
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_robusto_a_differenze_cosmetiche() {
        let a = content_hash("Usa sempre pnpm, mai npm.");
        let b = content_hash("usa   sempre PNPM,   mai npm.");
        assert_eq!(a, b, "case e spazi non devono cambiare l'hash (dedup)");
        let c = content_hash("Usa sempre yarn.");
        assert_ne!(a, c, "regole diverse -> hash diversi");
    }

    #[test]
    fn category_normalizzata_con_fallback() {
        assert_eq!(normalize_category(Some("Tooling")), "tooling");
        assert_eq!(normalize_category(Some("environment")), "environment");
        assert_eq!(normalize_category(Some("inesistente")), "convention");
        assert_eq!(normalize_category(None), "convention");
    }

    #[test]
    fn operations_parse_da_json_con_fence() {
        // L'LLM puo' incapsulare in un fence: extract_json_block deve estrarre.
        let raw = "```json\n{\"operations\":[{\"op\":\"add\",\"category\":\"tooling\",\"rule_text\":\"Usa pnpm\",\"confidence\":0.9}]}\n```";
        let parsed = nexus_types::llm_json::extract_json_block(raw).expect("json estratto");
        let ops = parsed.get("operations").and_then(Value::as_array).expect("operations");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].get("op").and_then(Value::as_str), Some("add"));
    }
}
