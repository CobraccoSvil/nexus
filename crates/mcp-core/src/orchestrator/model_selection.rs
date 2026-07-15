//! Punto unico (regola L) dei sotto-componenti CONDIVISI della selezione di un
//! modello dal `ai_price_catalog`.
//!
//! FASE 1 del consolidamento del selettore modello (vedi ADR 0030): questo
//! modulo elimina le duplicazioni ACCIDENTALI (pesi di scoring hardcoded,
//! normalizzazione case-insensitive dei provider esclusi sparsa/incoerente)
//! SENZA cambiare alcun comportamento osservabile dei call site. Il selettore
//! unico vero e proprio (`EligibilityFilter` + `RankStrategy` + `select_models`)
//! arrivera' in FASE 2 e vivra' qui.
//!
//! Regole applicate:
//!   - G: i pesi di scoring NON sono hardcoded nel codice; vengono dalla riga
//!     sentinella `intent='*'` di `nexus_intent_routing_requirements`. Se la
//!     riga manca, il sistema FALLISCE in modo visibile (Err), niente fallback.
//!   - L: un solo posto sa come si costruisce la lista dei provider esclusi
//!     (cooldown snapshot + extra del chiamante, tutti lowercase) e quali sono
//!     i pesi di default.

use nexus_cache::TtlCache;
use sqlx::{PgPool, Row};
use std::sync::OnceLock;
use std::time::Duration;

/// Pesi dello scoring multi-fattore usato dall'auto-promoter e dal routing
/// slot-based. Fonte unica: riga sentinella DB (regola G).
#[derive(Debug, Clone)]
pub(crate) struct ScoringWeights {
    pub tier: f32,
    pub cost: f32,
    pub context: f32,
    pub capabilities: f32,
}

/// Chiave/valore della riga sentinella dei pesi di default.
const DEFAULT_WEIGHTS_KEY: &str = "*";

static WEIGHTS_CACHE: OnceLock<TtlCache<String, ScoringWeights>> = OnceLock::new();

fn weights_cache() -> &'static TtlCache<String, ScoringWeights> {
    WEIGHTS_CACHE.get_or_init(|| TtlCache::new(Duration::from_secs(60)))
}

/// Legge i pesi di default dalla riga sentinella `intent='*', behavior_mode='*'`
/// di `nexus_intent_routing_requirements`. NESSUNA cache (per testabilita'
/// isolata): il wrapper con cache e' `default_scoring_weights`.
///
/// Regola G: se la riga sentinella non esiste ritorna `Err` (fail visibile,
/// niente pesi hardcoded di emergenza). I campi sono letti senza `unwrap_or`
/// numerico: un errore di deserializzazione si propaga invece di mascherare un
/// peso fittizio.
async fn fetch_default_weights(db: &PgPool) -> Result<ScoringWeights, String> {
    let row = sqlx::query(
        "SELECT weight_tier, weight_cost, weight_context, weight_capabilities \
           FROM nexus_intent_routing_requirements \
          WHERE intent = '*' AND behavior_mode = '*'",
    )
    .fetch_optional(db)
    .await
    .map_err(|e| format!("default_scoring_weights: query fallita: {e}"))?
    .ok_or_else(|| {
        "default_scoring_weights: riga sentinella intent='*'/behavior_mode='*' assente in \
         nexus_intent_routing_requirements; applicare la migrazione 0379 dei pesi di default \
         (regola G: nessun fallback hardcoded)"
            .to_string()
    })?;
    Ok(ScoringWeights {
        tier: row
            .try_get("weight_tier")
            .map_err(|e| format!("default_scoring_weights: weight_tier: {e}"))?,
        cost: row
            .try_get("weight_cost")
            .map_err(|e| format!("default_scoring_weights: weight_cost: {e}"))?,
        context: row
            .try_get("weight_context")
            .map_err(|e| format!("default_scoring_weights: weight_context: {e}"))?,
        capabilities: row
            .try_get("weight_capabilities")
            .map_err(|e| format!("default_scoring_weights: weight_capabilities: {e}"))?,
    })
}

/// Pesi di scoring di default, con cache 60s (TtlCache, punto unico cache,
/// regola L). Usato dal routing slot-based (`select_models_for_requirement`)
/// e in FASE 2 dalle viste runtime. Regola G: niente pesi hardcoded.
pub(crate) async fn default_scoring_weights(db: &PgPool) -> Result<ScoringWeights, String> {
    if let Some(w) = weights_cache().get(DEFAULT_WEIGHTS_KEY) {
        return Ok(w);
    }
    let w = fetch_default_weights(db).await?;
    weights_cache().insert(DEFAULT_WEIGHTS_KEY.to_string(), w.clone());
    Ok(w)
}

/// Costruisce la lista dei provider da escludere dalla selezione, normalizzati
/// in lowercase (regola L): provider attualmente in cooldown (snapshot
/// in-memory) PIU' gli `extra` indicati dal chiamante, deduplicati.
///
/// Punto unico della normalizzazione: prima alcuni call site facevano
/// `LOWER(provider)` mentre il ramo non-agentico di `best_model_for_tier`
/// confrontava il nome RAW del catalog contro lo snapshot (gia' lowercase),
/// con possibile mismatch. Qui la sorgente e' una sola.
pub(crate) fn excluded_providers_lower(extra: &[String]) -> Vec<String> {
    let mut excluded: Vec<String> = crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .map(|(name, _, _)| name.to_lowercase())
        .collect();
    for p in extra {
        let pl = p.to_lowercase();
        if !excluded.contains(&pl) {
            excluded.push(pl);
        }
    }
    excluded
}

