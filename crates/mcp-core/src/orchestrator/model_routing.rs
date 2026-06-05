//! Routing modello: stima complessita', selezione dal catalog,
//! soglie token, RoutingConfig e default per provider.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use uuid::Uuid;

use mcp_proto::neural::{
    neural_core_service_client::NeuralCoreServiceClient, ClassifyIntentRequest, EmbedTextRequest,
    GenerateAgentTurnRequest, GenerateCompletionRequest, RouteModelRequest,
};

use crate::{
    billing::{self, UsageNumbers},
    domain::OrchestratorAudit,
    nexus_gateway::{intent_to_alias, GwMessage, GwMetadata, GwRequest, NexusGatewayClient},
    provider_cooldown::{is_provider_in_cooldown, put_provider_in_cooldown},
    vector_memory,
};

use super::*;

#[derive(Debug)]
pub(crate) struct RoutingDecision {
    pub(crate) provider: String,
    pub(crate) model: String,
    #[allow(dead_code)]
    pub(crate) rationale: &'static str,
}

/// Soglie token per intent_key (route_model_with_mode). Letti da
/// `settings.routing.token_threshold_*` via `RoutingThresholds`.
/// Usato come "view" minimale per non passare l'intera struct.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TokenThresholds {
    pub(crate) chat_breve: u32,
    pub(crate) chat_media: u32,
    pub(crate) complex_fix: u32,
}

impl TokenThresholds {
    /// Default = seed mig 0111 (allineato).
    pub(crate) fn defaults() -> Self {
        Self {
            chat_breve: 400,
            chat_media: 1_500,
            complex_fix: 3_000,
        }
    }

    pub(crate) fn from_routing_thresholds(t: &crate::routing_config::RoutingThresholds) -> Self {
        Self {
            chat_breve: t.token_threshold_chat_breve,
            chat_media: t.token_threshold_chat_media,
            complex_fix: t.token_threshold_complex_fix,
        }
    }
}

/// Risultato dettagliato di [`Orchestrator::resolve_agent_provider_detailed`].
/// Esposto come JSON tramite l'endpoint internal di routing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingResolveResult {
    pub provider: String,
    pub model: String,
    pub intent: String,
    pub mode: String,
    pub risky: bool,
    pub rationale: String,
    /// Fonte della decisione di model. Permette al chiamante (brain Python,
    /// observability, dashboard admin) di capire come e' stato scelto:
    ///   - "matrix"   = decisa dalla `route_model_with_mode` (matrix statica)
    ///   - "catalog"  = decisa dal catalogo prezzi `ai_price_catalog`
    ///                  (modalita' dinamica, ottimizzazione costo/capability)
    ///   - "override" = forzata da `provider_override` utente
    ///   - "cooldown_fallback" = matrix scelta era anthropic ma in cooldown,
    ///                            sostituita con prossimo capable
    pub source: String,
    /// Behavior mode reale a livello DB (puo' differire da `mode` esposto
    /// se il routing applica un override risky o se il dinamico viene
    /// degradato a bilanciata sui task rischiosi).
    pub configured_behavior_mode: String,
    /// True se TUTTI i provider della hierarchy sono in cooldown E il
    /// `provider`/`model` ritornato e' un'ultima istanza non garantita.
    /// Il chiamante DEVE fermarsi e avvertire l'utente: nessuno dei
    /// provider configurati e' al momento utilizzabile (quote esaurite,
    /// rate limit, billing). Continuare comunque produrrebbe lo stesso
    /// errore in loop.
    pub no_capable_provider: bool,
    /// True se la decisione deriva da una FORZATURA esplicita dell'utente
    /// (`provider_override` / `model_override` dal dropdown chat, non Auto).
    /// In questo caso il cooldown billing/quota e' deliberatamente IGNORATO:
    /// l'utente ha scelto consapevolmente quel provider/modello e va usato
    /// anche se in cooldown (ADR 0020). Il chiamante puo' mostrare un avviso
    /// "stai forzando un provider in cooldown" ma NON deve bloccare il run.
    /// Quando true, `no_capable_provider` e' sempre false (la scelta utente
    /// e' per definizione "capable" dal suo punto di vista).
    pub user_override: bool,
    /// Lista provider in cooldown al momento della decisione, ordinata
    /// come la hierarchy. Permette al frontend di mostrare un alert
    /// dettagliato ("anthropic e openai sono in cooldown — solo deepseek
    /// e' disponibile").
    pub providers_in_cooldown: Vec<String>,
    /// Se valorizzato, il routing NON ha potuto decidere perche' la matrice
    /// DB e' irraggiungibile o non popolata. Il chiamante DEVE fermarsi
    /// (HTTP 503 Service Unavailable) e mostrare il messaggio all'admin.
    /// Niente fallback hardcoded: e' un errore di configurazione, non
    /// un caso da nascondere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Regola di disponibilita' provider (ADR 0020), estratta come funzione pura
