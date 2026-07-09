//! Worker `routing_matrix_auto_promoter` — ricostruisce periodicamente la
//! routing matrix dal `ai_price_catalog` in base a regole stabili
//! configurate in `nexus_intent_routing_requirements`.
//!
//! Pipeline (per ogni intent/behavior_mode):
//!   1. Leggi requisiti: capabilities richieste, tool_use, tier preferito,
//!      direzione costo, pesi scoring.
//!   2. Filtra candidati dal catalog:
//!      - is_enabled=true (la salute e' garantita da is_enabled; NON si filtra
//!        consecutive_failures: causava starvation, ADR 0025 / FASE 2b)
//!      - supports_tool_use=true se requires_tool_use
//!      - agentic_thinking_policy<>'exclude' se requires_tool_use (FASE 2b)
//!      - capabilities: soglia di rilevanza capability_match_pct>=0.5 (il grado
//!        esatto pesa nel cap_score)
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
    /// FASE 2b: allinea l'eleggibilita' offline al routing live (ADR 0025): un
    /// modello con `agentic_thinking_policy='exclude'` non e' candidabile per
    /// gli slot che richiedono tool_use (il live lo scarta gia').
    agentic_thinking_policy: String,
    /// Stato del pricing (mig 0477): 'unknown' (placeholder, costo 0 non
    /// raffinato), 'priced' (costo reale > 0), 'free' (gratuito reale confermato
    /// a mano). Disambigua il significato di input_cost==0 in cost_score: 'free'
    /// vale costo reale 0, 'unknown' e' neutro (non un ottimo).
    pricing_state: String,
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
    tracing::info!("routing_matrix_auto_promoter: avvio worker (interval={interval_s}s)");
    tokio::spawn(async move {
        // Aspetta 120s al primo avvio (dopo che gli altri worker hanno
        // popolato consecutive_failures e gli altri dati).
        sleep(Duration::from_secs(120)).await;
        loop {
            match run_one_round(&db).await {
                Ok(stats) => tracing::info!(
                    "routing_matrix_auto_promoter: round completato — healed={} updated={} skipped_manual={} no_candidates={} cleaned_up={}",
                    stats.healed, stats.updated, stats.skipped_manual, stats.no_candidates, stats.cleaned_up
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
    pub cleaned_up: usize,
    pub healed: usize,
}

pub async fn run_one_round(db: &PgPool) -> anyhow::Result<PromoteStats> {
    let mut stats = PromoteStats::default();

    // ── Heal pass (PRIMA di tutto) ───────────────────────────────────────────
    // Sana i pin orfani (model_id missing_from_api) sostituendoli con un modello
    // sano dello stesso provider+tier, ANCHE su righe manual_override. Cosi' il
    // promote/cleanup successivi e il routing live non vedono mai modelli morti.
    match heal_orphan_pinned_models(db).await {
        Ok(healed) => stats.healed = healed as usize,
        Err(e) => {
            tracing::warn!("routing_matrix_auto_promoter: heal_orphan_pinned_models fallito: {e}")
        }
    }

    let requirements = load_requirements(db).await?;
    let catalog = load_catalog(db).await?;

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

    // ── Cleanup pass ─────────────────────────────────────────────────────────
    // ORDINE CRITICO: il cleanup gira DOPO il promote, mai prima. Il promote ha
    // gia' inserito/aggiornato i modelli buoni; ora disattiviamo le righe
    // "stale" (modello ora broken nel catalog) senza creare una finestra senza
    // routing. Vedi cleanup_stale_rows per la logica idempotente e la safety
    // anti-blackout.
    match cleanup_stale_rows(db).await {
        Ok(deactivated) => {
            stats.cleaned_up = deactivated as usize;
        }
        Err(e) => {
            tracing::warn!("routing_matrix_auto_promoter: cleanup_stale_rows fallito: {e}");
        }
    }

    Ok(stats)
}

/// Disattiva le righe "stale" della routing matrix: righe `is_active=true` e
/// `manual_override=false` il cui `(provider, model_id)` NON ha piu' nel catalog
/// un modello ABILITATO (`is_enabled=true`; salute = solo is_enabled, la probe
/// auto-disabilita a soglia — ADR 0025, niente filtro consecutive_failures).
///
/// Idempotente: una seconda esecuzione non tocca nulla (le righe sono gia'
/// `is_active=false`). RISPETTA `manual_override=true` — quelle righe non
/// vengono MAI toccate.
///
/// Safety anti-blackout: dopo la disattivazione, per ogni `(intent,
/// behavior_mode)` rimasto SENZA alcuna riga `is_active=true` NON riattiviamo
/// righe broken (sarebbe mascherare il problema): logghiamo a WARNING l'elenco
/// cosi' la mancanza di routing e' visibile. In condizioni normali il promote
/// che precede ha gia' inserito modelli buoni; se non ce ne sono e' un problema
/// reale di disponibilita' modelli da risolvere a monte (catalog).
///
/// Ritorna il numero di righe disattivate.
pub async fn cleanup_stale_rows(db: &PgPool) -> sqlx::Result<u64> {
    // Flag enforcement (settings, regola G — niente env/hardcode).
    let enabled = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.routing_matrix_cleanup_stale_enabled'",
    )
    .fetch_optional(db)
    .await?
    .map(|v| {
        !matches!(
            v.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
    .unwrap_or(true);
    if !enabled {
        tracing::info!("routing_matrix_auto_promoter: cleanup_stale_rows DISABILITATO (flag)");
        return Ok(0);
    }

    const STALE_TAG: &str = " [auto-cleanup: modello non disponibile nel catalog]";

    // Disattiva le righe stale. Il NOT EXISTS confronta la riga matrix con un
    // modello presente e ABILITATO nel catalog (case-insensitive sul provider per
    // coerenza col resto del sistema). Appende il marcatore a notes solo se non
    // gia' presente (idempotenza sul testo di notes).
    //
    // NB: la salute e' garantita SOLO da is_enabled (punto unico: la probe
    // auto-disabilita a soglia). NON si filtra consecutive_failures, coerente
    // col filtro candidati del promote (ADR 0025 / FASE 2b): pretendere
    // failures=0 qui creava starvation — la riga spenta non riceve traffico,
    // il contatore non si azzera mai, la riga resta spenta per sempre (visto
    // il 2026-07-02: deepseek/google/mistral promossi con score alto e subito
    // rispenti dal cleanup, matrice ridotta ai soli manual_override).
    let res = sqlx::query(
        "UPDATE nexus_routing_matrix m
            SET is_active = false,
                notes = COALESCE(m.notes, '') ||
                        CASE WHEN COALESCE(m.notes, '') LIKE '%' || $1 || '%'
                             THEN '' ELSE $1 END,
                updated_at = NOW()
          WHERE m.is_active = true
            AND COALESCE(m.manual_override, false) = false
            AND NOT EXISTS (
                SELECT 1 FROM ai_price_catalog c
                 WHERE LOWER(c.provider) = LOWER(m.provider)
                   AND c.model = m.model_id
                   AND c.is_enabled = true
            )",
    )
    .bind(STALE_TAG)
    .execute(db)
    .await?;
    let deactivated = res.rows_affected();

    if deactivated > 0 {
        tracing::info!(
            "routing_matrix_auto_promoter: cleanup disattivate {} righe stale (modelli non piu' sani nel catalog)",
            deactivated
        );
    }

    // Safety anti-blackout: elenca (intent, behavior_mode) rimasti senza routing.
    let orphaned = sqlx::query(
        "SELECT DISTINCT m.intent, m.behavior_mode
           FROM nexus_routing_matrix m
          WHERE NOT EXISTS (
                SELECT 1 FROM nexus_routing_matrix a
                 WHERE a.intent = m.intent
                   AND a.behavior_mode = m.behavior_mode
                   AND a.is_active = true
            )
          ORDER BY m.intent, m.behavior_mode",
    )
    .fetch_all(db)
    .await?;
    if !orphaned.is_empty() {
        let pairs: Vec<String> = orphaned
            .iter()
            .map(|r| {
                let intent: String = r.try_get("intent").unwrap_or_default();
                let mode: String = r.try_get("behavior_mode").unwrap_or_default();
                format!("{intent}/{mode}")
            })
            .collect();
        tracing::warn!(
            "routing_matrix_auto_promoter: dopo cleanup {} (intent, behavior_mode) SENZA routing attivo: [{}] — problema reale di disponibilita' modelli, non riattivo righe broken",
            pairs.len(),
            pairs.join(", ")
        );
    }

    Ok(deactivated)
}

// ---------------------------------------------------------------------------
// Auto-heal dei "pin orfani" (regola H)
// ---------------------------------------------------------------------------
//
// PROBLEMA: il promote e il cleanup rispettano `manual_override=true` e non
// toccano quelle righe. Migrazioni storiche (0260/0268/0270/0274/0337) hanno
// messo manual_override=true su TUTTE le righe agentiche per "pinnarle". Quando
// un modello pinnato viene deprecato dall'API del provider (catalog_sync lo
// marca `auto_disabled_reason='missing_from_api'`), la riga resta a puntare a un
// modello inesistente: il routing lo sceglie, fallisce, e degrada su un modello
// light. Un pin su un modello che il provider non offre PIU' non e' una scelta
// valida dell'admin: e' orfano. Va sanato anche se manual_override=true.
//
// FIX: heal_orphan_pinned_models sostituisce il `model_id` morto con il miglior
// modello SANO dello stesso (provider, performance_tier), PRESERVANDO il pin
// (manual_override resta invariato: cambia solo il model_id, non la decisione di
// provider/tier dell'admin). Se non esiste alcun sostituto sano, lascia la riga
// al cleanup (che la logghera' come blackout reale).

/// Candidato sostituto del catalog (gia' filtrato sano: is_enabled, cf=0).
/// Campi separati per testabilita' della logica di selezione.
#[derive(Debug, Clone, PartialEq)]
struct HealCandidate {
    model: String,
    is_featured: bool,
    context_window: i64,
    input_cost: f64,
}

/// "Family stem" di un nome modello: rimuove l'ultimo segmento se e' una
/// versione (data/numero/"latest"). Serve a riconoscere il SUCCESSORE di un
/// modello deprecato nella stessa famiglia.
///   mistral-large-2411  -> mistral-large
///   mistral-large-latest -> mistral-large
///   ministral-8b-2512   -> ministral-8b   (8b non e' una versione pura)
///   gpt-4o-2024-05-13   -> gpt-4o-2024-05 (rimuove solo l'ultimo; approssimato
///                                          ma sufficiente per il match di famiglia)
fn model_stem(model: &str) -> &str {
    match model.rsplit_once('-') {
        Some((head, tail)) => {
            let is_version =
                tail.eq_ignore_ascii_case("latest") || tail.chars().all(|c| c.is_ascii_digit());
            if is_version && !head.is_empty() {
                head
            } else {
                model
            }
        }
        None => model,
    }
}

/// Sceglie il miglior sostituto tra candidati dello stesso (provider, tier).
///
/// CRITICO: il `performance_tier` di un provider puo' mescolare modelli di
/// capacita' molto diversa (es. Mistral 'medium' contiene sia ministral-3b sia
/// mistral-large). Per non degradare, si PREFERISCONO i candidati della STESSA
/// famiglia del modello deprecato (stesso `model_stem`): il successore naturale
/// di `mistral-large-2411` e' `mistral-large-latest`, non `ministral-8b`.
/// Solo se nessun candidato condivide la famiglia si ricade su tutti.
///
/// Ordine a parita' di famiglia: featured, poi context window, poi costo basso.
fn pick_best_replacement(dead_model: &str, candidates: &[HealCandidate]) -> Option<String> {
    let dead_stem = model_stem(dead_model);
    let same_family: Vec<&HealCandidate> = candidates
        .iter()
        .filter(|c| model_stem(&c.model) == dead_stem)
        .collect();
    let pool: Vec<&HealCandidate> = if same_family.is_empty() {
        candidates.iter().collect()
    } else {
        same_family
    };
    pool.into_iter()
        .max_by(|a, b| {
            a.is_featured
                .cmp(&b.is_featured)
                .then(a.context_window.cmp(&b.context_window))
                .then(
                    b.input_cost
                        .partial_cmp(&a.input_cost)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        })
        .map(|c| c.model.clone())
}

/// Sana le righe matrix che puntano a modelli `missing_from_api`, sostituendo
/// il model_id con il miglior sostituto sano dello stesso provider+tier.
/// RISPETTA `manual_override` (cambia solo il model_id morto, non il pin).
/// Ritorna il numero di righe sanate.
pub async fn heal_orphan_pinned_models(db: &PgPool) -> sqlx::Result<u64> {
    // Flag enforcement (settings, regola G — niente env/hardcode).
    let enabled = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.routing_matrix_heal_orphan_enabled'",
    )
    .fetch_optional(db)
    .await?
    .map(|v| {
        !matches!(
            v.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
    .unwrap_or(true);
    if !enabled {
        tracing::info!(
            "routing_matrix_auto_promoter: heal_orphan_pinned_models DISABILITATO (flag)"
        );
        return Ok(0);
    }

    // 1. Trova i (provider, dead_model, tier) distinti referenziati da righe
    //    matrix ATTIVE il cui modello e' missing_from_api nel catalog.
    let orphans = sqlx::query(
        "SELECT DISTINCT m.provider, m.model_id AS dead_model, c.performance_tier AS tier
           FROM nexus_routing_matrix m
           JOIN ai_price_catalog c
             ON LOWER(c.provider) = LOWER(m.provider) AND c.model = m.model_id
          WHERE m.is_active = true
            AND c.auto_disabled_reason = 'missing_from_api'",
    )
    .fetch_all(db)
    .await?;

    let mut healed_total: u64 = 0;
    for o in &orphans {
        let provider: String = o.try_get("provider").unwrap_or_default();
        let dead_model: String = o.try_get("dead_model").unwrap_or_default();
        let tier: String = o.try_get("tier").unwrap_or_default();

        // 2. Carica i candidati abilitati dello stesso (provider, tier).
        //    Salute = solo is_enabled (ADR 0025, come load_catalog e il cleanup):
        //    pretendere consecutive_failures=0 lasciava il modello deprecato
        //    senza sostituto quando tutti i candidati avevano counter stale.
        let cand_rows = sqlx::query(
            "SELECT model, COALESCE(is_featured, false) AS is_featured,
                    COALESCE(context_window, 8192) AS context_window,
                    input_cost_per_million_tokens::float8 AS input_cost
               FROM ai_price_catalog
              WHERE LOWER(provider) = LOWER($1)
                AND performance_tier = $2
                AND is_enabled = true
                AND model <> $3",
        )
        .bind(&provider)
        .bind(&tier)
        .bind(&dead_model)
        .fetch_all(db)
        .await?;

        let candidates: Vec<HealCandidate> = cand_rows
            .iter()
            .map(|r| HealCandidate {
                model: r.try_get("model").unwrap_or_default(),
                is_featured: r.try_get("is_featured").unwrap_or(false),
                context_window: r.try_get("context_window").unwrap_or(8192),
                input_cost: r.try_get("input_cost").unwrap_or(0.0),
            })
            .collect();

        let Some(replacement) = pick_best_replacement(&dead_model, &candidates) else {
            tracing::warn!(
                provider = %provider, dead_model = %dead_model, tier = %tier,
                "heal_orphan_pinned_models: nessun sostituto sano per il modello deprecato — riga lasciata al cleanup"
            );
            continue;
        };

        // 3. UPDATE tutte le righe (qualsiasi manual_override) che puntano al
        //    modello morto: cambia SOLO il model_id, preserva il pin.
        let note =
            format!(" [auto-heal: {dead_model} deprecato (missing_from_api) -> {replacement}]");
        let res = sqlx::query(
            "UPDATE nexus_routing_matrix
                SET model_id = $1,
                    notes = COALESCE(notes, '') ||
                            CASE WHEN COALESCE(notes,'') LIKE '%' || $2 || '%'
                                 THEN '' ELSE $2 END,
                    updated_at = NOW()
              WHERE LOWER(provider) = LOWER($3)
                AND model_id = $4
                AND is_active = true",
        )
        .bind(&replacement)
        .bind(&note)
        .bind(&provider)
        .bind(&dead_model)
        .execute(db)
        .await?;
        let n = res.rows_affected();
        if n > 0 {
            tracing::info!(
                provider = %provider, dead_model = %dead_model, replacement = %replacement, rows = n,
                "heal_orphan_pinned_models: sanate {n} righe (modello deprecato sostituito)"
            );
            healed_total += n;
        }
    }

    Ok(healed_total)
}

async fn load_requirements(db: &PgPool) -> sqlx::Result<Vec<IntentRequirement>> {
    let rows = sqlx::query(
        "SELECT intent, behavior_mode, required_capabilities, requires_tool_use,
                preferred_tier, weight_tier, weight_cost, weight_context,
                weight_capabilities, cost_direction
           FROM nexus_intent_routing_requirements
          WHERE intent <> '*'
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
            preferred_tier: r
                .try_get("preferred_tier")
                .unwrap_or_else(|_| "medium".into()),
            weight_tier: r.try_get("weight_tier").unwrap_or(0.35),
            weight_cost: r.try_get("weight_cost").unwrap_or(0.25),
            weight_context: r.try_get("weight_context").unwrap_or(0.20),
            weight_capabilities: r.try_get("weight_capabilities").unwrap_or(0.20),
            cost_direction: r.try_get("cost_direction").unwrap_or_else(|_| "asc".into()),
        })
        .collect())
}

async fn load_catalog(db: &PgPool) -> sqlx::Result<Vec<CatalogModel>> {
    // FASE 2b (regola L, allineamento al routing live / ADR 0025): si carica il
    // catalog SANO con il solo `is_enabled = true`. Il filtro
    // `consecutive_failures = 0` e' stato RIMOSSO: la salute e' gia' garantita da
    // is_enabled (il model_health_probe auto-disabilita a soglia) e filtrarlo
    // causava starvation (un modello con 1 fail transitorio sparito dal pool e
    // mai piu' scelto -> counter mai resettato). Identica scelta di
    // select_models_tierchain (path live).
    let rows = sqlx::query(
        "SELECT provider, model, performance_tier,
                input_cost_per_million_tokens::float8 AS input_cost,
                context_window, supports_tool_use,
                capabilities, agentic_thinking_policy, pricing_state
           FROM ai_price_catalog
          WHERE is_enabled = true",
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
                agentic_thinking_policy: r
                    .try_get("agentic_thinking_policy")
                    .unwrap_or_else(|_| "none".into()),
                pricing_state: r
                    .try_get("pricing_state")
                    .unwrap_or_else(|_| "unknown".into()),
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
            // FASE 2b: per gli slot che richiedono tool_use, escludi i modelli
            // con agentic_thinking_policy='exclude' (allineamento al routing live,
            // ADR 0025): cosi' la matrix non pinna modelli che il live scarta.
            if req.requires_tool_use && m.agentic_thinking_policy == "exclude" {
                return false;
            }
            if !req.required_capabilities.is_empty() && !m.capabilities.is_empty() {
                let required_lc: Vec<String> = req
                    .required_capabilities
                    .iter()
                    .map(|s| s.to_lowercase())
                    .collect();
                let cap_lc: Vec<String> = m.capabilities.iter().map(|s| s.to_lowercase()).collect();
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

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

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

/// Punto unico di selezione modello per un requisito tier+capability (regola L).
///
/// Carica il catalog ABILITATO (`is_enabled = true`; salute = solo is_enabled,
/// ADR 0025) e usa lo STESSO scoring di `select_top_candidates` che governa la
/// `nexus_routing_matrix` per intent. Ritorna i `(provider, model)` ordinati per
/// score (uno per provider, max 3): il chiamante puo' scorrere la lista
/// saltando i provider in cooldown.
///
/// Riusato da:
///   - questo modulo (popolamento matrix per `(intent, behavior_mode)`);
///   - `Orchestrator::route_by_slots` (routing slot-based, mig 0357): la
///     `nexus_routing_slots_matrix` esprime solo tier+capability, mentre il
///     provider+modello concreto viene scelto QUI. Niente piu' modelli pinnati
///     che marciscono (regola G/H).
///
/// I pesi di scoring provengono dalla riga sentinella `intent='*'` di
/// `nexus_intent_routing_requirements`, lette via
/// `orchestrator::default_scoring_weights` (regola G: niente pesi hardcoded;
/// punto unico dei pesi di default, condiviso con le viste runtime in FASE 2).
/// `cost_direction` resta una scelta esplicita del chiamante.
pub(crate) async fn select_models_for_requirement(
    db: &PgPool,
    preferred_tier: &str,
    required_capabilities: &[String],
    requires_tool_use: bool,
    cost_direction: &str,
) -> sqlx::Result<Vec<(String, String)>> {
    let catalog = load_catalog(db).await?;
    let weights = crate::orchestrator::default_scoring_weights(db)
        .await
        .map_err(sqlx::Error::Protocol)?;
    let req = IntentRequirement {
        intent: String::new(),
        behavior_mode: String::new(),
        required_capabilities: required_capabilities.to_vec(),
        requires_tool_use,
        preferred_tier: preferred_tier.to_string(),
        weight_tier: weights.tier,
        weight_cost: weights.cost,
        weight_context: weights.context,
        weight_capabilities: weights.capabilities,
        cost_direction: cost_direction.to_string(),
    };
    Ok(select_top_candidates(&req, &catalog)
        .into_iter()
        .map(|s| (s.catalog.provider, s.catalog.model))
        .collect())
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
        let required_lc: Vec<String> = req
            .required_capabilities
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        let cap_lc: Vec<String> = m.capabilities.iter().map(|s| s.to_lowercase()).collect();
        capability_match_pct(&required_lc, &cap_lc)
    };

    req.weight_tier * tier_score
        + req.weight_cost * cost_score
        + req.weight_context * context_score
        + req.weight_capabilities * cap_score
}

pub(crate) fn tier_score(preferred: &str, actual: &str) -> f32 {
    let pref_rank = tier_rank(preferred);
    let actual_rank = tier_rank(actual);
    let diff = (pref_rank - actual_rank).abs();
    match diff {
        0 => 1.0,
        1 => 0.5,
        _ => 0.0,
    }
}

/// Rank 0-based del performance-tier (`light`=0 ... `frontier`=4; sconosciuto ->
/// `medium`=1). DELEGA al PUNTO UNICO del vocabolario tier
/// ([`nexus_agent_graph::decisions::tiers::tier_rank`], scala 1..=5) e lo riporta
/// alla base 0 usata qui: nessuna lista di tier duplicata (regola L).
pub(crate) fn tier_rank(tier: &str) -> i32 {
    nexus_agent_graph::decisions::tiers::tier_rank(tier) as i32 - 1
}

fn cost_score(req: &IntentRequirement, m: &CatalogModel, catalog: &[CatalogModel]) -> f32 {
    // pricing_state (mig 0477) disambigua il significato di input_cost==0:
    //  - 'free'    = gratuito reale -> costo 0 effettivo: per 'asc' e' l'ottimo
    //                (score 1.0); per 'desc' (costo alto = proxy capability) e' il
    //                peggio (0.0). Resta FUORI dal pool min/max sotto: il suo 0 non
    //                deve schiacciare la normalizzazione dei modelli 'priced'.
    //  - 'unknown' = prezzo placeholder/ignoto -> score neutro 0.5, escluso dal
    //                pool min/max (oggi sono comunque is_enabled=false, ma il
    //                trattamento esplicito li rende non-ottimi anche se abilitati).
    //  - 'priced'  = comportamento storico (normalizzazione lineare sul pool).
    match m.pricing_state.as_str() {
        "free" => {
            return if req.cost_direction == "asc" {
                1.0
            } else {
                0.0
            }
        }
        "priced" => {}
        _ => return 0.5, // 'unknown' e qualsiasi valore non riconosciuto: neutro.
    }

    // Normalizza il costo sul pool di candidati passato (l'INTERO catalog
    // eleggibile ricevuto da score_model, NON filtrato per tier). min/max sono
    // calcolati SOLO sui modelli 'priced' con costo > 0: 'free'/'unknown' non
    // entrano nella normalizzazione. In pool piccoli la normalizzazione e'
    // sensibile al range (nota per FASE 2).
    let costs: Vec<f64> = catalog
        .iter()
        .filter(|c| c.pricing_state == "priced")
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

pub(crate) fn context_score(ctx: i32) -> f32 {
    // log scale: 8k = 0, 1M = 1.
    if ctx <= 8192 {
        return 0.0;
    }
    let log_ctx = (ctx as f32).log2();
    let min_log = (8192_f32).log2(); // 13
    let max_log = (1_048_576_f32).log2(); // 20
    ((log_ctx - min_log) / (max_log - min_log)).clamp(0.0, 1.0)
}

pub(crate) fn capability_match_pct(required: &[String], available: &[String]) -> f32 {
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

    fn model(
        p: &str,
        name: &str,
        tier: &str,
        cost: f64,
        ctx: i32,
        tools: bool,
        caps: Vec<&str>,
    ) -> CatalogModel {
        CatalogModel {
            provider: p.into(),
            model: name.into(),
            performance_tier: tier.into(),
            input_cost: cost,
            context_window: ctx,
            supports_tool_use: tools,
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            agentic_thinking_policy: "none".into(),
            // Default 'priced': gli helper di test usano costi reali > 0; lo stato
            // 'free'/'unknown' viene impostato esplicitamente nei test dedicati.
            pricing_state: "priced".into(),
        }
    }

    fn model_with_policy(
        p: &str,
        name: &str,
        tier: &str,
        tools: bool,
        policy: &str,
    ) -> CatalogModel {
        let mut m = model(
            p,
            name,
            tier,
            1.0,
            200_000,
            tools,
            vec!["code", "reasoning"],
        );
        m.agentic_thinking_policy = policy.into();
        m
    }

    fn req(
        intent: &str,
        behavior: &str,
        caps: Vec<&str>,
        tools: bool,
        tier: &str,
        cost_dir: &str,
    ) -> IntentRequirement {
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
        // Scala a 5 (light<medium<high<heavy<frontier): coppie a distanza 1 -> 0.5.
        assert_eq!(tier_score("frontier", "heavy"), 0.5);
        assert_eq!(tier_score("heavy", "high"), 0.5);
        assert_eq!(tier_score("high", "medium"), 0.5);
        assert_eq!(tier_score("medium", "light"), 0.5);
        // heavy e medium ora NON sono adiacenti (c'e' 'high' in mezzo) -> 0.0.
        assert_eq!(tier_score("heavy", "medium"), 0.0);
    }

    #[test]
    fn tier_score_far_zero() {
        assert_eq!(tier_score("heavy", "light"), 0.0);
        assert_eq!(tier_score("frontier", "medium"), 0.0);
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
    fn cost_score_free_is_optimal_for_asc() {
        // Un modello 'free' (gratuito reale, costo 0) deve valere score massimo
        // per cost_direction='asc' (costo basso = ottimo) e minimo per 'desc'.
        let mut free = model("z", "free-model", "light", 0.0, 8192, true, vec![]);
        free.pricing_state = "free".into();
        let priced = model("a", "cheap", "light", 0.1, 8192, true, vec![]);
        let catalog = vec![free.clone(), priced];

        let r_asc = req("test", "veloce", vec![], false, "light", "asc");
        assert!((cost_score(&r_asc, &catalog[0], &catalog) - 1.0).abs() < f32::EPSILON);

        let r_desc = req("test", "approfondita", vec![], false, "heavy", "desc");
        assert!((cost_score(&r_desc, &catalog[0], &catalog) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cost_score_unknown_is_neutral_and_excluded_from_pool() {
        // Un modello 'unknown' (placeholder a costo 0) riceve score neutro 0.5 e
        // NON contamina min/max del pool: i 'priced' restano normalizzati tra loro.
        let mut unknown = model("z", "unknown-model", "light", 0.0, 8192, true, vec![]);
        unknown.pricing_state = "unknown".into();
        let cheap = model("a", "cheap", "light", 1.0, 8192, true, vec![]);
        let expensive = model("b", "expensive", "heavy", 10.0, 200000, true, vec![]);
        let catalog = vec![unknown.clone(), cheap, expensive];

        let r = req("test", "veloce", vec![], false, "light", "asc");
        // L'unknown e' neutro.
        assert!((cost_score(&r, &catalog[0], &catalog) - 0.5).abs() < f32::EPSILON);
        // Il piu' economico tra i 'priced' resta l'ottimo (score 1.0 per asc).
        assert!((cost_score(&r, &catalog[1], &catalog) - 1.0).abs() < f32::EPSILON);
        // Il piu' costoso tra i 'priced' resta il peggiore (score 0.0 per asc).
        assert!((cost_score(&r, &catalog[2], &catalog) - 0.0).abs() < f32::EPSILON);
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
        assert_eq!(
            capability_match_pct(&req_caps, &["code".into(), "fix".into()]),
            1.0
        );
        assert_eq!(capability_match_pct(&req_caps, &["code".into()]), 0.5);
        assert_eq!(capability_match_pct(&req_caps, &[]), 0.0);
    }

    #[test]
    fn select_top_excludes_policy_exclude_when_tool() {
        let catalog = vec![
            model_with_policy("a", "escluso", "heavy", true, "exclude"),
            model_with_policy("b", "ok", "heavy", true, "none"),
        ];
        let r = req(
            "agentic_default",
            "approfondita",
            vec!["code"],
            true,
            "heavy",
            "asc",
        );
        let top = select_top_candidates(&r, &catalog);
        assert!(
            top.iter().all(|s| s.catalog.model != "escluso"),
            "agentic_thinking_policy='exclude' non deve essere candidabile per slot tool"
        );
        assert!(top.iter().any(|s| s.catalog.model == "ok"));
    }

    #[test]
    fn select_top_filters_tool_use() {
        let catalog = vec![
            model("a", "no-tools", "heavy", 1.0, 200000, false, vec!["code"]),
            model("b", "with-tools", "heavy", 1.0, 200000, true, vec!["code"]),
        ];
        let r = req(
            "fix_complesso",
            "approfondita",
            vec!["code"],
            true,
            "heavy",
            "desc",
        );
        let top = select_top_candidates(&r, &catalog);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].catalog.model, "with-tools");
    }

    #[test]
    fn select_top_one_per_provider() {
        let catalog = vec![
            model(
                "anthropic",
                "claude-opus",
                "heavy",
                5.0,
                200000,
                true,
                vec!["code", "reasoning"],
            ),
            model(
                "anthropic",
                "claude-sonnet",
                "medium",
                3.0,
                200000,
                true,
                vec!["code"],
            ),
            model(
                "openai",
                "gpt-5",
                "heavy",
                4.0,
                128000,
                true,
                vec!["code", "reasoning"],
            ),
            model(
                "google",
                "gemini-pro",
                "heavy",
                1.25,
                1_000_000,
                true,
                vec!["code", "reasoning", "long-context"],
            ),
        ];
        let r = req(
            "fix_complesso",
            "approfondita",
            vec!["code", "reasoning"],
            true,
            "heavy",
            "desc",
        );
        let top = select_top_candidates(&r, &catalog);
        // Max 3 provider, top-1 per provider
        assert_eq!(top.len(), 3);
        let providers: std::collections::HashSet<_> =
            top.iter().map(|s| s.catalog.provider.clone()).collect();
        assert_eq!(providers.len(), 3);
        // Solo "claude-opus" tra anthropic (top-1)
        let anthropic_pick = top
            .iter()
            .find(|s| s.catalog.provider == "anthropic")
            .unwrap();
        assert_eq!(anthropic_pick.catalog.model, "claude-opus");
    }

    #[test]
    fn select_top_excludes_low_capability_match() {
        let catalog = vec![
            model("a", "weak", "medium", 1.0, 100000, true, vec!["chat"]), // 0 di 2 capability
            model("b", "ok", "medium", 1.0, 100000, true, vec!["code", "fix"]), // 2 di 2
        ];
        let r = req(
            "fix_complesso",
            "bilanciata",
            vec!["code", "fix"],
            true,
            "medium",
            "asc",
        );
        let top = select_top_candidates(&r, &catalog);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].catalog.model, "ok");
    }

    // ── Cleanup pass (A) ─────────────────────────────────────────────────
    // Spec eseguibile della query SQL di `cleanup_stale_rows`: la condizione
    // di disattivazione e' replicata qui come funzione pura per i test di
    // regressione (la produzione esegue direttamente l'UPDATE SQL).

    /// Stato minimale di una riga matrix per la decisione di cleanup (testabile).
    #[derive(Debug, Clone)]
    struct MatrixRowRef {
        provider: String,
        model_id: String,
        is_active: bool,
        manual_override: bool,
    }

    /// Stato minimale di un modello del catalog per la decisione di cleanup.
    #[derive(Debug, Clone)]
    struct CatalogHealthRef {
        provider: String,
        model: String,
        is_enabled: bool,
        consecutive_failures: i32,
    }

    /// Regola PURA: la riga matrix va disattivata dal cleanup?
    /// True sse: e' attiva, non manuale, e nel catalog NON esiste un modello
    /// ABILITATO (`is_enabled=true`) con stesso provider (case-insensitive) e
    /// stesso model_id. La salute e' SOLO is_enabled (ADR 0025): pretendere
    /// consecutive_failures=0 creava starvation (riga spenta -> zero traffico ->
    /// counter mai azzerato -> riga spenta per sempre, incidente 2026-07-02).
    /// Identica alla condizione della query SQL di `cleanup_stale_rows`.
    fn row_should_be_deactivated(row: &MatrixRowRef, catalog: &[CatalogHealthRef]) -> bool {
        if !row.is_active || row.manual_override {
            return false;
        }
        let has_healthy = catalog.iter().any(|c| {
            c.provider.eq_ignore_ascii_case(&row.provider)
                && c.model == row.model_id
                && c.is_enabled
        });
        !has_healthy
    }

    fn mrow(provider: &str, model_id: &str, active: bool, manual: bool) -> MatrixRowRef {
        MatrixRowRef {
            provider: provider.into(),
            model_id: model_id.into(),
            is_active: active,
            manual_override: manual,
        }
    }
    fn chealth(provider: &str, model: &str, enabled: bool, failures: i32) -> CatalogHealthRef {
        CatalogHealthRef {
            provider: provider.into(),
            model: model.into(),
            is_enabled: enabled,
            consecutive_failures: failures,
        }
    }

    #[test]
    fn cleanup_deactivates_row_with_disabled_model() {
        // Modello disabilitato nel catalog -> riga matrix attiva non-manuale stale.
        let row = mrow("google", "gemini-2.5-pro", true, false);
        let catalog = vec![chealth("google", "gemini-2.5-pro", false, 0)];
        assert!(row_should_be_deactivated(&row, &catalog));
    }

    #[test]
    fn cleanup_keeps_row_with_failing_but_enabled_model() {
        // REGRESSIONE incidente 2026-07-02: modello enabled con
        // consecutive_failures>0 e' ancora "sano" per il cleanup (la salute e'
        // solo is_enabled, ADR 0025). Il vecchio filtro failures=0 spegneva la
        // riga -> zero traffico -> counter mai azzerato -> starvation.
        let row = mrow("openai", "gpt-5", true, false);
        let catalog = vec![chealth("openai", "gpt-5", true, 2)];
        assert!(!row_should_be_deactivated(&row, &catalog));
    }

    #[test]
    fn cleanup_keeps_row_with_healthy_model() {
        let row = mrow("anthropic", "claude-opus", true, false);
        let catalog = vec![chealth("anthropic", "claude-opus", true, 0)];
        assert!(!row_should_be_deactivated(&row, &catalog));
    }

    #[test]
    fn cleanup_respects_manual_override() {
        // Anche se il modello e' broken nel catalog, manual_override=true non si tocca MAI.
        let row = mrow("google", "gemini-2.5-pro", true, true);
        let catalog = vec![chealth("google", "gemini-2.5-pro", false, 5)];
        assert!(!row_should_be_deactivated(&row, &catalog));
    }

    #[test]
    fn cleanup_idempotent_on_already_inactive() {
        // Riga gia' disattivata: non rientra nel cleanup (idempotenza).
        let row = mrow("google", "gemini-2.5-pro", false, false);
        let catalog = vec![chealth("google", "gemini-2.5-pro", false, 0)];
        assert!(!row_should_be_deactivated(&row, &catalog));
    }

    #[test]
    fn cleanup_provider_case_insensitive() {
        // provider 'Google' nella matrix vs 'google' nel catalog: match sano -> tieni.
        let row = mrow("Google", "gemini-2.5-pro", true, false);
        let catalog = vec![chealth("google", "gemini-2.5-pro", true, 0)];
        assert!(!row_should_be_deactivated(&row, &catalog));
    }

    #[test]
    fn score_model_economica_prefers_light_cheap() {
        let catalog = vec![
            model("a", "light-cheap", "light", 0.1, 100000, true, vec!["code"]),
            model(
                "b",
                "heavy-expensive",
                "heavy",
                5.0,
                200000,
                true,
                vec!["code"],
            ),
        ];
        let r = req(
            "fix_semplice",
            "economica",
            vec!["code"],
            true,
            "light",
            "asc",
        );
        let s_light = score_model(&r, &catalog[0], &catalog);
        let s_heavy = score_model(&r, &catalog[1], &catalog);
        // "economica" + tier=light + cost_dir=asc → light-cheap deve vincere
        assert!(
            s_light > s_heavy,
            "economica deve preferire light-cheap ({s_light}) > heavy-expensive ({s_heavy})"
        );
    }

    // ── Test heal_orphan_pinned_models::pick_best_replacement ────────────────

    fn heal_cand(model: &str, featured: bool, ctx: i64, cost: f64) -> HealCandidate {
        HealCandidate {
            model: model.to_string(),
            is_featured: featured,
            context_window: ctx,
            input_cost: cost,
        }
    }

    #[test]
    fn heal_stem_extraction() {
        assert_eq!(model_stem("mistral-large-2411"), "mistral-large");
        assert_eq!(model_stem("mistral-large-latest"), "mistral-large");
        assert_eq!(model_stem("mistral-large-2512"), "mistral-large");
        assert_eq!(model_stem("ministral-8b-2512"), "ministral-8b");
        assert_eq!(model_stem("claude-sonnet"), "claude-sonnet"); // niente versione
    }

    #[test]
    fn heal_pick_none_when_empty() {
        assert_eq!(pick_best_replacement("mistral-large-2411", &[]), None);
    }

    #[test]
    fn heal_pick_prefers_featured_same_family() {
        // featured vince, MA solo tra la stessa famiglia. Qui entrambi family.
        let cands = vec![
            heal_cand("x-big", false, 1_000_000, 0.1),
            heal_cand("x-small", true, 8_192, 9.0),
        ];
        assert_eq!(
            pick_best_replacement("x-2411", &cands).as_deref(),
            Some("x-small")
        );
    }

    #[test]
    fn heal_pick_context_then_cost_same_family() {
        let cands = vec![
            heal_cand("x-small-ctx", false, 8_192, 0.1),
            heal_cand("x-big-ctx", false, 200_000, 5.0),
        ];
        assert_eq!(
            pick_best_replacement("x-0000", &cands).as_deref(),
            Some("x-big-ctx")
        );
    }

    #[test]
    fn heal_realcase_mistral_large_prefers_same_family() {
        // CASO REALE del bug: mistral-large-2411 deprecato. Candidati medium
        // sani includono ministral (piccolo, economico, context grande) e
        // mistral-large-latest. La famiglia deve vincere: NON deve scegliere
        // ministral-8b solo perche' costa meno.
        let cands = vec![
            heal_cand("ministral-8b-2512", false, 128_000, 0.10),
            heal_cand("ministral-3b-latest", false, 128_000, 0.04),
            heal_cand("mistral-large-latest", false, 128_000, 2.0),
            heal_cand("mistral-large-2512", false, 128_000, 2.0),
            heal_cand("mistral-small-2506", false, 32_000, 0.2),
        ];
        // dead stem = "mistral-large" -> candidati family: large-latest, large-2512
        // (entrambi non featured, stesso context, stesso costo) -> il primo per
        // l'ordine stabile max_by. Verifichiamo che sia UNO dei due large.
        let pick = pick_best_replacement("mistral-large-2411", &cands).unwrap();
        assert!(
            pick.starts_with("mistral-large"),
            "atteso un mistral-large-*, ottenuto {pick}"
        );
    }

    #[test]
    fn heal_fallback_when_no_same_family() {
        // Se nessun candidato condivide la famiglia, ricade su tutti
        // (featured > context > cost).
        let cands = vec![
            heal_cand("other-a", false, 8_192, 1.0),
            heal_cand("other-b", true, 8_192, 5.0),
        ];
        assert_eq!(
            pick_best_replacement("deadfamily-2411", &cands).as_deref(),
            Some("other-b")
        );
    }
}
