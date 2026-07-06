//! Adapter del trait [`nexus_agent_graph::runtime::ports::EscalationPort`].
//!
//! IMPLEMENTA (FASE 2b) `EscalationPort::escalation_inputs` risolvendo gli input
//! dell'auto-escalation:
//!   1. catena intra-provider DERIVATA dalla vista `v_model_escalation_chain`
//!      (mig 0471, punto unico regola L) — la vecchia tabella seed
//!      `nexus_model_escalation_chain` (mig 0128) e' stata droppata (mig 0474);
//!      `chain_for` enumera i modelli del provider con `escalation_rank`
//!      superiore al corrente, ordinati ASC;
//!   2. stato cooldown del provider corrente dalla FONTE UNICA del gate (ADR 0020,
//!      `crate::provider_cooldown::is_provider_in_cooldown`);
//!   3. candidato cross-provider risolvendo il purpose `loop_fallback_default`
//!      dalla routing matrix (regola G, `internal_routing::resolve_purpose_model_db`).
//!
//! FILTRO PROVIDER REGISTRATI (nota verifica PR-J1): il signature-loop Python ha la
//! guardia `_providers._providers.get(provider)` (`__init__.py:3200`): se la chain
//! DB punta a un provider NON disponibile runtime, il candidato Tier 1 viene
//! scartato e si cade al Tier 2 cross-provider. Qui replichiamo l'intento in modo
//! DB-driven (regola G): un provider e' "disponibile" se ha una API key configurata
//! in `settings` (categoria `providers`, `<provider>_api_key` non vuota). Se il
//! provider corrente NON e' disponibile, la catena (intra-provider) viene
//! AZZERATA: `pick_escalation_model` salta cosi' il Tier 1 e usa il cross-provider.
//!
//! FAIL-OPEN (sicurezza): su guasto di lettura (DB/router down) ritorna
//! `EscalationInputs::default()` (catena vuota, `provider_in_cooldown=false`,
//! `cross_provider=None`) -> la selezione risolve a `None` (chiusura secca), mai un
//! `PortError`. CONFINE (regola L): qui SOLO l'I/O; la SELEZIONE resta nel modulo
//! puro `nexus_agent_graph::decisions::escalation::pick_escalation_model`.

use async_trait::async_trait;
use sqlx::PgPool;
use std::collections::HashMap;

use nexus_agent_graph::decisions::escalation::{
    pick_failover_model, ChainEntry, CrossProviderCandidate, EscalationCandidate,
};
use nexus_agent_graph::decisions::governance::ModelTelemetry;
use nexus_agent_graph::runtime::ports::{EscalationInputs, EscalationPort, ExecMode, PortError};

use crate::governance_telemetry::{load_governance_policy, load_model_telemetry};
use crate::internal_routing::{resolve_purpose_model_db, PurposeResolution};
use crate::provider_cooldown::is_provider_in_cooldown;

/// Sentinelle del router cross-provider: NON sono provider reali (regola G), vanno
/// trattate come "nessun candidato" (parita' col Python `helpers.py:1753-1754`).
const SENTINELS: [&str; 2] = ["__router_unavailable__", "__no_capable_provider__"];

/// Adapter [`EscalationPort`] -> vista `v_model_escalation_chain` (mig 0471/0475)
/// + gate cooldown (ADR 0020) + purpose `loop_fallback_default` (routing matrix).
pub struct PgEscalationPort {
    /// Pool Postgres per la lettura della catena di escalation, dei provider
    /// disponibili (`settings`) e per la risoluzione del purpose cross-provider.
    db: PgPool,
}