/// per renderla testabile senza DB.
///
/// - Modalita' AUTO (nessuna forzatura): il cooldown e' VINCOLANTE. Se il
///   provider scelto e' in cooldown, o tutti i provider noti lo sono, la
///   decisione e' `no_capable_provider = true` (il chiamante deve fermarsi e
///   avvertire, non ritentare un provider morto).
/// - Forzatura ESPLICITA utente (`user_override`): la scelta e' consapevole e
///   non e' mai "no_capable" — il provider va usato anche se in cooldown.
pub(crate) fn compute_no_capable_provider(
    user_override: bool,
    chosen_provider_in_cooldown: bool,
    all_known_in_cooldown: bool,
) -> bool {
    !user_override && (chosen_provider_in_cooldown || all_known_in_cooldown)
}

#[derive(Debug, Clone)]
pub(crate) struct DynamicRoutingDecision {
    pub(crate) provider: String,
    pub(crate) model: String,
    #[allow(dead_code)]
    pub(crate) rationale: &'static str,
}

/// Stima la complessità reale del messaggio ignorando liste/elenchi ripetitivi
/// (es. quality findings con decine di righe identiche).
/// Ritorna il numero di token "significativi" (prime 300 parole uniche).
pub(crate) fn estimate_complexity(message: &str) -> u32 {
    // Conta solo le prime 200 parole per la complessità dell'intent — il resto sono dati
    let core_words = message.split_whitespace().take(200).count() as u32;
    core_words.saturating_mul(2).max(50)
}

/// Sceglie provider e modello in base all'intent, complessità e behavior_mode.
// route_model_local rimossa: era dead_code dopo l'introduzione di route_model_from_catalog
// (refactor 0101 model-registry). Tutti i call site usano route_model_with_mode con la
// matrice DB passata esplicitamente.

/// Ricava il provider di un modello dal catalogo prezzi (ADR 0023).
/// Usato quando l'utente forza un `model_override` senza `provider_override`:
/// un modello identifica univocamente il suo provider.
/// Deterministico: se piu' provider espongono lo stesso `model`, prende quello
/// abilitato col costo input piu' basso (NULL per ultimo). Ritorna `None` se il
/// modello non e' nel catalogo (il chiamante fa fallback al routing per intent,
/// senza inventare provider — regola G).
pub(crate) async fn provider_for_model(db: &PgPool, model: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT provider FROM ai_price_catalog
         WHERE model = $1 AND is_enabled = true
         ORDER BY input_cost_per_million_tokens ASC NULLS LAST
         LIMIT 1",
    )
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// Seleziona il modello ottimale dal catalogo DB per la modalità richiesta.
/// La modalità "dinamico" sceglie il modello più adatto per capability+tier,
/// privilegiando il costo più basso a parità di tier richiesto.
pub(crate) async fn route_model_from_catalog(
    db: &PgPool,
    base_tier: &str,
    capability: &str,
    mode: &str,
) -> Option<DynamicRoutingDecision> {
    // Promozione/declassamento del tier in base al behavior_mode.
    // "approfondita" scala in alto, "veloce"/"economica" scala in basso.
    // Il `base_tier` arriva gia' risolto dal chiamante via IntentCapabilityMap
    // (mig 0110), che applica le soglie di token threshold per l'intent.
    let required_tier = match mode {
        "approfondita" => match base_tier {
            "light" => "medium",
            "medium" => "heavy",
            other => other,
        },
        "veloce" | "economica" => match base_tier {
            "heavy" => "medium",
            other => other,
        },
        _ => base_tier,
    };

    // Query al catalogo: trova il modello più economico che soddisfa tier+capability.
    // Per "veloce" ordina per speed_tier, per "economica" per costo, per altri per featured.
    let order_clause = match mode {
        "veloce"    => "CASE speed_tier WHEN 'fast' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, input_cost_per_million_tokens ASC",
        "economica" => "input_cost_per_million_tokens ASC",
        "approfondita" => "is_featured DESC, input_cost_per_million_tokens DESC",
        _           => "is_featured DESC, input_cost_per_million_tokens ASC",
    };

    // Selezione tramite il PUNTO UNICO (regola L): l'eleggibilita' agentica
    // (tool_use, agentic_thinking_policy<>'exclude', consecutive_failures, cooldown)
    // e' definita una sola volta in select_agentic_model. Degradazione di tier
    // controllata: heavy->medium, medium->light, ecc.
    let tier_chain: Vec<&str> = match required_tier {
        "heavy" => vec!["heavy", "medium"],
        "medium" => vec!["medium", "light"],
        "light" => vec!["light", "medium"],
        other => vec![other],
    };
    select_agentic_model(db, &tier_chain, Some(capability), 0, &[], order_clause)
        .await
        .map(|(provider, model)| DynamicRoutingDecision {
            provider,
            model,
            rationale: "catalog dynamic routing (selettore agentico unico)",
        })
}