/// Predicato di ELEGGIBILITA' di un modello del catalog (FASE 2, regola L).
///
/// Un solo posto definisce QUALI filtri si applicano; i call site scelgono i
/// flag. Sostituisce le WHERE duplicate di `select_agentic_model` (path
/// agentico) e del ramo non-agentico inline di `best_model_for_tier`.
///
/// NON include `consecutive_failures`: la salute e' gia' garantita da
/// `is_enabled = TRUE` (il `model_health_probe` auto-disabilita a soglia) e
/// filtrare `consecutive_failures = 0` causerebbe starvation (ADR 0025).
#[derive(Debug, Clone)]
pub(super) struct EligibilityFilter<'a> {
    /// `true` => `AND supports_tool_use = TRUE` (path agentico).
    pub require_tool_use: bool,
    /// `true` => `AND agentic_thinking_policy <> 'exclude'` e abilita il
    /// TIE-BREAKER `(agentic_thinking_policy = 'none') DESC` come ULTIMO criterio
    /// di ORDER BY (ADR 0025, declassato: era pre-ordinamento PRIMARIO, ma con i
    /// modelli forti moderni ormai tutti dual-mode escludeva i migliori a favore
    /// dei completion/legacy; l'affidabilita' sotto `tool_choice` e' garantita dal
    /// gateway). Per il path non-agentico (vision/embedding) il thinking non si
    /// applica -> `false`, niente tie-breaker.
    pub require_thinking_non_exclude: bool,
    /// Capability richiesta. Le capability con una COLONNA canonica dedicata
    /// (vision + i media kind image_gen/audio_in/audio_out/video_gen, mig 0478)
    /// si filtrano via `AND supports_<x> = TRUE` (vedi `capability_to_column`);
    /// ogni altra capability => `capabilities @> ["c"]` nel jsonb. Quando la
    /// capability richiesta NON e' un media kind, i modelli media vengono ESCLUSI
    /// (un image-gen non risale la classifica dei purpose testuali).
    pub capability: Option<&'a str>,
    /// `>0` => `AND context_window >= N`.
    pub min_context_window: i64,
    /// Provider extra da escludere (oltre al cooldown se `apply_cooldown`).
    pub exclude_providers: &'a [String],
    /// `true` => esclude anche i provider attualmente in cooldown (snapshot).
    pub apply_cooldown: bool,
    /// `Some(p)` => RESTRINGE la selezione al SOLO provider `p` (filtro POSITIVO
    /// `AND LOWER(provider) = LOWER(p)`), oltre a tutti gli altri filtri. Usato
    /// per la propagazione del PIN del provider ai sub-agenti worker: il pin e'
    /// una preferenza-forte tier-aware (tier+capability+tool_use invariati, solo
    /// il provider e' vincolato). `None` (default storico) => nessuna restrizione,
    /// comportamento bit-identico per i ~13 costruttori esistenti. Il valore e'
    /// BINDATO (mai interpolato): niente SQL injection.
    pub only_provider: Option<&'a str>,
    /// `true` => richiede l'EVIDENZA che il modello regga il profilo d'uso reale
    /// (gate di qualificazione, mig 0591): `qualification_state = 'qualified'`
    /// non scaduto, e le capability jsonb si filtrano su `qualified_capabilities`
    /// (PROVATE dal qualificatore) invece di `capabilities` (dichiarate).
    /// Distinto da `require_tool_use`: dichiarato != provato — e' l'assunzione
    /// "la salute e' gia' garantita da is_enabled" che ha permesso gli incidenti
    /// 2026-07-14/15 (11 modelli 404 e un 429-quota scoperti dalle richieste di
    /// produzione). Acceso dal flag DB `agent.model_qualification.enforce_routing_gate`
    /// nel solo path AGENTICO; `false` = comportamento storico.
    pub require_qualified: bool,
    /// `true` => esclude i modelli preview/experimental dalla selezione. I
    /// pre-GA girano su capacita' CONDIVISA best-effort (Vertex Dynamic Shared
    /// Quota: 429 RESOURCE_EXHAUSTED a intermittenza anche a basso volume) e
    /// vengono ritirati con ~2 settimane di preavviso (404 improvvisi su tutte
    /// le region) — e' esattamente la coppia di incidenti 2026-07-14/15 dei
    /// consiglieri. Google stessa dichiara gli experimental non adatti alla
    /// produzione. Acceso dal flag DB `agent.model_qualification.exclude_preview_agentic`
    /// nel solo path AGENTICO (le chain agentiche muoiono su un singolo 429/404);
    /// il pin esplicito dell'utente non passa di qui e resta libero.
    pub exclude_preview: bool,
}

/// Frammento WHERE (statico, niente input utente) che riconosce i modelli
/// pre-GA dal SUFFISSO canonico di naming dei provider: `-preview`/`preview-`,
/// `-exp` terminale o seguito da separatore (gemini-2.0-flash-exp,
/// gemini-exp-1206), `experimental`. PUNTO UNICO (regola L) del criterio: i
/// call site non duplicano la regex.
const PRE_GA_MODEL_PREDICATE_SQL: &str =
    " AND model !~* '(preview|experimental|[-_]exp([-_.]|$))'";

/// Flag del gate di qualificazione applicati al path AGENTICO (mig 0591/0592).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct QualificationGate {
    /// `agent.model_qualification.enforce_routing_gate`: richiede
    /// `qualification_state='qualified'` non scaduto + capability PROVATE.
    pub require_qualified: bool,
    /// `agent.model_qualification.exclude_preview_agentic`: esclude i modelli
    /// preview/experimental (capacita' best-effort + ritiri improvvisi).
    pub exclude_preview: bool,
}