impl PgEscalationPort {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// `true` se il provider e' disponibile runtime (API key configurata in
    /// `settings`, categoria `providers`, chiave `<provider>_api_key` non vuota).
    /// Replica DB-driven della guardia Python `_providers._providers.get(provider)`
    /// (`__init__.py:3200`): un provider senza chiave non e' realmente disponibile,
    /// quindi escalare su di lui sprecherebbe un turno. FAIL-OPEN: su errore DB
    /// ritorna `true` (NON priva il run del Tier 1 per un guasto infrastrutturale).
    async fn provider_available(&self, provider: &str) -> bool {
        let key = format!("{}_api_key", provider.trim().to_lowercase());
        match sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings \
             WHERE category = 'providers' AND key = $1 LIMIT 1",
        )
        .bind(&key)
        .fetch_optional(&self.db)
        .await
        {
            Ok(Some(value)) => !value.trim().is_empty(),
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(
                    provider = %provider,
                    error = %e,
                    "escalation_port: lettura disponibilita' provider fallita, fail-open disponibile"
                );
                true
            }
        }
    }

    /// Catena intra-provider per `(provider, base_model)` DERIVATA dal catalog
    /// (vista `v_model_escalation_chain`, mig 0471 - punto unico, regola L).
    /// Enumera TUTTI i modelli sani del provider con `escalation_rank` SUPERIORE
    /// al modello corrente, ordinati `escalation_rank ASC` (dal piu' economico/
    /// leggero al piu' capace): catena ricca multi-livello, sempre allineata al
    /// catalog (un nuovo modello abilitato entra da solo). Resiliente: la vista
    /// filtra `is_enabled = TRUE` (modelli auto-disabilitati esclusi -> mai
    /// escalation verso modelli morti); `supports_tool_use = TRUE` perche'
    /// l'escalation serve a uscire da loop agentici (un modello senza tool non
    /// aiuta). `COALESCE(..., -1)`: se il modello corrente non e' (piu') nel
    /// catalog, parte dall'intera catena del provider. Vuota su errore (fail-open).
    ///
    /// FINESTRA-AWARE (NON-convergenza, regola H): esclude i modelli con
    /// `context_window` STRETTAMENTE minore di quello del modello corrente. Un
    /// modello "piu' capace per rank" ma con finestra piu' PICCOLA (incidente reale
    /// deepseek 1M -> deepseek-chat 131K) manderebbe in context-overflow un run gia'
    /// vicino al limite di contesto, peggiorando lo stallo invece di risolverlo. Il
    /// filtro vive qui (I/O) perche' la window e' gia' disponibile dalla vista; la
    /// SELEZIONE resta nel modulo puro `pick_escalation_model` (confine regola L).
    /// Se il modello corrente non e' nel catalog (`window=0`) il filtro e' inattivo
    /// (nessun riferimento), coerente col fail-open.
    async fn chain_for(&self, provider: &str, base_model: &str) -> Vec<ChainEntry> {
        if provider.trim().is_empty() || base_model.trim().is_empty() {
            return Vec::new();
        }
        // Window del modello corrente (0 se non in catalog -> filtro inattivo).
        let current_window = self.model_window(provider, base_model).await;
        // FIX-A (scale-controller): la vista espone gia' `performance_tier`; lo
        // selezioniamo insieme al modello cosi' il tier del modello promosso viaggia
        // nella `ChainEntry` fino al pick, SENZA lookup extra (regola L/H: il DB e'
        // gia' interrogato qui per derivare la catena).
        match sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT model, performance_tier FROM v_model_escalation_chain \
             WHERE provider = $1 \
               AND supports_tool_use = TRUE \
               AND escalation_rank > COALESCE( \
                     (SELECT escalation_rank FROM v_model_escalation_chain \
                       WHERE provider = $1 AND model = $2), -1) \
               AND context_window >= $3 \
             ORDER BY escalation_rank ASC",
        )
        .bind(provider)
        .bind(base_model)
        .bind(current_window)
        .fetch_all(&self.db)
        .await
        {
            Ok(rows) => rows
                .into_iter()
                .map(|(escalation_model, tier)| ChainEntry {
                    escalation_model,
                    tier: tier.filter(|t| !t.trim().is_empty()),
                })
                .collect(),
            Err(e) => {
                tracing::warn!(
                    provider = %provider,
                    error = %e,
                    "escalation_port: derivazione catena dal catalog fallita, fail-open catena vuota"
                );
                Vec::new()
            }
        }
    }

    /// Performance tier di `(provider, model)` dal catalog (vista
    /// `v_model_escalation_chain`, colonna `performance_tier`). `None` se il modello
    /// non e' nel catalog o su errore (fail-open: il chiamante ricade sul default
    /// `medium` a valle, comportamento invariato). Punto unico (regola L/H) della
    /// lettura del tier per il candidato cross-provider dell'escalation, che — a
    /// differenza della catena intra-provider — arriva dal router purpose e NON porta
    /// il tier con se' (FIX-A scale-controller). Il DB e' gia' interrogato in questo
    /// ramo (`cross_provider` legge anche la finestra), quindi non e' un lookup sparso.
    async fn model_tier(&self, provider: &str, model: &str) -> Option<String> {
        if provider.trim().is_empty() || model.trim().is_empty() {
            return None;
        }
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT performance_tier FROM v_model_escalation_chain \
             WHERE provider = $1 AND model = $2 LIMIT 1",
        )
        .bind(provider)
        .bind(model)
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten()
        .flatten()
        .filter(|t| !t.trim().is_empty())
    }

    /// Context window (token) di `(provider, model)` dal catalog (vista
    /// `v_model_escalation_chain`). `0` se il modello non e' nel catalog o su errore
    /// (fail-open: il chiamante tratta `0` come "finestra ignota" -> nessun filtro
    /// window-aware, comportamento storico). Punto unico (regola L) della lettura
    /// della finestra per l'escalation finestra-aware (catena intra + cross-provider).
    async fn model_window(&self, provider: &str, model: &str) -> i64 {
        if provider.trim().is_empty() || model.trim().is_empty() {
            return 0;
        }
        sqlx::query_scalar::<_, i64>(
            "SELECT context_window::bigint FROM v_model_escalation_chain \
             WHERE provider = $1 AND model = $2 LIMIT 1",
        )
        .bind(provider)
        .bind(model)
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
    }

    /// Candidato cross-provider (`loop_fallback_default`) dal router. Eleggibile
    /// SOLO la variante `Resolved` (tier-only, niente fallback hardcoded: ogni
    /// altro esito — NotFound / NoCapableModel / MatrixUnavailable — NON e' un
    /// candidato valido, regola G/H). `None` anche su sentinella o coppia vuota.
    /// Best-effort: ogni esito non-risolto -> `None`.
    ///
    /// FINESTRA-AWARE (NON-convergenza, regola H): se la coppia corrente e' nota e
    /// ha una finestra nota (`current_window > 0`), il candidato cross-provider con
    /// `context_window` STRETTAMENTE minore viene SCARTATO (evita il downgrade di
    /// finestra che manda in overflow, come per la catena intra-provider). Se la
    /// finestra del candidato e' ignota (`0`, non in catalog) il filtro e' inattivo
    /// (fail-open: meglio offrire il cross-provider che restare bloccati).
    async fn cross_provider(
        &self,
        current_provider: Option<&str>,
        current_model: Option<&str>,
    ) -> Option<CrossProviderCandidate> {
        let (provider, model) =
            match resolve_purpose_model_db(&self.db, "loop_fallback_default").await {
                PurposeResolution::Resolved {
                    provider, model, ..
                } => (provider, model),
                _ => return None,
            };
        if SENTINELS.contains(&provider.as_str()) || SENTINELS.contains(&model.as_str()) {
            return None;
        }
        if provider.trim().is_empty() || model.trim().is_empty() {
            return None;
        }
        // Downgrade-finestra guard: scarta il cross-provider se ha finestra nota e
        // STRETTAMENTE minore di quella corrente (entrambe note).
        if let (Some(cp), Some(cm)) = (current_provider, current_model) {
            let current_window = self.model_window(cp, cm).await;
            if current_window > 0 {
                let candidate_window = self.model_window(&provider, &model).await;
                if candidate_window > 0 && candidate_window < current_window {
                    tracing::info!(
                        cross_provider = %provider,
                        cross_model = %model,
                        candidate_window,
                        current_window,
                        "escalation_port: cross-provider scartato (finestra piu' piccola della corrente)"
                    );
                    return None;
                }
            }
        }
        // FIX-A (scale-controller): risolvi il tier del candidato dal catalog cosi'
        // viaggia nel pick fino a `current_tier` (regola L/H: DB gia' interrogato in
        // questo ramo per la finestra). `None` -> default `medium` a valle.
        let tier = self.model_tier(&provider, &model).await;
        Some(CrossProviderCandidate {
            provider,
            model,
            tier,
        })
    }

    /// PUNTO UNICO (regola L) dell'arricchimento candidati: triple
    /// `(provider, model, tier)` -> [`EscalationCandidate`] con telemetria
    /// strutturata (regola M, batch: una query per l'intero insieme) e flag
    /// cooldown (gate ADR 0020, segnale forte anche senza storico probe).
    /// Condiviso da `escalation_inputs` e `failover_provider`.
    ///
    /// `load_telemetry=false` (replay): telemetria default (sano) -> il ranking
    /// a valle si riduce a tier + ordine d'ingresso, replay-stabile.
    async fn enrich_candidates(
        &self,
        pmt: Vec<(String, String, Option<String>)>,
        load_telemetry: bool,
    ) -> Vec<EscalationCandidate> {
        let tmap: HashMap<(String, String), ModelTelemetry> = if load_telemetry {
            let pm: Vec<(String, String)> =
                pmt.iter().map(|(p, m, _)| (p.clone(), m.clone())).collect();
            load_model_telemetry(&self.db, &pm)
                .await
                .into_iter()
                .map(|t| (ModelTelemetry::key(&t.provider, &t.model), t))
                .collect()
        } else {
            HashMap::new()
        };
        pmt.into_iter()
            .map(|(p, m, tier)| {
                let mut telemetry = tmap
                    .get(&ModelTelemetry::key(&p, &m))
                    .cloned()
                    .unwrap_or_default();
                telemetry.provider_in_cooldown =
                    telemetry.provider_in_cooldown || is_provider_in_cooldown(&p);
                EscalationCandidate {
                    provider: p,
                    model: m,
                    tier,
                    telemetry,
                }
            })
            .collect()
    }
}