/// Seleziona il miglior modello del catalog per un dato `tier`, opzionalmente
/// filtrato per `capability` e `requires_tool_use`. Usato dalla risoluzione
/// tier-based dei purpose (mig 0203): es. il purpose 'planner' -> tier 'heavy'
/// + capability 'reasoning' sceglie dinamicamente il miglior modello heavy
/// disponibile (esclusi i provider in cooldown), il piu' economico tra i
/// featured. Ritorna None se nessun candidato soddisfa i criteri (il chiamante
/// cade sul fallback statico del purpose).
pub async fn best_model_for_tier(
    db: &PgPool,
    tier: &str,
    capability: Option<&str>,
    requires_tool_use: bool,
) -> Option<(String, String)> {
    // Run AGENTICO (tool): delega al PUNTO UNICO di selezione (regola L), che
    // applica l'eleggibilita' agentica + cooldown in un solo posto.
    if requires_tool_use {
        return select_agentic_model(
            db,
            &[tier],
            capability,
            0,
            &[],
            "is_featured DESC, input_cost_per_million_tokens ASC",
        )
        .await;
    }

    // Caso NON-agentico (es. purpose vision/chat/embedding): nessun filtro
    // tool_use/policy; esclude comunque i provider in cooldown.
    let cooldown_providers: Vec<String> = crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .map(|(name, _, _)| name)
        .collect();
    let mut idx = 1; // $1 = tier
    let capability_json = capability.map(|c| format!("[\"{c}\"]"));
    let capability_predicate = if capability_json.is_some() {
        idx += 1;
        format!("AND capabilities @> ${idx}::jsonb")
    } else {
        String::new()
    };
    idx += 1;
    let cooldown_idx = idx; // ultimo placeholder

    let query = format!(
        r#"SELECT provider, model FROM ai_price_catalog
           WHERE is_enabled = TRUE
             AND performance_tier = $1
             {capability_predicate}
             AND provider <> ALL(${cooldown_idx})
           ORDER BY is_featured DESC, input_cost_per_million_tokens ASC
           LIMIT 1"#
    );

    let mut q = sqlx::query_as::<_, (String, String)>(&query).bind(tier);
    if let Some(cap) = capability_json.as_ref() {
        q = q.bind(cap);
    }
    q = q.bind(&cooldown_providers);

    q.fetch_optional(db).await.ok().flatten()
}