/// PUNTO UNICO (regola L) della lettura dei flag del gate di qualificazione
/// (mig 0591/0592). UNA query, cache 60s in-process (stesso pattern di
/// `agent.enforce_port_allocation`): il routing la consulta a ogni selezione
/// AGENTICA senza martellare il DB. Chiave assente o illeggibile -> `false`
/// (comportamento storico: i rollout si accendono SOLO con la riga in settings,
/// regola G, nessun default nascosto che scavalchi il DB).
pub(crate) async fn qualification_gate(db: &PgPool) -> QualificationGate {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static CACHE: OnceLock<Mutex<Option<(QualificationGate, Instant)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some((value, expires_at)) = *guard {
            if Instant::now() < expires_at {
                return value;
            }
        }
    }
    fn flag(v: &str) -> bool {
        matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes" | "on")
    }
    let mut value = QualificationGate::default();
    match sqlx::query_as::<_, (String, String)>(
        "SELECT key, value FROM settings WHERE key IN (
            'agent.model_qualification.enforce_routing_gate',
            'agent.model_qualification.exclude_preview_agentic'
        )",
    )
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            for (k, v) in rows {
                match k.as_str() {
                    "agent.model_qualification.enforce_routing_gate" => {
                        value.require_qualified = flag(&v)
                    }
                    "agent.model_qualification.exclude_preview_agentic" => {
                        value.exclude_preview = flag(&v)
                    }
                    _ => {}
                }
            }
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "qualification_gate: lettura settings fallita, gate spento"
            );
        }
    }
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((value, Instant::now() + Duration::from_secs(60)));
    }
    value
}

/// PUNTO UNICO (regola L) del mapping capability -> colonna booleana canonica
/// di `ai_price_catalog`. Ritorna il nome colonna per le capability che hanno
/// una colonna dedicata (vision, mig 0318; i 4 media kind, mig 0478), `None`
/// per le capability che vivono nel jsonb `capabilities` (chat/code/reasoning/...).
///
/// Aggiungere un nuovo media kind richiede UNA riga qui + la colonna in
/// migrazione: nessun `if`/`match` duplicato sparso nei call site (regola L).
/// I valori ritornati sono nomi-colonna STATICI (whitelist): vengono interpolati
/// nella SQL ma NON derivano da input utente -> niente SQL injection.
fn capability_to_column(capability: &str) -> Option<&'static str> {
    match capability {
        "vision" => Some("supports_vision"),
        "image_gen" => Some("supports_image_gen"),
        "audio_in" => Some("supports_audio_in"),
        "audio_out" => Some("supports_audio_out"),
        "video_gen" => Some("supports_video_gen"),
        _ => None,
    }
}

/// True se la capability e' un MEDIA kind (image/audio/video), non testuale
/// (chat/code/reasoning) ne' vision. Punto unico (regola L) usato per decidere
/// se ESCLUDERE i modelli media dalla selezione: i purpose testuali NON devono
/// pescare un image-gen; un purpose media (es. generate_image) si'.
fn is_media_capability(capability: &str) -> bool {
    matches!(
        capability,
        "image_gen" | "audio_in" | "audio_out" | "video_gen"
    )
}