#[async_trait]
impl EscalationPort for PgEscalationPort {
    /// Risolve gli input dell'escalation per il turno corrente. SOLA LETTURA.
    /// FAIL-OPEN: ogni sotto-lettura degrada a vuoto, mai un `PortError`.
    ///
    /// La catena Tier 1 puo' essere RIORDINATA per probabilita' di successo
    /// (governance telemetria-aware, `maybe_rank_chain`): gata `mode` (solo Real) e
    /// dietro il flag `agent.governance.telemetry_aware` (OFF = bit-identico).
    async fn escalation_inputs(
        &self,
        _intent: Option<&str>,
        provider: Option<&str>,
        model: Option<&str>,
        mode: ExecMode,
    ) -> Result<EscalationInputs, PortError> {
        // Candidato cross-provider (loop_fallback_default), sempre risolto.
        let cross = self.cross_provider(provider, model).await;

        // Catena intra-provider (modelli piu' forti dello stesso provider): solo se
        // provider+model valorizzati E il provider e' disponibile runtime (API key):
        // escalare su un provider senza chiave sprecherebbe un turno.
        let intra: Vec<ChainEntry> = match (provider, model) {
            (Some(p), Some(m)) if !p.trim().is_empty() && !m.trim().is_empty() => {
                if self.provider_available(p).await {
                    self.chain_for(p, m).await
                } else {
                    tracing::info!(
                        provider = %p,
                        "escalation_port: provider corrente non disponibile runtime, \
                         catena intra saltata (si usera' il cross-provider)"
                    );
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };

        // Insieme UNIFICATO (provider, model, tier): intra (provider corrente) + cross.
        // Niente split Tier1/Tier2 ne' indice posizionale: la SELEZIONE agentica
        // (salute -> tier -> likelihood) e' del modulo puro pick_escalation_model.
        let cur_provider = provider.unwrap_or("").trim().to_string();
        let mut pmt: Vec<(String, String, Option<String>)> = intra
            .into_iter()
            .map(|e| (cur_provider.clone(), e.escalation_model, e.tier))
            .collect();
        if let Some(c) = &cross {
            pmt.push((c.provider.clone(), c.model.clone(), c.tier.clone()));
        }

        let policy = load_governance_policy(&self.db).await;

        // Telemetria: caricata SOLO in Real. In Replay resterebbe non-deterministica
        // (parita' shadow), quindi telemetria default (sano) -> il ranking si riduce a
        // tier + ordine d'ingresso, replay-stabile (come le altre decisioni gata mode).
        let candidates = self
            .enrich_candidates(pmt, mode == ExecMode::Real)
            .await;

        Ok(EscalationInputs { candidates, policy })
    }

    /// FAILOVER su provider caduto: selezione AGENTICA del SOSTITUTO. Enumera
    /// TUTTI i candidati agentici ammissibili (OGNI tier — nessun pavimento ne'
    /// catena — esclusi i provider in cooldown, gate ADR 0020, e quelli gia'
    /// provati `exclude`) dal punto unico di eleggibilita'
    /// ([`crate::orchestrator::model_routing::agentic_failover_candidates`],
    /// regola L), li arricchisce di telemetria strutturata (regola M, batch) e
    /// DELEGA la scelta al modulo puro `pick_failover_model`: salute ->
    /// `likelihood * affinita' di tier`. Il tier del modello CADUTO e' una
    /// INDICAZIONE (dal chiamante, o risolto dal catalog se assente), mai un
    /// filtro. FAIL-OPEN: errore di lettura -> `None`.
    async fn failover_provider(
        &self,
        current_provider: Option<&str>,
        current_model: Option<&str>,
        current_tier: Option<&str>,
        exclude: &[String],
    ) -> Result<Option<CrossProviderCandidate>, PortError> {
        let pool =
            crate::orchestrator::model_routing::agentic_failover_candidates(&self.db, exclude)
                .await;
        if pool.is_empty() {
            return Ok(None);
        }

        // Arricchimento dal punto unico condiviso con `escalation_inputs`
        // (telemetria batch + cooldown, regola L). Il failover e' sempre Real.
        let policy = load_governance_policy(&self.db).await;
        let candidates = self.enrich_candidates(pool, true).await;

        // Tier corrente come INDICAZIONE: dal chiamante se noto, altrimenti
        // risolto dal catalog (fail-open: None = medium neutro nel modulo puro).
        let cur_tier = match current_tier.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => Some(t.to_string()),
            None => match (current_provider, current_model) {
                (Some(p), Some(m)) => self.model_tier(p, m).await,
                _ => None,
            },
        };

        let pick = pick_failover_model(&candidates, cur_tier.as_deref(), &policy);
        if let Some(ref c) = pick {
            tracing::info!(
                target: "nexus_mcp_core::escalation_port",
                failover_provider = %c.provider,
                failover_model = %c.model,
                failover_tier = c.tier.as_deref().unwrap_or("?"),
                current_tier = cur_tier.as_deref().unwrap_or("?"),
                candidates = candidates.len(),
                excluded = exclude.len(),
                "failover_provider: sostituto scelto agenticamente (salute+likelihood, tier come indicazione)"
            );
        }
        Ok(pick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Schema minimale per i test: `settings` (per la disponibilita' provider) +
    /// `nexus_purpose_model` VUOTA (cosi' `resolve_purpose_model_db` ritorna
    /// `NotFound` -> `cross_provider = None`: isoliamo il Tier 1 senza dipendere
    /// da routing) + `ai_price_catalog` con la vista derivata `v_model_escalation_chain`
    /// (mig 0471/0475), da cui `chain_for` legge la catena. La tabella seed
    /// `nexus_model_escalation_chain` (mig 0128) e' stata droppata (mig 0474):
    /// non esiste piu' qui.
    async fn create_schema(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE settings ( \
                 key TEXT PRIMARY KEY, \
                 value TEXT NOT NULL, \
                 category TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("create settings");
        sqlx::query(
            "CREATE TABLE nexus_purpose_model ( \
                 purpose TEXT PRIMARY KEY, \
                 tier TEXT, \
                 required_capability TEXT, \
                 requires_tool_use BOOLEAN NOT NULL DEFAULT false \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_purpose_model");
        // Catalog dal punto unico condiviso (regola L, `test_support`): la stessa
        // tabella serve sia la vista v_model_escalation_chain sia il punto unico
        // `select_models_tierchain` usato dal failover agentico.
        crate::test_support::create_ai_price_catalog_table(pool).await;
        // Storico health per la telemetria del failover (`load_model_telemetry`):
        // vuoto = telemetria neutra dai soli contatori del catalog.
        sqlx::query(
            "CREATE TABLE ai_model_health_history ( \
                 provider TEXT NOT NULL, \
                 model TEXT NOT NULL, \
                 healthy BOOLEAN NOT NULL, \
                 latency_ms BIGINT, \
                 error_kind TEXT, \
                 checked_at TIMESTAMPTZ NOT NULL DEFAULT now() \
             )",
        )
        .execute(pool)
        .await
        .expect("create ai_model_health_history");
        sqlx::query(
            "CREATE VIEW v_model_escalation_chain AS SELECT \
                 provider, model, performance_tier, speed_tier, is_enabled, \
                 consecutive_failures, consecutive_tool_failures, supports_tool_use, \
                 supports_vision, agentic_thinking_policy, capabilities, context_window, \
                 (input_cost_per_million_tokens * 0.75 + output_cost_per_million_tokens * 0.25) AS blended_cost, \
                 ((CASE performance_tier WHEN 'light' THEN 0 WHEN 'medium' THEN 1 WHEN 'heavy' THEN 2 ELSE 1 END) * 1000000 \
                  + round((input_cost_per_million_tokens * 0.75 + output_cost_per_million_tokens * 0.25) * 1000))::bigint AS escalation_rank, \
                 (CASE performance_tier WHEN 'light' THEN 0 WHEN 'medium' THEN 1 WHEN 'heavy' THEN 2 ELSE 1 END) AS performance_tier_ord \
             FROM ai_price_catalog WHERE is_enabled = TRUE",
        )
        .execute(pool)
        .await
        .expect("create v_model_escalation_chain");
    }

    /// Seed del catalog (sorgente della catena derivata). Tuple:
    /// (provider, model, performance_tier, input_cost, is_enabled, supports_tool_use).
    /// La `context_window` resta al default della tabella (8192): per i test che
    /// esercitano il filtro finestra-aware usa [`seed_catalog_window`].
    async fn seed_catalog(pool: &PgPool, rows: &[(&str, &str, &str, f64, bool, bool)]) {
        for (provider, model, tier, in_cost, enabled, tool) in rows {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                 (provider, model, performance_tier, input_cost_per_million_tokens, \
                  output_cost_per_million_tokens, is_enabled, supports_tool_use) \
                 VALUES ($1, $2, $3, $4, 0, $5, $6)",
            )
            .bind(provider)
            .bind(model)
            .bind(tier)
            .bind(in_cost)
            .bind(enabled)
            .bind(tool)
            .execute(pool)
            .await
            .expect("insert catalog row");
        }
    }

    /// Come [`seed_catalog`] ma con `context_window` esplicita (ultimo campo) per
    /// esercitare il filtro finestra-aware. Tuple:
    /// (provider, model, tier, input_cost, is_enabled, supports_tool_use, context_window).
    async fn seed_catalog_window(
        pool: &PgPool,
        rows: &[(&str, &str, &str, f64, bool, bool, i64)],
    ) {
        for (provider, model, tier, in_cost, enabled, tool, window) in rows {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                 (provider, model, performance_tier, input_cost_per_million_tokens, \
                  output_cost_per_million_tokens, is_enabled, supports_tool_use, context_window) \
                 VALUES ($1, $2, $3, $4, 0, $5, $6, $7)",
            )
            .bind(provider)
            .bind(model)
            .bind(tier)
            .bind(in_cost)
            .bind(enabled)
            .bind(tool)
            .bind(window)
            .execute(pool)
            .await
            .expect("insert catalog row con window");
        }
    }

    /// Marca un provider come disponibile inserendone la API key in `settings`.
    async fn set_api_key(pool: &PgPool, provider: &str, value: &str) {
        sqlx::query(
            "INSERT INTO settings (key, value, category) VALUES ($1, $2, 'providers')",
        )
        .bind(format!("{provider}_api_key"))
        .bind(value)
        .execute(pool)
        .await
        .expect("insert api key");
    }

    /// La catena e' DERIVATA dal catalog (vista v_model_escalation_chain): enumera
    /// i modelli del provider con escalation_rank > corrente, ordinati ASC
    /// (economico/leggero -> capace), esclusi is_enabled=false e supports_tool_use=false.
    #[sqlx::test]
    async fn catena_derivata_dal_catalog_ordina_per_rank(pool: PgPool) {
        create_schema(&pool).await;
        set_api_key(&pool, "anthropic", "sk-live").await;
        seed_catalog(
            &pool,
            &[
                // base corrente: rank piu' basso.
                ("anthropic", "claude-haiku-4-5", "medium", 0.25, true, true),
                // candidati sopra il base, costo crescente.
                ("anthropic", "claude-sonnet-4-6", "medium", 3.0, true, true),
                ("anthropic", "claude-opus-4-6", "heavy", 15.0, true, true),
                // disabilitato -> escluso dalla vista (is_enabled=false).
                ("anthropic", "claude-spento", "heavy", 1.0, false, true),
                // senza tool_use -> escluso da chain_for (escalation = loop agentici).
                ("anthropic", "claude-no-tool", "heavy", 1.0, true, false),
            ],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("anthropic"), Some("claude-haiku-4-5"), ExecMode::Real)
            .await
            .expect("fail-open: mai PortError");
        let models: Vec<&str> = inputs
            .candidates
            .iter()
            .map(|c| c.model.as_str())
            .collect();
        assert_eq!(
            models,
            vec!["claude-sonnet-4-6", "claude-opus-4-6"],
            "catena derivata ordinata per escalation_rank ASC, esclusi spento+no-tool"
        );
    }

    /// FIX-A (scale-controller): la `ChainEntry` porta il `performance_tier` del
    /// modello di destinazione, letto dalla vista insieme al modello (nessun lookup
    /// extra). Il pick a valle scrivera' `current_tier` con questo valore.
    #[sqlx::test]
    async fn catena_propaga_il_performance_tier(pool: PgPool) {
        create_schema(&pool).await;
        set_api_key(&pool, "anthropic", "sk-live").await;
        seed_catalog(
            &pool,
            &[
                ("anthropic", "claude-haiku-4-5", "medium", 0.25, true, true),
                ("anthropic", "claude-sonnet-4-6", "medium", 3.0, true, true),
                ("anthropic", "claude-opus-4-6", "heavy", 15.0, true, true),
            ],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("anthropic"), Some("claude-haiku-4-5"), ExecMode::Real)
            .await
            .expect("fail-open");
        let tiers: Vec<(&str, Option<&str>)> = inputs
            .candidates
            .iter()
            .map(|c| (c.model.as_str(), c.tier.as_deref()))
            .collect();
        assert_eq!(
            tiers,
            vec![
                ("claude-sonnet-4-6", Some("medium")),
                ("claude-opus-4-6", Some("heavy")),
            ],
            "ogni ChainEntry porta il performance_tier del catalog"
        );
    }

    /// FIX-A: `model_tier` legge il tier dalla vista; `None` se il modello non e' in
    /// catalog (fail-open) o su argomenti vuoti. Punto unico per il cross-provider e
    /// il failover (che arrivano dal router senza tier).
    #[sqlx::test]
    async fn model_tier_legge_il_tier(pool: PgPool) {
        create_schema(&pool).await;
        seed_catalog(&pool, &[("openai", "gpt-x", "heavy", 1.0, true, true)]).await;
        let port = PgEscalationPort::new(pool.clone());
        assert_eq!(port.model_tier("openai", "gpt-x").await.as_deref(), Some("heavy"));
        // Non in catalog -> None (default a valle).
        assert_eq!(port.model_tier("openai", "ignoto").await, None);
        // Argomenti vuoti -> None.
        assert_eq!(port.model_tier("", "x").await, None);
    }

    /// FINESTRA-AWARE (NON-convergenza, regola H): la catena intra-provider esclude
    /// i modelli con `context_window` STRETTAMENTE minore di quello corrente. Il
    /// modello corrente ha finestra grande (1M); il candidato "piu' capace per rank"
    /// ma con finestra piccola (131K) NON deve entrare in catena (manderebbe in
    /// overflow). Resta solo il candidato con finestra >= corrente.
    #[sqlx::test]
    async fn catena_esclude_finestra_piu_piccola(pool: PgPool) {
        create_schema(&pool).await;
        set_api_key(&pool, "deepseek", "sk-live").await;
        seed_catalog_window(
            &pool,
            &[
                // corrente: rank basso, finestra GRANDE (1M).
                ("deepseek", "deepseek-v4-flash", "medium", 0.10, true, true, 1_000_000),
                // piu' capace per rank ma finestra PICCOLA -> escluso (downgrade window).
                ("deepseek", "deepseek-chat", "heavy", 1.0, true, true, 131_072),
                // piu' capace E finestra >= corrente -> ammesso.
                ("deepseek", "deepseek-reasoner", "heavy", 2.0, true, true, 1_000_000),
            ],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("deepseek"), Some("deepseek-v4-flash"), ExecMode::Real)
            .await
            .expect("fail-open");
        let models: Vec<&str> = inputs
            .candidates
            .iter()
            .map(|c| c.model.as_str())
            .collect();
        assert_eq!(
            models,
            vec!["deepseek-reasoner"],
            "il candidato con finestra piu' piccola della corrente e' escluso"
        );
    }

    /// FINESTRA-AWARE: `model_window` legge la finestra dalla vista; `0` se il
    /// modello non e' in catalog (filtro inattivo -> fail-open). E' il punto unico
    /// usato sia dal filtro catena sia dal guard downgrade del cross-provider.
    #[sqlx::test]
    async fn model_window_legge_la_finestra(pool: PgPool) {
        create_schema(&pool).await;
        seed_catalog_window(
            &pool,
            &[("deepseek", "deepseek-v4-flash", "medium", 0.10, true, true, 1_000_000)],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        assert_eq!(port.model_window("deepseek", "deepseek-v4-flash").await, 1_000_000);
        // Modello non in catalog -> 0 (finestra ignota, filtro inattivo).
        assert_eq!(port.model_window("deepseek", "ignoto").await, 0);
        // Argomenti vuoti -> 0.
        assert_eq!(port.model_window("", "x").await, 0);
    }

    /// Provider corrente NON disponibile (nessuna API key) -> catena Tier 1
    /// AZZERATA (filtro PR-J1), anche se la tabella avrebbe righe.
    #[sqlx::test]
    async fn provider_non_registrato_azzera_la_catena(pool: PgPool) {
        create_schema(&pool).await;
        // NESSUNA api key per 'anthropic' -> provider non disponibile: la catena
        // viene azzerata a monte (provider_available=false), prima ancora di
        // leggere la vista, quindi non serve alcun seed del catalog.
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("anthropic"), Some("claude-haiku-4-5"), ExecMode::Real)
            .await
            .expect("fail-open");
        assert!(
            inputs.candidates.is_empty(),
            "provider non registrato -> Tier 1 saltato (catena vuota)"
        );
    }

    /// API key presente ma VUOTA -> provider non disponibile (catena azzerata).
    #[sqlx::test]
    async fn provider_con_api_key_vuota_azzera_la_catena(pool: PgPool) {
        create_schema(&pool).await;
        set_api_key(&pool, "anthropic", "   ").await;
        // API key vuota -> provider non disponibile: catena azzerata a monte,
        // la vista non viene nemmeno interrogata (nessun seed catalog necessario).
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("anthropic"), Some("claude-haiku-4-5"), ExecMode::Real)
            .await
            .expect("fail-open");
        assert!(inputs.candidates.is_empty(), "api key vuota -> provider non disponibile");
    }

    /// Provider/model assenti -> Tier 1 saltato (catena vuota), nessun cooldown.
    #[sqlx::test]
    async fn coppia_assente_catena_vuota(pool: PgPool) {
        create_schema(&pool).await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, None, None, ExecMode::Real)
            .await
            .expect("fail-open");
        assert!(inputs.candidates.is_empty());
    }

    /// Catena assente per la coppia corrente -> vuota (non un errore).
    #[sqlx::test]
    async fn coppia_senza_catena_ritorna_vuoto(pool: PgPool) {
        create_schema(&pool).await;
        set_api_key(&pool, "openai", "sk-live").await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("openai"), Some("gpt-4o-mini"), ExecMode::Real)
            .await
            .expect("fail-open");
        assert!(inputs.candidates.is_empty());
    }