/// PUNTO UNICO di selezione di un modello AGENTICO dal catalog (CLAUDE.md, regola L).
///
/// Tutte le selezioni/fallback di un modello per un run a tool DEVONO passare di
/// qui: niente query SQL duplicate sparse (best_model_for_tier, cooldown-fallback,
/// re-route context-aware, cascade, dynamic catalog) con filtri copiati a mano.
///
/// Eleggibilita' SEMPRE applicata (definita una volta sola):
///   - `is_enabled = TRUE`
///   - `supports_tool_use = TRUE`
///   - `agentic_thinking_policy <> 'exclude'` (i dual-mode sono ammessi; l'adapter
///     forza il non-thinking nei tool-loop, ADR 0025)
///   - `consecutive_failures = 0` (modelli sani)
///   - provider NON in cooldown (snapshot in-memory) e NON in `exclude_providers`
///
/// Filtri opzionali:
///   - `tier_chain`: tier provati in ordine (degradazione); il primo tier con un
///     match vince. `&[]` = qualunque tier (singola query, ordinata da `order_by`).
///   - `capability`: `capabilities @> ["cap"]`.
///   - `min_context_window`: `context_window >= N` (0 = nessun filtro).
///   - `order_by`: clausola ORDER BY SQL (UNICA variazione per call site; valori
///     costanti dal codice, mai input utente).
pub(crate) async fn select_agentic_model(
    db: &PgPool,
    tier_chain: &[&str],
    capability: Option<&str>,
    min_context_window: i64,
    exclude_providers: &[String],
    order_by: &str,
) -> Option<(String, String)> {
    // Provider esclusi = cooldown (snapshot) + extra del chiamante, lowercase.
    let mut excluded: Vec<String> = crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .map(|(name, _, _)| name.to_lowercase())
        .collect();
    for p in exclude_providers {
        let pl = p.to_lowercase();
        if !excluded.contains(&pl) {
            excluded.push(pl);
        }
    }

    // Tier da provare: nessuno (None) oppure la chain in ordine.
    let tiers: Vec<Option<&str>> = if tier_chain.is_empty() {
        vec![None]
    } else {
        tier_chain.iter().map(|t| Some(*t)).collect()
    };
    let capability_json = capability.map(|c| format!("[\"{c}\"]"));

    for tier in tiers {
        // $1 = array provider esclusi (sempre). Placeholder successivi assegnati
        // in ordine per tenere bind e SQL coerenti.
        let mut idx = 1;
        let mut sql = String::from(
            "SELECT provider, model FROM ai_price_catalog \
             WHERE is_enabled = TRUE \
               AND supports_tool_use = TRUE \
               AND agentic_thinking_policy <> 'exclude' \
               AND consecutive_failures = 0 \
               AND LOWER(provider) <> ALL($1)",
        );
        if tier.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND performance_tier = ${idx}"));
        }
        if capability_json.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND capabilities @> ${idx}::jsonb"));
        }
        if min_context_window > 0 {
            idx += 1;
            sql.push_str(&format!(" AND context_window >= ${idx}"));
        }
        sql.push_str(&format!(" ORDER BY {order_by} LIMIT 1"));

        let mut q = sqlx::query_as::<_, (String, String)>(&sql).bind(&excluded);
        if let Some(t) = tier {
            q = q.bind(t);
        }
        if let Some(c) = capability_json.as_ref() {
            q = q.bind(c);
        }
        if min_context_window > 0 {
            q = q.bind(min_context_window);
        }
        if let Some(found) = q.fetch_optional(db).await.ok().flatten() {
            return Some(found);
        }
    }
    None
}