/// Selezione TIER-CHAIN + `ORDER BY` SQL: semantica del routing LIVE.
///
/// Prova i tier di `tier_chain` in ordine (degradazione); il PRIMO tier con
/// almeno un candidato eleggibile vince (corto-circuito). Entro quel tier
/// ordina per `order_by` (SEGUITO dal tie-breaker `(agentic_thinking_policy='none')
/// DESC` se `require_thinking_non_exclude`) e ritorna i primi `limit`. `tier_chain`
/// vuoto = qualunque tier (singola query).
///
/// Punto unico (regola L) della WHERE di eleggibilita' del path live: prima
/// duplicata tra `select_agentic_model` (SQL agentico) e il ramo non-agentico
/// inline di `best_model_for_tier`. Propaga `Result` (regola H): l'errore SQL
/// non e' piu' silenziato come "nessun modello".
///
/// Ritorna `(provider, model, performance_tier)`: il tier viaggia con la riga
/// (i selettori tier-aware, es. il failover agentico, lo usano come indicazione
/// senza un lookup extra); i caller che non ne hanno bisogno lo ignorano.
pub(super) async fn select_models_tierchain(
    db: &PgPool,
    filter: &EligibilityFilter<'_>,
    tier_chain: &[&str],
    order_by: &str,
    limit: i64,
) -> Result<Vec<(String, String, Option<String>)>, String> {
    let excluded: Vec<String> = if filter.apply_cooldown {
        excluded_providers_lower(filter.exclude_providers)
    } else {
        filter
            .exclude_providers
            .iter()
            .map(|p| p.to_lowercase())
            .collect()
    };

    let tiers: Vec<Option<&str>> = if tier_chain.is_empty() {
        vec![None]
    } else {
        tier_chain.iter().map(|t| Some(*t)).collect()
    };

    // PUNTO UNICO (regola L) del mapping capability -> colonna canonica del
    // catalog. Le capability con una colonna booleana dedicata (vision + i media
    // kind della mig 0478) si filtrano via colonna; ogni altra capability (chat,
    // 'code', 'reasoning', ...) resta nel jsonb `capabilities`. Aggiungere un
    // nuovo media kind = una riga qui (e la colonna in mig), niente if sparsi.
    let capability_column: Option<&'static str> = filter.capability.and_then(capability_to_column);
    // jsonb solo per le capability SENZA colonna dedicata.
    let capability_json = filter
        .capability
        .filter(|_| capability_column.is_none())
        .map(|c| format!("[\"{c}\"]"));
    // Una capability media o vision e' "specializzata": NON va esclusa da se
    // stessa. Le capability TESTUALI (chat/code/None/vision) NON devono pescare
    // modelli media (un image-gen non e' un modello di testo): esclusione esplicita
    // dei flag media quando la capability richiesta NON e' un media kind.
    let requested_is_media = filter.capability.map(is_media_capability).unwrap_or(false);

    for tier in tiers {
        // $1 = array provider esclusi (sempre). Placeholder successivi assegnati
        // in ordine per tenere bind e SQL coerenti (stesso schema di idx manuale
        // del precedente select_agentic_model).
        let mut idx = 1;
        let mut sql = String::from(
            "SELECT provider, model, performance_tier FROM ai_price_catalog \
             WHERE is_enabled = TRUE \
               AND LOWER(provider) <> ALL($1) \
               AND (auto_disabled_reason IS NULL \
                    OR (auto_disabled_reason NOT LIKE 'invalid_model%' \
                        AND auto_disabled_reason NOT LIKE 'model_not_found%'))",
        );
        if filter.require_tool_use {
            sql.push_str(" AND supports_tool_use = TRUE");
        }
        if filter.require_qualified {
            // Gate di qualificazione (mig 0591): solo modelli PROVATI e non
            // scaduti. Frammento statico, nessun input utente interpolato.
            sql.push_str(
                " AND qualification_state = 'qualified' \
                  AND (qualification_expires_at IS NULL OR qualification_expires_at > NOW())",
            );
        }
        if filter.exclude_preview {
            sql.push_str(PRE_GA_MODEL_PREDICATE_SQL);
        }
        if filter.require_thinking_non_exclude {
            sql.push_str(" AND agentic_thinking_policy <> 'exclude'");
        }
        if let Some(col) = capability_column {
            // `col` proviene da `capability_to_column` (whitelist statica di nomi
            // colonna): nessun input utente interpolato, niente SQL injection.
            sql.push_str(&format!(" AND {col} = TRUE"));
        }
        if !requested_is_media {
            // I modelli media non risalgono la classifica dei purpose testuali.
            sql.push_str(
                " AND supports_image_gen = FALSE AND supports_audio_in = FALSE \
                  AND supports_audio_out = FALSE AND supports_video_gen = FALSE",
            );
        }
        if tier.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND performance_tier = ${idx}"));
        }
        if capability_json.is_some() {
            idx += 1;
            // Col gate acceso le capability jsonb si verificano sul PROVATO
            // (qualified_capabilities, scritto solo dal qualificatore), non sul
            // dichiarato: una capability affermata a mano e mai dimostrata non
            // instrada piu' nessuno (nomi colonna statici, niente injection).
            let cap_col = if filter.require_qualified {
                "qualified_capabilities"
            } else {
                "capabilities"
            };
            sql.push_str(&format!(" AND {cap_col} @> ${idx}::jsonb"));
        }
        if filter.min_context_window > 0 {
            idx += 1;
            sql.push_str(&format!(" AND context_window >= ${idx}"));
        }
        if filter.only_provider.is_some() {
            // PIN provider (filtro POSITIVO): restringe al solo provider pinnato.
            // Ultimo placeholder DOPO min_context_window per preservare lo schema
            // idx incrementale; il valore e' bindato lowercase (no interpolazione).
            idx += 1;
            sql.push_str(&format!(" AND LOWER(provider) = ${idx}"));
        }
        // ORDER BY: capacita'/costo (`order_by`) e' il criterio PRIMARIO. Il
        // pre-ordinamento ADR 0025 (preferire i modelli nativamente non-thinking,
        // `policy='none'`) e' declassato a TIE-BREAKER finale. Razionale (regola H,
        // causa radice): i modelli forti moderni sono ORMAI TUTTI dual-mode
        // (`disable_for_tools`: claude opus/sonnet, gpt-5.x, deepseek-v4), mentre i
        // `none` rimasti sono i completion/legacy deboli (deepseek-coder/chat,
        // codestral, gpt-4.1). Con `none` come criterio PRIMARIO il routing agentico
        // sceglieva sistematicamente i modelli peggiori. L'affidabilita' sotto
        // `tool_choice` forzato e' garantita a monte dal gateway (disabilita il
        // thinking quando ci sono tool, vedi nexus-gateway providers). Resta come
        // SPAREGGIO a parita' di `order_by` (preferenza conservata dove non costa).
        sql.push_str(" ORDER BY ");
        sql.push_str(order_by);
        if filter.require_thinking_non_exclude {
            sql.push_str(", (agentic_thinking_policy = 'none') DESC");
        }
        sql.push_str(&format!(" LIMIT {limit}"));

        let mut q = sqlx::query_as::<_, (String, String, Option<String>)>(&sql).bind(&excluded);
        if let Some(t) = tier {
            q = q.bind(t);
        }
        if let Some(c) = capability_json.as_ref() {
            q = q.bind(c);
        }
        if filter.min_context_window > 0 {
            q = q.bind(filter.min_context_window);
        }
        if let Some(p) = filter.only_provider {
            // Stesso ordine dei placeholder: bind DOPO min_context_window.
            q = q.bind(p.to_lowercase());
        }
        let rows = q
            .fetch_all(db)
            .await
            .map_err(|e| format!("select_models_tierchain: query fallita: {e}"))?;
        if !rows.is_empty() {
            return Ok(rows);
        }
    }
    if filter.require_qualified {
        // Pool VUOTO col gate acceso: il sintomo giusto e' "il gate non ha
        // candidati provati" (es. worker di qualificazione fermo, batteria
        // troppo severa, qualificazioni scadute in massa), non un generico
        // "nessun modello". Fail-closed VOLUTO (design gate, regola G): il
        // chiamante gestisce il None; qui il log dice DOVE guardare.
        tracing::warn!(
            capability = filter.capability.unwrap_or("-"),
            "gate qualificazione: NESSUN modello 'qualified' non scaduto per il \
             filtro richiesto — verificare il worker di qualificazione \
             (agent.model_qualification.*) e ai_model_probe_evidence"
        );
    }
    Ok(Vec::new())
}

