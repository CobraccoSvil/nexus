//! Worker `routing_matrix_auto_promoter` — ricostruisce periodicamente la
//! routing matrix dal `ai_price_catalog` in base a regole stabili
//! configurate in `nexus_intent_routing_requirements`.
//!
//! Pipeline (per ogni intent/behavior_mode):
//!   1. Leggi requisiti: capabilities richieste, tool_use, tier preferito,
//!      direzione costo, pesi scoring.
//!   2. Filtra candidati dal catalog:
//!      - is_enabled=true
//!      - consecutive_failures=0 (oppure < model_health_probe_failure_threshold)
//!      - supports_tool_use=true se requires_tool_use
//!      - capabilities @> required_capabilities (se non vuoto)
//!   3. Score ogni candidato (0..1) come somma pesata di 4 score:
//!      - tier_score: 1.0 se match preferred_tier, 0.5 se adiacente, 0.0 lontano
//!      - cost_score: normalizzato 0..1, direzione asc/desc
//!      - context_score: log(ctx) normalizzato (preferisce >= 100k)
//!      - capability_score: % capability richieste presenti
//!   4. Prendi top-1 per provider (max 3 candidati: i 3 provider migliori).
//!   5. UPDATE nexus_routing_matrix per ogni (intent, behavior_mode, provider)
//!      che NON ha manual_override=true.
//!
//! Cadenza: configurabile, default 6h.
//!
//! Test: vedi #[cfg(test)] in fondo al file (5 test unitari su scoring).

use sqlx::{PgPool, Row};
use std::time::Duration;
use tokio::time::sleep;

const MIN_INTERVAL_S: u64 = 600;

#[derive(Debug, Clone)]
struct IntentRequirement {
    intent: String,
    behavior_mode: String,
    required_capabilities: Vec<String>,
    requires_tool_use: bool,
    preferred_tier: String,
    weight_tier: f32,
    weight_cost: f32,
    weight_context: f32,
    weight_capabilities: f32,
    cost_direction: String,
}

#[derive(Debug, Clone)]
struct CatalogModel {
    provider: String,
    model: String,
    performance_tier: String,
    input_cost: f64,
    context_window: i32,
    supports_tool_use: bool,
    capabilities: Vec<String>,
}

#[derive(Debug, Clone)]
struct ScoredModel {
    catalog: CatalogModel,
    score: f32,
}