/// Route (intent, behavior_mode) -> (provider, model) consultando la matrice DB
/// (cache 60s in-memory). Sostituisce la matrice hardcoded che era qui prima
/// del refactor 0101 (vedi `crates/mcp-core/src/routing_matrix.rs`).
///
/// Se la matrice non ha entry per (intent, mode), fallback in cascata:
/// 1. Prova lo stesso intent con mode 'bilanciata'
/// 2. Prova default per provider 'anthropic' (tool use solido)
/// 3. Ultima istanza: default per provider 'openai'
pub(crate) fn route_model_with_mode(
    matrix: &crate::routing_matrix::RoutingMatrix,
    intent: &str,
    estimated_tokens: u32,
    mode: &str,
    preferred_provider_for_intent: Option<&str>,
    token_thresholds: &TokenThresholds,
) -> RoutingDecision {
    // Determina intent_key composta usando le soglie da settings.routing.*
    // (mig 0111). I valori default sono replicati in `TokenThresholds::defaults()`.
    let intent_key = match intent {
        "debug" => "debug",
        "architecture" => "architecture",
        "refactor" => "refactor",
        "fix" => {
            if estimated_tokens > token_thresholds.complex_fix {
                "fix_complesso"
            } else {
                "fix_semplice"
            }
        }
        "test" => "test",
        "docs" => "docs",
        "file_ops" => "file_ops",
        "system_admin" => "system_admin",
        _ => {
            if estimated_tokens <= token_thresholds.chat_breve {
                "chat_breve"
            } else if estimated_tokens <= token_thresholds.chat_media {
                "chat_media"
            } else {
                "chat_lunga"
            }
        }
    };

    // Routing matrix: (intent_key, mode) → (provider, model)
    // Budget-aware lookup: usa `lookup_with_budget` che applica le regole
    // escalation (mig 0120) quando `estimated_tokens >= threshold`. Cosi'
    // task lunghi/complessi prendono automaticamente il modello escalation
    // (es. google: 2.5-pro -> 3.1-pro-preview-customtools sopra soglia).
    // Senza questo (bug 30/05/2026) i campi escalation_* del DB erano popolati
    // ma mai usati — il routing prendeva sempre il modello base.
    let est_i32: i32 = estimated_tokens.try_into().unwrap_or(i32::MAX);

    // Helper: skip provider in cooldown — chiamarli produrrebbe billing/rate-limit
    // error che farebbe fallire l'intera richiesta utente.
    let in_cooldown = |p: &str| crate::provider_cooldown::is_provider_in_cooldown(p);

    // 1. Lookup diretto (intent_key, mode) nella matrice DB con escalation
    if let Some((provider, model)) = matrix.lookup_with_budget(intent_key, mode, est_i32) {
        if !in_cooldown(&provider) {
            return RoutingDecision {
                provider,
                model,
                rationale: "routing_matrix DB",
            };
        }
        tracing::warn!(
            "route_model_with_mode: skip provider {} (in cooldown)",
            provider
        );
    }

    // 2. Fallback: prova lo stesso intent con mode 'bilanciata' (budget-aware)
    if mode != "bilanciata" {
        if let Some((provider, model)) =
            matrix.lookup_with_budget(intent_key, "bilanciata", est_i32)
        {
            if !in_cooldown(&provider) {
                return RoutingDecision {
                    provider,
                    model,
                    rationale: "routing_matrix DB (mode fallback bilanciata)",
                };
            }
            tracing::warn!(
                "route_model_with_mode: skip provider {} su fallback bilanciata (in cooldown)",
                provider
            );
        }
    }

    // 2b. Fallback: cerca QUALSIASI mode per lo stesso intent_key con un provider non in cooldown
    for try_mode in &["bilanciata", "approfondita", "veloce", "economica"] {
        if let Some((provider, model)) = matrix.lookup_with_budget(intent_key, try_mode, est_i32) {
            if !in_cooldown(&provider) {
                return RoutingDecision {
                    provider,
                    model,
                    rationale: "routing_matrix DB (cooldown bypass: any mode)",
                };
            }
        }
    }

    // 3. Fallback: usa il preferred_provider per l'intent passato dal caller
    // (letto dalla cache nexus_intent_capability, mig 0110). Se non specificato
    // o se il provider non ha default model in matrix.default_model, ritorna
    // una sentinella `__no_model__` che il chiamante a monte traduce in
    // RoutingResolveResult { no_capable_provider: true } → HTTP 503.
    if let Some(provider) = preferred_provider_for_intent {
        if let Some(model) = matrix.default_model(provider) {
            return RoutingDecision {
                provider: provider.to_string(),
                model,
                rationale: "routing_matrix default per preferred_provider intent",
            };
        }
    }

    // 4. Nessun match possibile. Niente fallback hardcoded (regola G CLAUDE.md):
    // il chiamante DEVE intercettare questa sentinella e propagare HTTP 503.
    tracing::error!(
        "route_model_with_mode: nessun match per (intent={}, mode={}) e preferred_provider mancante o non in matrix.default_models. \
         Verifica nexus_routing_matrix e nexus_intent_capability.",
        intent_key, mode
    );
    RoutingDecision {
        provider: "__no_model__".to_string(),
        model: "__no_model__".to_string(),
        rationale: "no model available — verifica routing matrix + intent_capability",
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct SettingValueRow {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RoutingConfig {
    pub(crate) provider_hierarchy: Vec<String>,
    pub(crate) default_provider: Option<String>,
    pub(crate) default_model: Option<String>,
    pub(crate) token_budget: u32,
    pub(crate) max_token_budget: u32,
    pub(crate) provider_models: HashMap<String, String>,
    pub(crate) intent_provider_hierarchy: HashMap<String, Vec<String>>,
    pub(crate) behavior_mode: String,
}

impl RoutingConfig {
    pub(crate) fn from_settings(settings: &[SettingValueRow]) -> Self {
        let mut values = HashMap::new();
        for setting in settings {
            values.insert(setting.key.as_str(), setting.value.trim().to_string());
        }

        let provider_hierarchy = [
            "provider_hierarchy",
            "provider_priority",
            "provider_order",
            "fallback_order",
        ]
        .iter()
        .find_map(|key| parse_provider_list(values.get(key).map(String::as_str)))
        .unwrap_or_else(|| {
            parse_provider_list(values.get("default_provider").map(String::as_str)).unwrap_or_else(
                || {
                    KNOWN_PROVIDERS
                        .iter()
                        .map(|provider| (*provider).to_string())
                        .collect()
                },
            )
        });

        let default_provider = values
            .get("default_provider")
            .map(|value| value.to_lowercase())
            .filter(|value| !value.is_empty());
        let default_model = values
            .get("default_model")
            .cloned()
            .filter(|value| !value.is_empty());

        let token_budget = values
            .get("token_budget")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(4096);
        let max_token_budget = values
            .get("max_token_budget")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(token_budget.max(4096));

        let mut provider_models = HashMap::new();
        for provider in KNOWN_PROVIDERS {
            for key in [
                format!("provider_model_{provider}"),
                format!("{provider}_model"),
            ] {
                if let Some(value) = values.get(key.as_str()).filter(|value| !value.is_empty()) {
                    provider_models.insert(provider.to_string(), value.clone());
                    break;
                }
            }
        }

        let mut intent_provider_hierarchy = HashMap::new();
        for intent in KNOWN_INTENTS {
            let keys = [
                format!("routing_{intent}_providers"),
                format!("{intent}_provider_hierarchy"),
                format!("{intent}_providers"),
            ];
            if let Some(providers) = keys
                .iter()
                .find_map(|key| parse_provider_list(values.get(key.as_str()).map(String::as_str)))
            {
                intent_provider_hierarchy.insert(intent.to_string(), providers);
            }
        }

        let behavior_mode = values
            .get("nexus_behavior_mode")
            .cloned()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "bilanciata".to_string());

        Self {
            provider_hierarchy,
            default_provider,
            default_model,
            token_budget,
            max_token_budget,
            provider_models,
            intent_provider_hierarchy,
            behavior_mode,
        }
    }

    pub(crate) fn resolve_token_budget(&self, suggested_budget: Option<u32>) -> u32 {
        let requested = suggested_budget.unwrap_or(self.token_budget).max(1);
        requested.min(self.max_token_budget.max(1))
    }

    pub(crate) fn candidates(&self, intent: &str, suggested_provider: Option<&str>) -> Vec<String> {
        let mut providers = Vec::new();

        if let Some(intent_chain) = self.intent_provider_hierarchy.get(intent) {
            for provider in intent_chain {
                push_unique(&mut providers, provider.clone());
            }
        }

        for provider in &self.provider_hierarchy {
            push_unique(&mut providers, provider.clone());
        }

        if let Some(provider) = self.default_provider.as_ref() {
            push_unique(&mut providers, provider.clone());
        }

        if let Some(provider) = suggested_provider
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
        {
            push_unique(&mut providers, provider.to_lowercase());
        }

        for provider in KNOWN_PROVIDERS {
            push_unique(&mut providers, provider.to_string());
        }

        providers
    }

    pub(crate) fn resolve_model(
        &self,
        matrix: &crate::routing_matrix::RoutingMatrix,
        provider: &str,
        suggested_provider: Option<&str>,
        suggested_model: Option<&str>,
    ) -> String {
        if let Some(model) = self.provider_models.get(provider) {
            return model.clone();
        }

        if self.default_provider.as_deref() == Some(provider) {
            if let Some(model) = self
                .default_model
                .as_ref()
                .filter(|value| !value.is_empty())
            {
                return model.clone();
            }
        }

        if suggested_provider == Some(provider) {
            if let Some(model) = suggested_model.filter(|value| !value.is_empty()) {
                return model.to_string();
            }
        }

        default_model_for_provider(matrix, provider)
    }
}

pub(crate) fn parse_provider_list(value: Option<&str>) -> Option<Vec<String>> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }

    let parsed = if raw.starts_with('[') {
        serde_json::from_str::<Vec<String>>(raw).ok()?
    } else {
        raw.split(',')
            .map(|provider| provider.trim().to_lowercase())
            .filter(|provider| !provider.is_empty())
            .collect::<Vec<_>>()
    };

    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

pub(crate) fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !values.iter().any(|value| value == &candidate) {
        values.push(candidate);
    }
}