/// FASE 3 (Stadio 1) — SHADOW-COMPARE del routing per-intent (ADR 0030).
///
/// Opt-in via settings `routing.per_intent_runtime_shadow` (default false). NON
/// cambia la decisione servita all'utente: calcola IN PARALLELO la risoluzione
/// tier-runtime (requirements + cooldown caller-side, stesso ordine di
/// `route_by_slots`) e logga la divergenza vs la decisione STATICA del lookup
/// matrix, per misurare la parita' prima di abilitare il routing runtime (stadi
/// 2-3). Best-effort, non solleva. Chiamare SOLO su intent senza manual_override
/// (il chiamante verifica `RoutingMatrix::is_manual_override`).
pub(crate) async fn shadow_compare_per_intent(
    db: &PgPool,
    intent: &str,
    behavior_mode: &str,
    estimated_tokens: u32,
    static_provider: &str,
    static_model: &str,
) {
    let enabled = crate::settings::get_setting(db, "routing.per_intent_runtime_shadow")
        .await
        .ok()
        .flatten()
        .map(|v| {
            matches!(
                v.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }
    // Requisito tier per (intent, behavior_mode): STESSA fonte dell'auto-promoter
    // (nexus_intent_routing_requirements). Se manca, nessuno shadow per la chiave.
    let req = sqlx::query_as::<_, (String, Vec<String>, bool, String)>(
        "SELECT preferred_tier, required_capabilities, requires_tool_use, cost_direction \
         FROM nexus_intent_routing_requirements WHERE intent = $1 AND behavior_mode = $2",
    )
    .bind(intent)
    .bind(behavior_mode)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let Some((tier, caps, tool, cost_dir)) = req else {
        return;
    };
    // Risoluzione tier-runtime via il selettore unico (regola L), poi cooldown
    // caller-side: primo candidato con provider non in cooldown (come route_by_slots).
    let candidates = match crate::routing_matrix_auto_promoter::select_models_for_requirement(
        db, &tier, &caps, tool, &cost_dir,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("shadow_compare_per_intent: select_models fallita: {e}");
            return;
        }
    };
    let (runtime_provider, runtime_model) = candidates
        .into_iter()
        .find(|(p, _)| !crate::provider_cooldown::is_provider_in_cooldown(p))
        .unwrap_or_else(|| ("__no_model__".to_string(), String::new()));
    let is_match = runtime_provider == static_provider && runtime_model == static_model;
    tracing::info!(
        target: "routing_shadow",
        intent = %intent,
        behavior_mode = %behavior_mode,
        estimated_tokens,
        static_provider = %static_provider,
        static_model = %static_model,
        runtime_provider = %runtime_provider,
        runtime_model = %runtime_model,
        is_match,
        "FASE3 shadow-compare per-intent (statico vs tier-runtime)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    // Schema `ai_price_catalog` dal punto unico condiviso (regola L).
    use crate::test_support::create_ai_price_catalog_table;

    async fn create_requirements_table(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_intent_routing_requirements ( \
                 intent TEXT NOT NULL, \
                 behavior_mode TEXT NOT NULL, \
                 required_capabilities TEXT[] NOT NULL DEFAULT '{}', \
                 requires_tool_use BOOLEAN NOT NULL DEFAULT false, \
                 preferred_tier TEXT NOT NULL DEFAULT 'medium', \
                 weight_tier REAL NOT NULL DEFAULT 0.35, \
                 weight_cost REAL NOT NULL DEFAULT 0.25, \
                 weight_context REAL NOT NULL DEFAULT 0.20, \
                 weight_capabilities REAL NOT NULL DEFAULT 0.20, \
                 cost_direction TEXT NOT NULL DEFAULT 'asc', \
                 PRIMARY KEY (intent, behavior_mode) \
             )",
        )
        .execute(pool)
        .await
        .expect("create table requirements");
    }

    #[sqlx::test]
    async fn fetch_default_weights_legge_riga_sentinella(pool: sqlx::PgPool) {
        create_requirements_table(&pool).await;
        sqlx::query(
            "INSERT INTO nexus_intent_routing_requirements \
             (intent, behavior_mode, weight_tier, weight_cost, weight_context, weight_capabilities, cost_direction) \
             VALUES ('*', '*', 0.40, 0.30, 0.15, 0.15, 'desc')",
        )
        .execute(&pool)
        .await
        .expect("insert sentinella");
        let w = fetch_default_weights(&pool).await.expect("pesi presenti");
        assert!((w.tier - 0.40).abs() < 1e-6);
        assert!((w.cost - 0.30).abs() < 1e-6);
        assert!((w.context - 0.15).abs() < 1e-6);
        assert!((w.capabilities - 0.15).abs() < 1e-6);
    }

    #[sqlx::test]
    async fn fetch_default_weights_err_se_sentinella_assente(pool: sqlx::PgPool) {
        create_requirements_table(&pool).await;
        // Solo righe per intent reali, nessuna sentinella '*'.
        sqlx::query(
            "INSERT INTO nexus_intent_routing_requirements (intent, behavior_mode) VALUES ('chat', 'bilanciata')",
        )
        .execute(&pool)
        .await
        .expect("insert intent reale");
        let res = fetch_default_weights(&pool).await;
        assert!(
            res.is_err(),
            "senza riga sentinella deve fallire visibilmente (regola G)"
        );
    }

    #[sqlx::test]
    async fn tierchain_agentico_sceglie_tool_capable_piu_economico(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens) VALUES \
             ('mistral', 'caro', true, 'none', 'heavy', 10.0), \
             ('openai', 'economico', true, 'none', 'heavy', 2.0), \
             ('google', 'no-tool', false, 'none', 'heavy', 0.5)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        // Esclude no-tool; tra i tool-capable sceglie il piu' economico.
        assert_eq!(
            out,
            vec![(
                "openai".to_string(),
                "economico".to_string(),
                Some("heavy".to_string())
            )]
        );
    }

    /// TEST 6 — only_provider (PIN): `Some(p)` RESTRINGE la selezione al solo
    /// provider `p` (filtro positivo bindato); `None` = query identica alla
    /// precedente (nessuna regressione per i chiamanti storici). Discriminante:
    /// senza pin vince il piu' economico (mistral); col pin='openai' vince openai
    /// anche se piu' caro (il pin e' preferenza-forte tier-aware).
    #[sqlx::test]
    async fn tierchain_only_provider_restringe_e_none_invariato(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens) VALUES \
             ('mistral', 'm-economico', true, 'none', 'heavy', 2.0), \
             ('openai', 'o-caro', true, 'none', 'heavy', 10.0)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // None: nessuna restrizione -> il piu' economico (mistral).
        let f_none = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out_none = select_models_tierchain(
            &pool,
            &f_none,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out_none,
            vec![(
                "mistral".to_string(),
                "m-economico".to_string(),
                Some("heavy".to_string())
            )],
            "None: query invariata, vince il piu' economico"
        );
        // Some('openai'): restringe a openai anche se piu' caro.
        let f_pin = EligibilityFilter {
            only_provider: Some("openai"),
            ..f_none.clone()
        };
        let out_pin = select_models_tierchain(
            &pool,
            &f_pin,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out_pin,
            vec![(
                "openai".to_string(),
                "o-caro".to_string(),
                Some("heavy".to_string())
            )],
            "Some('openai'): filtro positivo, solo openai"
        );
        // Some di un provider ASSENTE dal catalog -> nessun candidato (pool vuoto).
        let f_absent = EligibilityFilter {
            only_provider: Some("deepseek"),
            ..f_none.clone()
        };
        let out_absent = select_models_tierchain(
            &pool,
            &f_absent,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        assert!(
            out_absent.is_empty(),
            "provider pinnato assente -> pool vuoto"
        );
    }

    #[sqlx::test]
    async fn tierchain_preferisce_policy_none_su_dual_mode(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        // Stesso costo: il TIE-BREAKER (policy='none' DESC, ultimo criterio dopo
        // order_by) fa vincere il nativamente non-thinking A PARITA' di order_by.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens) VALUES \
             ('a', 'dual', true, 'disable_for_tools', 'heavy', 1.0), \
             ('b', 'nativo', true, 'none', 'heavy', 1.0)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "b".to_string(),
                "nativo".to_string(),
                Some("heavy".to_string())
            )]
        );
    }

    #[sqlx::test]
    async fn tierchain_capacita_costo_vince_sul_tiebreaker_thinking(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        // REGRESSIONE (fix routing agentico, regola H): con i modelli forti moderni
        // tutti dual-mode ('disable_for_tools') e i 'none' rimasti deboli/legacy, il
        // criterio PRIMARIO deve essere order_by (qui: costo), NON la policy thinking.
        // Il forte ed economico ('forte', disable_for_tools, 0.14) deve battere il
        // debole piu' caro ('debole', none, 1.0): col vecchio pre-ordinamento PRIMARIO
        // avrebbe vinto 'debole' (causa radice di "agentic usa deepseek-coder").
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens) VALUES \
             ('a', 'forte', true, 'disable_for_tools', 'medium', 0.14), \
             ('b', 'debole', true, 'none', 'medium', 1.0)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["medium"],
            "input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "a".to_string(),
                "forte".to_string(),
                Some("medium".to_string())
            )]
        );
    }

    #[sqlx::test]
    async fn tierchain_esclude_policy_exclude_quando_richiesto(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier) VALUES \
             ('a', 'escluso', true, 'exclude', 'heavy')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        assert!(
            out.is_empty(),
            "agentic_thinking_policy='exclude' deve essere escluso"
        );
    }

    #[sqlx::test]
    async fn tierchain_degrada_al_tier_successivo(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        // Nessun heavy: la chain deve scendere a medium.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier) VALUES \
             ('a', 'medio', true, 'none', 'medium')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["heavy", "medium"],
            "input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "a".to_string(),
                "medio".to_string(),
                Some("medium".to_string())
            )]
        );
    }

    #[sqlx::test]
    async fn tierchain_vision_via_supports_vision(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, supports_vision, performance_tier) VALUES \
             ('a', 'no-vision', false, false, 'medium'), \
             ('b', 'vision', false, true, 'medium')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // Ramo non-agentico: nessun filtro tool/policy, capability='vision'.
        let f = EligibilityFilter {
            require_tool_use: false,
            require_thinking_non_exclude: false,
            capability: Some("vision"),
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["medium"],
            "is_featured DESC, input_cost_per_million_tokens ASC",
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "b".to_string(),
                "vision".to_string(),
                Some("medium".to_string())
            )]
        );
    }

    #[sqlx::test]
    async fn tierchain_capability_none_esclude_media(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        // Un image-gen tool-capable (assurdo, ma testa che l'esclusione media
        // scatti a prescindere) NON deve entrare nel routing chat (capability=None).
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, supports_image_gen, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens) VALUES \
             ('openai', 'dall-e-3', true, true, 'none', 'medium', 0.1), \
             ('openai', 'gpt-4o', true, false, 'none', 'medium', 1.0)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: false,
            require_thinking_non_exclude: false,
            capability: None,
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["medium"],
            "input_cost_per_million_tokens ASC",
            5,
        )
        .await
        .expect("ok");
        // Solo il chat: il media (image-gen) e' escluso dai purpose testuali.
        assert_eq!(
            out,
            vec![(
                "openai".to_string(),
                "gpt-4o".to_string(),
                Some("medium".to_string())
            )]
        );
    }

    #[sqlx::test]
    async fn tierchain_image_gen_via_supports_image_gen(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, supports_image_gen, performance_tier) VALUES \
             ('openai', 'gpt-4o', false, false, 'light'), \
             ('openai', 'gpt-image-1', false, true, 'light')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // Gemello del test vision: capability='image_gen' deve filtrare via colonna
        // canonica supports_image_gen e selezionare SOLO il modello media.
        let f = EligibilityFilter {
            require_tool_use: false,
            require_thinking_non_exclude: false,
            capability: Some("image_gen"),
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["light"],
            "is_featured DESC, input_cost_per_million_tokens ASC",
            5,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "openai".to_string(),
                "gpt-image-1".to_string(),
                Some("light".to_string())
            )]
        );
    }

    #[test]
    fn capability_to_column_mappa_solo_le_capability_con_colonna() {
        assert_eq!(capability_to_column("vision"), Some("supports_vision"));
        assert_eq!(
            capability_to_column("image_gen"),
            Some("supports_image_gen")
        );
        assert_eq!(capability_to_column("audio_in"), Some("supports_audio_in"));
        assert_eq!(
            capability_to_column("audio_out"),
            Some("supports_audio_out")
        );
        assert_eq!(
            capability_to_column("video_gen"),
            Some("supports_video_gen")
        );
        // capability nel jsonb: nessuna colonna dedicata.
        assert_eq!(capability_to_column("code"), None);
        assert_eq!(capability_to_column("reasoning"), None);
    }

    #[test]
    fn is_media_capability_distingue_media_da_testuali() {
        assert!(is_media_capability("image_gen"));
        assert!(is_media_capability("audio_in"));
        assert!(is_media_capability("audio_out"));
        assert!(is_media_capability("video_gen"));
        // vision NON e' media (e' una capability di input testuale-multimodale).
        assert!(!is_media_capability("vision"));
        assert!(!is_media_capability("code"));
    }

    #[test]
    fn excluded_providers_lower_normalizza_e_deduplica() {
        // cooldown_snapshot e' vuoto in test (nessun provider messo in cooldown):
        // verifichiamo la normalizzazione/dedup degli extra.
        let out = excluded_providers_lower(&["OpenAI".into(), "openai".into(), "Google".into()]);
        assert!(out.contains(&"openai".to_string()));
        assert!(out.contains(&"google".to_string()));
        // "OpenAI" e "openai" collassano in un solo elemento.
        assert_eq!(out.iter().filter(|p| *p == "openai").count(), 1);
    }

    // ── Gate di qualificazione (mig 0591/0592) ────────────────────────────────
    // Incidenti 2026-07-14/15: il routing pinnava alle figure del consiglio
    // modelli DICHIARATI nel catalog ma mai provati (404 su Vertex) o pre-GA in
    // quota condivisa satura (429). Il gate richiede l'EVIDENZA.

    /// Filtro agentico base dei test del gate (i flag del gate variano per test).
    fn gate_filter(require_qualified: bool, exclude_preview: bool) -> EligibilityFilter<'static> {
        EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified,
            exclude_preview,
        }
    }

    #[sqlx::test]
    async fn gate_qualificazione_esclude_i_non_provati_e_gli_scaduti(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, input_cost_per_million_tokens, qualification_state, qualification_expires_at) VALUES \
             ('a', 'dichiarato-mai-provato', 1.0, 'unqualified', NULL), \
             ('b', 'provato-ma-scaduto',     2.0, 'qualified',   NOW() - interval '1 hour'), \
             ('c', 'provato-valido',         3.0, 'qualified',   NOW() + interval '1 day')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // Gate ACCESO: resta solo il provato non scaduto, anche se costa di piu'.
        let out = select_models_tierchain(
            &pool,
            &gate_filter(true, false),
            &[],
            "input_cost_per_million_tokens ASC",
            10,
        )
        .await
        .expect("ok");
        assert_eq!(
            out.iter().map(|(_, m, _)| m.as_str()).collect::<Vec<_>>(),
            vec!["provato-valido"],
            "il gate deve escludere unqualified e qualified scaduto"
        );
        // Gate SPENTO: comportamento storico, tutti e tre eleggibili.
        let out = select_models_tierchain(
            &pool,
            &gate_filter(false, false),
            &[],
            "input_cost_per_million_tokens ASC",
            10,
        )
        .await
        .expect("ok");
        assert_eq!(out.len(), 3, "gate spento = comportamento storico");
    }

    #[sqlx::test]
    async fn gate_capability_verificata_sul_provato_non_sul_dichiarato(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        // 'millantatore' DICHIARA reasoning ma il qualificatore non gliel'ha
        // provato; 'provato' ce l'ha in qualified_capabilities.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, input_cost_per_million_tokens, capabilities, qualification_state, qualified_capabilities) VALUES \
             ('a', 'millantatore', 1.0, '[\"chat\",\"reasoning\"]', 'qualified', '[]'), \
             ('b', 'provato',      2.0, '[\"chat\",\"reasoning\"]', 'qualified', '[\"chat\",\"reasoning\"]')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let mut f = gate_filter(true, false);
        f.capability = Some("reasoning");
        let out = select_models_tierchain(&pool, &f, &[], "input_cost_per_million_tokens ASC", 10)
            .await
            .expect("ok");
        assert_eq!(
            out.iter().map(|(_, m, _)| m.as_str()).collect::<Vec<_>>(),
            vec!["provato"],
            "col gate la capability si verifica su qualified_capabilities"
        );
        // Gate spento: si crede al dichiarato (comportamento storico).
        let mut f = gate_filter(false, false);
        f.capability = Some("reasoning");
        let out = select_models_tierchain(&pool, &f, &[], "input_cost_per_million_tokens ASC", 10)
            .await
            .expect("ok");
        assert_eq!(out.len(), 2);
    }

    #[sqlx::test]
    async fn exclude_preview_taglia_i_pre_ga_ma_non_i_ga(pool: sqlx::PgPool) {
        create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, input_cost_per_million_tokens) VALUES \
             ('g', 'gemini-3.1-pro-preview',  1.0), \
             ('g', 'gemini-2.0-flash-exp',    1.1), \
             ('g', 'gemini-exp-1206',         1.2), \
             ('x', 'modello-experimental',    1.3), \
             ('g', 'gemini-2.5-flash',        2.0), \
             ('m', 'model-express',           3.0)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let out = select_models_tierchain(
            &pool,
            &gate_filter(false, true),
            &[],
            "input_cost_per_million_tokens ASC",
            10,
        )
        .await
        .expect("ok");
        let models: Vec<&str> = out.iter().map(|(_, m, _)| m.as_str()).collect();
        assert_eq!(
            models,
            vec!["gemini-2.5-flash", "model-express"],
            "i pre-GA (preview/-exp/experimental) sono esclusi; i GA e i nomi \
             che CONTENGONO 'exp' senza esserlo (express) restano"
        );
    }
    /// IL TEST PONTE fra le due meta' del vocabolario (regola L).
    ///
    /// La scala dei tier deve vivere in UN posto solo, ma Rust e SQL sono
    /// linguaggi diversi: l'espressione SQL e' GENERATA da `tier_rank_sql` a
    /// partire dalle stesse `PERFORMANCE_TIERS`/`tier_rank`. Questo test chiude
    /// il cerchio provandola su POSTGRES VERO: se le due meta' divergessero —
    /// com'era successo con la scala a 3 livelli di `agent_run.rs`, dove
    /// `frontier` e `high` collassavano su 0 come `light` — qui diventa rosso.
    ///
    /// Copre anche i casi che il CASE scritto a mano sbagliava piu' spesso: il
    /// tier NULL (la colonna sta per diventare nullable) e un valore fuori
    /// vocabolario, che devono prendere lo stesso rank neutro di `tier_rank`.
    #[sqlx::test]
    async fn tier_rank_sql_coincide_col_rank_rust(pool: sqlx::PgPool) {
        use nexus_agent_graph::decisions::tiers::{tier_rank, tier_rank_sql, PERFORMANCE_TIERS};

        let expr = tier_rank_sql("t");
        for tier in PERFORMANCE_TIERS {
            let sql_rank: i32 = sqlx::query_scalar(&format!("SELECT {expr} FROM (SELECT $1::text AS t) s"))
                .bind(tier)
                .fetch_one(&pool)
                .await
                .expect("query rank");
            assert_eq!(
                sql_rank as u8,
                tier_rank(tier),
                "Postgres e Rust ordinano '{tier}' in modo diverso: la scala si e'                  sdoppiata (SQL={sql_rank}, Rust={})",
                tier_rank(tier)
            );
        }
        // Tolleranza identica: maiuscole e spazi.
        let sql_rank: i32 = sqlx::query_scalar(&format!("SELECT {expr} FROM (SELECT '  HEAVY '::text AS t) s"))
            .fetch_one(&pool)
            .await
            .expect("query rank");
        assert_eq!(sql_rank as u8, tier_rank("  HEAVY "));
        // Valore ignoto e NULL -> rank neutro, come tier_rank.
        for ignoto in ["ultra", "fast"] {
            let sql_rank: i32 = sqlx::query_scalar(&format!("SELECT {expr} FROM (SELECT $1::text AS t) s"))
                .bind(ignoto)
                .fetch_one(&pool)
                .await
                .expect("query rank");
            assert_eq!(sql_rank as u8, tier_rank(ignoto), "'{ignoto}' deve avere il rank neutro");
        }
        let sql_null: i32 = sqlx::query_scalar(&format!("SELECT {expr} FROM (SELECT NULL::text AS t) s"))
            .fetch_one(&pool)
            .await
            .expect("query rank null");
        assert_eq!(
            sql_null as u8,
            tier_rank(""),
            "un tier NULL deve prendere il rank neutro, non sparire dall'ordinamento"
        );
    }

    /// L'ordinamento REALE sul catalog: il difetto misurato il 15/07 era che
    /// l'escalation "sali al modello piu' capace" sceglieva un heavy scartando i
    /// frontier. Con l'espressione generata il primo e' il frontier.
    #[sqlx::test]
    async fn ordinare_col_rank_generato_mette_il_frontier_in_testa(pool: sqlx::PgPool) {
        use nexus_agent_graph::decisions::tiers::tier_rank_sql;
        create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, performance_tier) VALUES              ('openai', 'gpt-frontier', 'frontier'),              ('openai', 'gpt-heavy', 'heavy'),              ('mistral', 'mistral-medium', 'medium'),              ('openai', 'gpt-high', 'high'),              ('openai', 'gpt-light', 'light')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let ordinati: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT model FROM ai_price_catalog ORDER BY {} DESC",
            tier_rank_sql("performance_tier")
        ))
        .fetch_all(&pool)
        .await
        .expect("select");
        assert_eq!(
            ordinati,
            vec!["gpt-frontier", "gpt-heavy", "gpt-high", "mistral-medium", "gpt-light"],
            "l'ordine deve seguire la scala a 5 livelli; col CASE a 3 livelli              frontier e high finivano in fondo, sotto il medium"
        );
    }
}