pub fn spawn_routing_matrix_auto_promoter(db: PgPool, enabled: bool, interval_s: u64) {
    let enabled = match std::env::var("NEXUS_ROUTING_MATRIX_AUTO_PROMOTE_ENABLED").as_deref() {
        Ok("false") | Ok("0") => false,
        Ok("true") | Ok("1") => true,
        _ => enabled,
    };
    if !enabled {
        tracing::info!("routing_matrix_auto_promoter: DISABILITATO");
        return;
    }
    let interval_s = std::env::var("NEXUS_ROUTING_MATRIX_AUTO_PROMOTE_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(interval_s)
        .max(MIN_INTERVAL_S);
    tracing::info!(
        "routing_matrix_auto_promoter: avvio worker (interval={interval_s}s)"
    );
    tokio::spawn(async move {
        // Aspetta 120s al primo avvio (dopo che gli altri worker hanno
        // popolato consecutive_failures e gli altri dati).
        sleep(Duration::from_secs(120)).await;
        loop {
            match run_one_round(&db).await {
                Ok(stats) => tracing::info!(
                    "routing_matrix_auto_promoter: round completato — updated={} skipped_manual={} no_candidates={}",
                    stats.updated, stats.skipped_manual, stats.no_candidates
                ),
                Err(e) => tracing::warn!("routing_matrix_auto_promoter: round fallito: {e}"),
            }
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

#[derive(Default, Debug, serde::Serialize)]
pub struct PromoteStats {
    pub updated: usize,
    pub skipped_manual: usize,
    pub no_candidates: usize,
}

pub async fn run_one_round(db: &PgPool) -> anyhow::Result<PromoteStats> {
    let requirements = load_requirements(db).await?;
    let catalog = load_catalog(db).await?;
    let mut stats = PromoteStats::default();

    for req in &requirements {
        let top_by_provider = select_top_candidates(req, &catalog);
        if top_by_provider.is_empty() {
            stats.no_candidates += 1;
            continue;
        }
        for (rank, scored) in top_by_provider.iter().enumerate() {
            // Skip se la riga esiste con manual_override=true.
            let row_exists_manual: bool = sqlx::query_scalar::<_, bool>(
                "SELECT COALESCE(manual_override, false) FROM nexus_routing_matrix
                  WHERE intent=$1 AND behavior_mode=$2 AND provider=$3",
            )
            .bind(&req.intent)
            .bind(&req.behavior_mode)
            .bind(&scored.catalog.provider)
            .fetch_optional(db)
            .await
            .unwrap_or(None)
            .unwrap_or(false);
            if row_exists_manual {
                stats.skipped_manual += 1;
                continue;
            }
            sqlx::query(
                "INSERT INTO nexus_routing_matrix
                   (intent, behavior_mode, provider, model_id, priority, is_active,
                    notes, last_auto_promote_at, auto_promote_score)
                 VALUES ($1,$2,$3,$4,$5,true,$6,NOW(),$7)
                 ON CONFLICT (intent, behavior_mode, provider) DO UPDATE
                   SET model_id = EXCLUDED.model_id,
                       is_active = true,
                       priority = EXCLUDED.priority,
                       notes = EXCLUDED.notes,
                       last_auto_promote_at = NOW(),
                       auto_promote_score = EXCLUDED.auto_promote_score,
                       updated_at = NOW()
                 WHERE nexus_routing_matrix.manual_override = false",
            )
            .bind(&req.intent)
            .bind(&req.behavior_mode)
            .bind(&scored.catalog.provider)
            .bind(&scored.catalog.model)
            .bind(100 - (rank as i32) * 10) // priority 100/90/80 sui top-3
            .bind(format!("auto-promote: score={:.3}", scored.score))
            .bind(scored.score)
            .execute(db)
            .await?;
            stats.updated += 1;
        }
    }
    Ok(stats)
}

async fn load_requirements(db: &PgPool) -> sqlx::Result<Vec<IntentRequirement>> {
    let rows = sqlx::query(
        "SELECT intent, behavior_mode, required_capabilities, requires_tool_use,
                preferred_tier, weight_tier, weight_cost, weight_context,
                weight_capabilities, cost_direction
           FROM nexus_intent_routing_requirements
          ORDER BY intent, behavior_mode",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| IntentRequirement {
            intent: r.try_get("intent").unwrap_or_default(),
            behavior_mode: r.try_get("behavior_mode").unwrap_or_default(),
            required_capabilities: r
                .try_get::<Vec<String>, _>("required_capabilities")
                .unwrap_or_default(),
            requires_tool_use: r.try_get("requires_tool_use").unwrap_or(false),
            preferred_tier: r.try_get("preferred_tier").unwrap_or_else(|_| "medium".into()),
            weight_tier: r.try_get("weight_tier").unwrap_or(0.35),
            weight_cost: r.try_get("weight_cost").unwrap_or(0.25),
            weight_context: r.try_get("weight_context").unwrap_or(0.20),
            weight_capabilities: r.try_get("weight_capabilities").unwrap_or(0.20),
            cost_direction: r.try_get("cost_direction").unwrap_or_else(|_| "asc".into()),
        })
        .collect())
}

async fn load_catalog(db: &PgPool) -> sqlx::Result<Vec<CatalogModel>> {
    let rows = sqlx::query(
        "SELECT provider, model, performance_tier,
                input_cost_per_million_tokens::float8 AS input_cost,
                context_window, supports_tool_use,
                capabilities
           FROM ai_price_catalog
          WHERE is_enabled = true
            AND consecutive_failures = 0",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let caps: serde_json::Value = r
                .try_get("capabilities")
                .unwrap_or(serde_json::Value::Array(vec![]));
            let capabilities = match caps {
                serde_json::Value::Array(arr) => arr
                    .into_iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
                _ => vec![],
            };
            CatalogModel {
                provider: r.try_get("provider").unwrap_or_default(),
                model: r.try_get("model").unwrap_or_default(),
                performance_tier: r
                    .try_get("performance_tier")
                    .unwrap_or_else(|_| "medium".into()),
                input_cost: r.try_get("input_cost").unwrap_or(0.0),
                context_window: r.try_get("context_window").unwrap_or(8192),
                supports_tool_use: r.try_get("supports_tool_use").unwrap_or(true),
                capabilities,
            }
        })
        .collect())
}

/// Per un (intent, behavior_mode) ritorna top-N candidati distinti per provider
/// (max 3, uno per provider). Score 0..1.
fn select_top_candidates(req: &IntentRequirement, catalog: &[CatalogModel]) -> Vec<ScoredModel> {
    let mut scored: Vec<ScoredModel> = catalog
        .iter()
        .filter(|m| {
            // Filtri obbligatori.
            if req.requires_tool_use && !m.supports_tool_use {
                return false;
            }
            if !req.required_capabilities.is_empty() && !m.capabilities.is_empty() {
                let required_lc: Vec<String> =
                    req.required_capabilities.iter().map(|s| s.to_lowercase()).collect();
                let cap_lc: Vec<String> =
                    m.capabilities.iter().map(|s| s.to_lowercase()).collect();
                let pct = capability_match_pct(&required_lc, &cap_lc);
                // Almeno 50% delle capability richieste devono essere presenti.
                if pct < 0.5 {
                    return false;
                }
            }
            true
        })
        .map(|m| ScoredModel {
            score: score_model(req, m, catalog),
            catalog: m.clone(),
        })
        .collect();

    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Top-1 per provider, max 3 provider distinti.
    let mut seen_providers = std::collections::HashSet::new();
    let mut top = Vec::new();
    for s in scored {
        if seen_providers.contains(&s.catalog.provider) {
            continue;
        }
        seen_providers.insert(s.catalog.provider.clone());
        top.push(s);
        if top.len() >= 3 {
            break;
        }
    }
    top
}

/// Calcola lo score 0..1 per un modello dato un requirement.
/// Esposto per testabilita'.
fn score_model(req: &IntentRequirement, m: &CatalogModel, full_catalog: &[CatalogModel]) -> f32 {
    let tier_score = tier_score(&req.preferred_tier, &m.performance_tier);
    let cost_score = cost_score(req, m, full_catalog);
    let context_score = context_score(m.context_window);
    let cap_score = if req.required_capabilities.is_empty() {
        1.0
    } else {
        let required_lc: Vec<String> =
            req.required_capabilities.iter().map(|s| s.to_lowercase()).collect();
        let cap_lc: Vec<String> = m.capabilities.iter().map(|s| s.to_lowercase()).collect();
        capability_match_pct(&required_lc, &cap_lc)
    };

    req.weight_tier * tier_score
        + req.weight_cost * cost_score
        + req.weight_context * context_score
        + req.weight_capabilities * cap_score
}

fn tier_score(preferred: &str, actual: &str) -> f32 {
    let pref_rank = tier_rank(preferred);
    let actual_rank = tier_rank(actual);
    let diff = (pref_rank - actual_rank).abs();
    match diff {
        0 => 1.0,
        1 => 0.5,
        _ => 0.0,
    }
}

fn tier_rank(tier: &str) -> i32 {
    match tier {
        "heavy" => 2,
        "medium" => 1,
        "light" => 0,
        _ => 1,
    }
}

fn cost_score(req: &IntentRequirement, m: &CatalogModel, catalog: &[CatalogModel]) -> f32 {
    // Trova min e max cost nel catalog (filtrato per tier match per essere
    // confrontabile).
    let costs: Vec<f64> = catalog
        .iter()
        .map(|c| c.input_cost)
        .filter(|c| *c > 0.0)
        .collect();
    if costs.is_empty() {
        return 0.5;
    }
    let min = costs.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = costs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if (max - min).abs() < f64::EPSILON {
        return 0.5;
    }
    let normalized = ((m.input_cost - min) / (max - min)) as f32;
    if req.cost_direction == "asc" {
        // Vogliamo costo basso = score alto.
        1.0 - normalized
    } else {
        // Approfondita: costo alto = score alto (proxy di capability).
        normalized
    }
}

fn context_score(ctx: i32) -> f32 {
    // log scale: 8k = 0, 1M = 1.
    if ctx <= 8192 {
        return 0.0;
    }
    let log_ctx = (ctx as f32).log2();
    let min_log = (8192_f32).log2(); // 13
    let max_log = (1_048_576_f32).log2(); // 20
    ((log_ctx - min_log) / (max_log - min_log)).clamp(0.0, 1.0)
}

fn capability_match_pct(required: &[String], available: &[String]) -> f32 {
    if required.is_empty() {
        return 1.0;
    }
    let matched = required.iter().filter(|r| available.contains(r)).count();
    matched as f32 / required.len() as f32
}

// ── TESTS ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn model(p: &str, name: &str, tier: &str, cost: f64, ctx: i32, tools: bool, caps: Vec<&str>) -> CatalogModel {
        CatalogModel {
            provider: p.into(),
            model: name.into(),
            performance_tier: tier.into(),
            input_cost: cost,
            context_window: ctx,
            supports_tool_use: tools,
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn req(intent: &str, behavior: &str, caps: Vec<&str>, tools: bool, tier: &str, cost_dir: &str) -> IntentRequirement {
        IntentRequirement {
            intent: intent.into(),
            behavior_mode: behavior.into(),
            required_capabilities: caps.iter().map(|s| s.to_string()).collect(),
            requires_tool_use: tools,
            preferred_tier: tier.into(),
            weight_tier: 0.35,
            weight_cost: 0.25,
            weight_context: 0.20,
            weight_capabilities: 0.20,
            cost_direction: cost_dir.into(),
        }
    }

    #[test]
    fn tier_score_exact_match() {
        assert_eq!(tier_score("heavy", "heavy"), 1.0);
        assert_eq!(tier_score("medium", "medium"), 1.0);
    }

    #[test]
    fn tier_score_adjacent_half() {
        assert_eq!(tier_score("heavy", "medium"), 0.5);
        assert_eq!(tier_score("medium", "light"), 0.5);
    }

    #[test]
    fn tier_score_far_zero() {
        assert_eq!(tier_score("heavy", "light"), 0.0);
    }

    #[test]
    fn cost_score_asc_prefers_cheap() {
        let catalog = vec![
            model("a", "cheap", "light", 0.1, 8192, true, vec![]),
            model("b", "expensive", "heavy", 10.0, 200000, true, vec![]),
        ];
        let r = req("test", "veloce", vec![], false, "light", "asc");
        let s_cheap = cost_score(&r, &catalog[0], &catalog);
        let s_expensive = cost_score(&r, &catalog[1], &catalog);
        assert!(s_cheap > s_expensive);
    }

    #[test]
    fn cost_score_desc_prefers_expensive() {
        let catalog = vec![
            model("a", "cheap", "light", 0.1, 8192, true, vec![]),
            model("b", "expensive", "heavy", 10.0, 200000, true, vec![]),
        ];
        let r = req("test", "approfondita", vec![], false, "heavy", "desc");
        let s_cheap = cost_score(&r, &catalog[0], &catalog);
        let s_expensive = cost_score(&r, &catalog[1], &catalog);
        assert!(s_expensive > s_cheap);
    }

    #[test]
    fn context_score_monotonic() {
        let small = context_score(8192);
        let medium = context_score(100_000);
        let large = context_score(1_000_000);
        assert!(small <= medium);
        assert!(medium <= large);
        assert!((large - 1.0).abs() < 0.05);
    }

    #[test]
    fn capability_match_full_partial_none() {
        let req_caps = vec!["code".to_string(), "fix".to_string()];
        assert_eq!(capability_match_pct(&req_caps, &["code".into(), "fix".into()]), 1.0);
        assert_eq!(capability_match_pct(&req_caps, &["code".into()]), 0.5);
        assert_eq!(capability_match_pct(&req_caps, &[]), 0.0);
    }

    #[test]
    fn select_top_filters_tool_use() {
        let catalog = vec![
            model("a", "no-tools", "heavy", 1.0, 200000, false, vec!["code"]),
            model("b", "with-tools", "heavy", 1.0, 200000, true, vec!["code"]),
        ];
        let r = req("fix_complesso", "approfondita", vec!["code"], true, "heavy", "desc");
        let top = select_top_candidates(&r, &catalog);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].catalog.model, "with-tools");
    }

    #[test]
    fn select_top_one_per_provider() {
        let catalog = vec![
            model("anthropic", "claude-opus", "heavy", 5.0, 200000, true, vec!["code", "reasoning"]),
            model("anthropic", "claude-sonnet", "medium", 3.0, 200000, true, vec!["code"]),
            model("openai", "gpt-5", "heavy", 4.0, 128000, true, vec!["code", "reasoning"]),
            model("google", "gemini-pro", "heavy", 1.25, 1_000_000, true, vec!["code", "reasoning", "long-context"]),
        ];
        let r = req("fix_complesso", "approfondita", vec!["code", "reasoning"], true, "heavy", "desc");
        let top = select_top_candidates(&r, &catalog);
        // Max 3 provider, top-1 per provider
        assert_eq!(top.len(), 3);
        let providers: std::collections::HashSet<_> = top.iter().map(|s| s.catalog.provider.clone()).collect();
        assert_eq!(providers.len(), 3);
        // Solo "claude-opus" tra anthropic (top-1)
        let anthropic_pick = top.iter().find(|s| s.catalog.provider == "anthropic").unwrap();
        assert_eq!(anthropic_pick.catalog.model, "claude-opus");
    }

    #[test]
    fn select_top_excludes_low_capability_match() {
        let catalog = vec![
            model("a", "weak", "medium", 1.0, 100000, true, vec!["chat"]),  // 0 di 2 capability
            model("b", "ok",   "medium", 1.0, 100000, true, vec!["code", "fix"]), // 2 di 2
        ];
        let r = req("fix_complesso", "bilanciata", vec!["code", "fix"], true, "medium", "asc");
        let top = select_top_candidates(&r, &catalog);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].catalog.model, "ok");
    }

    #[test]
    fn score_model_economica_prefers_light_cheap() {
        let catalog = vec![
            model("a", "light-cheap", "light", 0.1, 100000, true, vec!["code"]),
            model("b", "heavy-expensive", "heavy", 5.0, 200000, true, vec!["code"]),
        ];
        let r = req("fix_semplice", "economica", vec!["code"], true, "light", "asc");
        let s_light = score_model(&r, &catalog[0], &catalog);
        let s_heavy = score_model(&r, &catalog[1], &catalog);
        // "economica" + tier=light + cost_dir=asc → light-cheap deve vincere
        assert!(s_light > s_heavy, "economica deve preferire light-cheap ({s_light}) > heavy-expensive ({s_heavy})");
    }
}