/// Modello di default per un provider, letto dalla matrice DB
/// (`nexus_provider_default_model`, vedi migrazione 0101).
///
/// La matrice e' SEMPRE popolata: in caso di DB irraggiungibile,
/// `RoutingMatrix::fallback_safe()` riempie i 5 provider standard
/// (openai, anthropic, google, mistral, deepseek) con modelli letti
/// da env var `NEXUS_FALLBACK_<PROVIDER>_MODEL` o, in ultima istanza,
/// dal fallback hardcoded di emergenza in fallback_safe().
///
/// Se viene richiesto un provider sconosciuto (non in DB ne' nei 5
/// standard), ritorna un placeholder `unknown-provider-model` che
/// triggera errore 400 dal layer chiamante. NON c'e' fallback al
/// modello "gpt-4o-mini" hardcoded come prima.
pub fn default_model_for_provider(
    matrix: &crate::routing_matrix::RoutingMatrix,
    provider: &str,
) -> String {
    matrix
        .default_model(provider)
        .unwrap_or_else(|| {
            tracing::warn!(
                "default_model_for_provider: provider '{}' non presente nella matrice DB ne' nei 5 standard. \
                 Aggiungilo via UPDATE/INSERT su nexus_provider_default_model.",
                provider
            );
            format!("unknown-provider-{}", provider)
        })
}