    // ---- failover_provider: selezione agentica del sostituto ----

    /// REGRESSIONE (incidente run a0b6e0a9): cade un heavy, il vecchio failover
    /// ripartiva dal pavimento fisso 'medium' e sceglieva il piu' economico
    /// (v4-flash), IGNORANDO il high sano (v4-pro). Con la selezione agentica il
    /// tier corrente e' un'indicazione: vince il sostituto piu' vicino.
    #[sqlx::test]
    async fn failover_sceglie_il_sostituto_vicino_non_il_pavimento(pool: PgPool) {
        create_schema(&pool).await;
        seed_catalog(
            &pool,
            &[
                ("deepseek", "deepseek-v4-flash", "medium", 0.1, true, true),
                ("deepseek", "deepseek-v4-pro", "high", 1.0, true, true),
            ],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let pick = port
            .failover_provider(
                Some("google"),
                Some("gemini-3.5-flash"),
                Some("heavy"),
                &["google".to_string(), "mistral".to_string()],
            )
            .await
            .expect("fail-open")
            .expect("un sostituto sano esiste");
        assert_eq!(pick.provider, "deepseek");
        assert_eq!(pick.model, "deepseek-v4-pro");
        assert_eq!(pick.tier.as_deref(), Some("high"));
    }

    /// La salute (segnale strutturato, regola M) domina l'indicazione di tier:
    /// il candidato piu' vicino ma "recently_failed" (contatori del catalog)
    /// perde contro uno piu' lontano ma sano.
    #[sqlx::test]
    async fn failover_telemetria_retrocede_il_degradato(pool: PgPool) {
        create_schema(&pool).await;
        seed_catalog(
            &pool,
            &[
                ("deepseek", "deepseek-v4-pro", "high", 1.0, true, true),
                ("mistral", "mistral-large", "medium", 0.5, true, true),
            ],
        )
        .await;
        // 2 fallimenti consecutivi = soglia recently_failed della policy default.
        sqlx::query(
            "UPDATE ai_price_catalog SET consecutive_failures = 2 \
             WHERE provider = 'deepseek' AND model = 'deepseek-v4-pro'",
        )
        .execute(&pool)
        .await
        .expect("update failures");
        let port = PgEscalationPort::new(pool.clone());
        let pick = port
            .failover_provider(Some("google"), Some("g"), Some("heavy"), &["google".to_string()])
            .await
            .expect("fail-open")
            .expect("un sostituto sano esiste");
        assert_eq!(pick.model, "mistral-large");
    }

    /// I provider gia' provati (`exclude`) non rientrano mai nel pool.
    #[sqlx::test]
    async fn failover_esclude_i_provider_gia_provati(pool: PgPool) {
        create_schema(&pool).await;
        seed_catalog(
            &pool,
            &[
                ("google", "gemini-heavy", "heavy", 1.0, true, true),
                ("deepseek", "deepseek-v4-pro", "high", 1.0, true, true),
            ],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let pick = port
            .failover_provider(Some("google"), Some("gemini-heavy"), Some("heavy"), &[
                "google".to_string(),
            ])
            .await
            .expect("fail-open")
            .expect("resta deepseek");
        assert_eq!(pick.provider, "deepseek");
    }

    /// `current_tier` assente -> risolto dal catalog via (provider, model). Con la
    /// risoluzione attiva l'indicazione 'heavy' preferisce il high; senza (medium
    /// neutro) vincerebbe il medium (distanza 0): il test discrimina i due casi.
    #[sqlx::test]
    async fn failover_tier_corrente_risolto_dal_catalog(pool: PgPool) {
        create_schema(&pool).await;
        seed_catalog(
            &pool,
            &[
                ("google", "gemini-heavy", "heavy", 1.0, true, true),
                ("deepseek", "deepseek-v4-flash", "medium", 0.1, true, true),
                ("deepseek", "deepseek-v4-pro", "high", 1.0, true, true),
            ],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let pick = port
            .failover_provider(Some("google"), Some("gemini-heavy"), None, &[
                "google".to_string(),
            ])
            .await
            .expect("fail-open")
            .expect("un sostituto esiste");
        assert_eq!(pick.model, "deepseek-v4-pro");
    }

    /// Catalog vuoto (o tutto escluso) -> `None`, mai un errore (fail-open).
    #[sqlx::test]
    async fn failover_pool_vuoto_ritorna_none(pool: PgPool) {
        create_schema(&pool).await;
        let port = PgEscalationPort::new(pool.clone());
        let pick = port
            .failover_provider(Some("google"), Some("g"), Some("heavy"), &[])
            .await
            .expect("fail-open");
        assert!(pick.is_none());
    }
}