pub(crate) fn completion_has_error(completion: &Value) -> bool {
    // metadata.error presente e non-null → errore
    if let Some(err) = completion
        .get("metadata")
        .and_then(|metadata| metadata.get("error"))
    {
        if !err.is_null() {
            return true;
        }
    }
    // campo error a root presente e non-null → errore
    if let Some(err) = completion.get("error") {
        if !err.is_null() {
            return true;
        }
    }
    completion
        .get("content")
        .and_then(Value::as_str)
        .map(|content| {
            let trimmed = content.trim_start();
            trimmed.starts_with("[Error:") || trimmed.starts_with("[error:")
        })
        .unwrap_or(false)
}

// ────────────────────────────────────────────────────────────────────────────
// Gate di capability sul routing agentico (ADR 0018, leva 0).
// ────────────────────────────────────────────────────────────────────────────

/// Decisione del gate di capability per un run AGENTICO.
///
/// Un run agentico (intent != "chat") deve usare SOLO modelli con
/// `ai_price_catalog.supports_tool_use = true`. Questa enum separa la LOGICA
/// di decisione (pura, testabile senza DB) dall'I/O (query catalog + fallback),
/// che resta nel chiamante `resolve_agent_provider_detailed`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ToolCapabilityGate {
    /// Il modello risolto e' utilizzabile cosi' com'e'. Nessuna sostituzione.
    /// Casi: intent non agentico, gate disabilitato, oppure modello gia'
    /// tool-capable.
    KeepOriginal,
    /// Il modello risolto NON e' tool-capable: il chiamante deve cercare un
    /// fallback tool-capable (via `best_model_for_tier` o default provider).
    NeedsFallback,
}

/// Decide se applicare il gate di capability tool-use al modello risolto.
///
/// Funzione PURA: nessun accesso DB. Il chiamante fornisce i fatti gia' letti
/// (intent agentico?, flag abilitato?, il modello supporta tool_use?).
///
/// Regole:
/// - intent == "chat" (non agentico) -> `KeepOriginal` (il gate non si applica
///   ai task non-agentici: classify, title, vision, embedding, completion).
/// - gate disabilitato (`agent.require_tool_use_capability` = false) ->
///   `KeepOriginal`.
/// - intent agentico + gate abilitato + modello NON tool-capable ->
///   `NeedsFallback`.
/// - se la capability del modello e' sconosciuta (`None`, es. modello assente
///   dal catalog) il gate e' CONSERVATIVO: NON sostituisce (KeepOriginal), per
///   non degradare un modello potenzialmente valido solo perche' manca dal
///   catalog. La mancanza nel catalog e' un problema di sync separato.
/// - `agentic_thinking_policy == "exclude"` (reasoning-only SENZA function
///   calling, es. deepseek-reasoner) su run agentico -> `NeedsFallback`.
///   I dual-mode (`disable_for_tools`) e i reasoning con tool nativi (`native`)
///   restano ammessi: l'adapter del brain gestisce la modalita' (ADR 0025).
///   `None` resta conservativo (KeepOriginal).
pub(crate) fn decide_tool_capability_gate(
    intent: &str,
    gate_enabled: bool,
    model_supports_tool_use: Option<bool>,
    agentic_thinking_policy: Option<&str>,
) -> ToolCapabilityGate {
    // Il gate si applica SOLO ai run agentici. Convenzione del progetto:
    // intent == "chat" e' l'unico intent non-agentico (vedi agent_run.rs:1206
    // `intent_uses_tools = classified_intent_for_loop != "chat"`).
    if intent == "chat" {
        return ToolCapabilityGate::KeepOriginal;
    }
    if !gate_enabled {
        return ToolCapabilityGate::KeepOriginal;
    }
    // Policy 'exclude': reasoning-only senza function calling -> non puo' reggere
    // un loop agentico, serve fallback. Ha priorita' sul check tool_use.
    if agentic_thinking_policy == Some("exclude") {
        return ToolCapabilityGate::NeedsFallback;
    }
    match model_supports_tool_use {
        // Modello esplicitamente non tool-capable -> serve fallback.
        Some(false) => ToolCapabilityGate::NeedsFallback,
        // Tool-capable, oppure capability ignota (conservativo) -> tieni.
        Some(true) | None => ToolCapabilityGate::KeepOriginal,
    }
}

#[cfg(test)]
mod tool_capability_gate_tests {
    use super::{compute_no_capable_provider, decide_tool_capability_gate, ToolCapabilityGate};

    #[test]
    fn gate_scarta_modello_non_tool_capable_su_intent_agentico() {
        // mistral-code-latest (supports_tool_use=false) su intent agentico -> fallback.
        let d = decide_tool_capability_gate("file_ops", true, Some(false), Some("none"));
        assert_eq!(d, ToolCapabilityGate::NeedsFallback);
    }

    #[test]
    fn gate_lascia_passare_modello_tool_capable() {
        let d = decide_tool_capability_gate("refactor", true, Some(true), Some("none"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_disattivato_lascia_passare_anche_modello_non_capable() {
        // Flag agent.require_tool_use_capability = false -> passthrough.
        let d = decide_tool_capability_gate("file_ops", false, Some(false), Some("exclude"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_non_si_applica_a_intent_chat() {
        // Run non agentico: il gate non interviene neppure su policy 'exclude'.
        let d = decide_tool_capability_gate("chat", true, Some(false), Some("exclude"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_conservativo_su_capability_ignota() {
        // Modello assente dal catalog (None): non degradare.
        let d = decide_tool_capability_gate("debug", true, None, None);
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    // ── ADR 0025: eleggibilita' agentica via agentic_thinking_policy ──

    #[test]
    fn gate_scarta_policy_exclude_su_intent_agentico() {
        // deepseek-reasoner: reasoning-only SENZA function calling (exclude) -> fallback.
        let d = decide_tool_capability_gate("fix", true, Some(true), Some("exclude"));
        assert_eq!(d, ToolCapabilityGate::NeedsFallback);
    }

    #[test]
    fn gate_dual_mode_passa_su_agentico() {
        // deepseek-v4 (disable_for_tools): tool-capable, NON escluso -> KeepOriginal
        // (l'adapter del brain forza il non-thinking nel loop tool).
        let d = decide_tool_capability_gate("fix", true, Some(true), Some("disable_for_tools"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_native_reasoning_passa_su_agentico() {
        // o3 (native): reasoning con tool nativi -> KeepOriginal.
        let d = decide_tool_capability_gate("debug", true, Some(true), Some("native"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_exclude_lecito_su_chat() {
        // Su intent non agentico anche 'exclude' e' lecito (nessun tool-forcing).
        let d = decide_tool_capability_gate("chat", true, Some(true), Some("exclude"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    // ── ADR 0020: regola di disponibilita' provider (cooldown vincolante) ──

    #[test]
    fn auto_provider_in_cooldown_e_no_capable() {
        // Modalita' AUTO, provider scelto in cooldown -> no_capable.
        assert!(compute_no_capable_provider(false, true, false));
    }

    #[test]
    fn auto_tutti_in_cooldown_e_no_capable() {
        // Modalita' AUTO, tutti i provider noti in cooldown -> no_capable.
        assert!(compute_no_capable_provider(false, false, true));
    }

    #[test]
    fn auto_provider_disponibile_non_e_no_capable() {
        // Modalita' AUTO, provider scelto fuori cooldown -> capable.
        assert!(!compute_no_capable_provider(false, false, false));
    }

    #[test]
    fn user_override_bypassa_il_cooldown() {
        // Forzatura utente: anche se il provider scelto e' in cooldown e tutti
        // i provider noti lo sono, la scelta NON e' mai no_capable (l'utente
        // decide consapevolmente, ADR 0020).
        assert!(!compute_no_capable_provider(true, true, true));
        assert!(!compute_no_capable_provider(true, true, false));
        assert!(!compute_no_capable_provider(true, false, true));
    }
}
