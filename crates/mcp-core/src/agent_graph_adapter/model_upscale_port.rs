//! Adapter del trait [`nexus_agent_graph::runtime::ports::ModelUpscalePort`].
//!
//! IMPLEMENTERA' (FASE 2):
//! - `context_window` (lookup del context window del modello corrente da
//!   `ai_price_catalog`; `0` se ignoto, fail-open su errore);
//! - `select_upscale_model` (selezione dinamica dal catalog di un modello con
//!   `context_window >= required_tokens` nel tier configurato, capable per tool use,
//!   escluso `agentic_thinking_policy = 'exclude'`, col provider risolto).
//! Tier-based e DB-driven (regola G): nessun nome modello hardcoded; tier e flag
//! (`agent.upscale.*`) sono settings letti via `sqlx`. CONFINE (regola L): la
//! DECISIONE di SE fare upscale resta PURA in
//! `nexus_agent_graph::decisions::end_turn`; qui SOLO l'I/O. BEST-EFFORT:
//! `Ok(0)` / `Ok(None)` su guasto, mai `PortError`. SOLA LETTURA: nessun gate `mode`.

use async_trait::async_trait;
use sqlx::PgPool;

use nexus_agent_graph::runtime::ports::{ExecMode, ModelUpscalePort, PortError, UpscalePick};

use crate::orchestrator::select_agentic_model;

/// Adapter [`ModelUpscalePort`] -> `ai_price_catalog` + settings `agent.upscale.*`.
pub struct CatalogModelUpscalePort {
    /// Pool Postgres su cui girano i lookup catalog/settings dell'upscale.
    db: PgPool,
}

impl CatalogModelUpscalePort {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Tier target dello smart upscale dal setting `agent.upscale.target_tier`
    /// (regola G: niente nome modello/tier hardcoded nella logica; il default DB
    /// e' `'heavy'`, mig 0332). Se il setting manca usa `'heavy'` come da
    /// descrizione del setting stesso.
    async fn target_tier(&self) -> String {
        crate::settings::get_setting(&self.db, "agent.upscale.target_tier")
            .await
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "heavy".to_string())
    }
}

#[async_trait]
impl ModelUpscalePort for CatalogModelUpscalePort {
    /// Context window (token) del modello corrente da `ai_price_catalog`. `0` se
    /// ignoto (il chiamante salta l'upscale). Fail-open: errore -> `Ok(0)`.
    /// Deterministico se piu' provider espongono lo stesso `model`: prende il MAX
    /// context_window dichiarato (la window e' una proprieta' del modello, non del
    /// provider).
    async fn context_window(&self, model: &str) -> Result<i64, PortError> {
        let res = sqlx::query_scalar::<_, Option<i32>>(
            "SELECT MAX(context_window) FROM ai_price_catalog WHERE model = $1",
        )
        .bind(model)
        .fetch_optional(&self.db)
        .await;
        match res {
            Ok(Some(Some(w))) => Ok(w as i64),
            Ok(_) => Ok(0),
            Err(e) => {
                tracing::warn!(
                    model = %model,
                    error = %e,
                    "model_upscale_port: context_window query fallita, fail-open 0"
                );
                Ok(0)
            }
        }
    }

    /// Seleziona dal catalog un modello con `context_window >= required_tokens` nel
    /// tier configurato (`agent.upscale.target_tier`), tool-capable e
    /// `agentic_thinking_policy <> 'exclude'`, col provider risolto. DELEGA al
    /// selettore unico `select_models_tierchain` (regola L: la WHERE di
    /// eleggibilita' del catalog vive in UN solo posto). Ordina per
    /// `context_window DESC` (window piu' grande), tie-break su costo input ASC.
    /// `None` se nessun candidato o se il migliore coincide col modello corrente.
    /// Fail-open: errore -> `Ok(None)`.
    async fn select_upscale_model(
        &self,
        current_model: &str,
        required_tokens: i64,
    ) -> Result<Option<UpscalePick>, PortError> {
        let tier = self.target_tier().await;
        // SERVIZIO UNICO (regola L). `Exact{ScaleTarget}`: qui il tier NON e' un
        // requisito da soddisfare al meglio, e' il BERSAGLIO deciso a monte dal
        // modulo puro dello scale-controller — questa funzione lo ESEGUE, non lo
        // negozia. Degradare significherebbe fare l'opposto di un upscale. Con
        // `ExactReason` la differenza fra questo `&[tier]` (voluto) e quello di
        // `best_non_agentic_model` (che era un difetto) e' finalmente DICIBILE:
        // prima erano lo stesso identico codice.
        let req = crate::orchestrator::model_service::ModelRequest::agentic(&tier)
            .tier_policy(crate::orchestrator::model_service::TierPolicy::Exact {
                why: crate::orchestrator::model_service::ExactReason::ScaleTarget,
            })
            .rank(crate::orchestrator::model_service::Rank::WidestWindow)
            .min_context_window(required_tokens);
        let choice = match crate::orchestrator::model_service::select_model(&self.db, &req).await {
            Ok(c) => c,
            // Fail-open invariato: nessun bersaglio -> nessun upscale, non un run
            // morto. Il motivo TIPIZZATO distingue "il tier e' vuoto" (atteso) da
            // un guasto del catalog: solo il secondo merita un WARN.
            Err(reason) => {
                if !reason.is_expected() {
                    tracing::warn!(
                        current_model = %current_model,
                        required_tokens,
                        motivo = ?reason,
                        "model_upscale_port: select_upscale_model senza candidati, fail-open None"
                    );
                }
                return Ok(None);
            }
        };
        let (provider, model) = (choice.provider, choice.model);
        // Se il migliore coincide col modello corrente non c'e' upscale da fare.
        if model == current_model {
            return Ok(None);
        }
        Ok(Some(UpscalePick {
            provider,
            model,
            reason: format!(
                "context_overflow:required={required_tokens}:tier={tier}:from={current_model}"
            ),
            // Tier come CAMPO strutturato (regola M): il chiamante lo legge da qui,
            // non parsando `reason`. E' il vincolo di selezione (target_tier), quindi
            // il modello scelto e' garantito di quel tier.
            tier,
        }))
    }

    /// Selezione BIDIREZIONALE per lo SCALE-CONTROLLER (PR-B3): DELEGA al PUNTO
    /// UNICO `select_agentic_model` (regola L) col `tier` target e il
    /// `min_context_window` richiesto (FIX-B: nel downscale = est_tokens*overhead).
    /// GATE mode opzione A: in Replay ritorna `Ok(None)` (il rientro rilegge lo
    /// sticky checkpointato -> nessun I/O di risoluzione, parita' shadow). Fail-open
    /// gia' incorporato: `select_agentic_model` ritorna `None` su guasto/nessun
    /// candidato (nessun panico), che qui e' `Ok(None)` -> il chiamante ANNULLA il
    /// cambio-tier (fail-safe, mantiene il modello corrente).
    async fn select_model_for_tier(
        &self,
        tier: &str,
        min_context_window: i64,
        capability: Option<&str>,
        exclude_providers: &[String],
        mode: ExecMode,
    ) -> Result<Option<(String, String)>, PortError> {
        if mode != ExecMode::Real {
            // Opzione A: nessuna risoluzione in Replay (lo sticky del primario e' la
            // fonte di verita' checkpointata; risolvere qui divergerebbe se il
            // catalog e' cambiato tra primario e resume).
            return Ok(None);
        }
        // Stesso ordinamento del routing agentico (AGENTIC_COST_FIRST_ORDER): a
        // tier fissato prende il modello piu' economico che soddisfa i vincoli
        // (context_window incluso), is_featured solo come tie-break.
        let picked = select_agentic_model(
            &self.db,
            &[tier],
            capability,
            min_context_window,
            exclude_providers,
            crate::orchestrator::model_routing::AGENTIC_COST_FIRST_ORDER,
        )
        .await;
        Ok(picked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_schema(pool: &PgPool) {
        // Schema `ai_price_catalog` dal punto unico condiviso (regola L): una sola
        // definizione per tutti i #[sqlx::test] del crate, allineata alle colonne
        // lette da `select_models_tierchain`. Qui in piu' serve `settings` per i
        // lookup `agent.upscale.*`.
        crate::test_support::create_ai_price_catalog_table(pool).await;
        sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(pool)
            .await
            .expect("create settings");
    }

    #[sqlx::test]
    async fn context_window_legge_il_max(pool: PgPool) {
        create_schema(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, context_window) VALUES \
             ('a', 'm', 100000), ('b', 'm', 200000)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let port = CatalogModelUpscalePort::new(pool.clone());
        let w = port.context_window("m").await.expect("ok");
        assert_eq!(w, 200000, "prende il MAX context_window dichiarato");
    }

    #[sqlx::test]
    async fn context_window_zero_se_ignoto(pool: PgPool) {
        create_schema(&pool).await;
        let port = CatalogModelUpscalePort::new(pool.clone());
        assert_eq!(port.context_window("inesistente").await.expect("ok"), 0);
    }

    #[sqlx::test]
    async fn select_upscale_sceglie_window_piu_grande_nel_tier(pool: PgPool) {
        create_schema(&pool).await;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('agent.upscale.target_tier', 'heavy')",
        )
        .execute(&pool)
        .await
        .expect("set tier");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, context_window) VALUES \
             ('openai', 'piccolo', true, 'none', 'heavy', 120000), \
             ('anthropic', 'grande', true, 'none', 'heavy', 400000), \
             ('mistral', 'small', true, 'none', 'medium', 1000000)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let port = CatalogModelUpscalePort::new(pool.clone());
        let pick = port
            .select_upscale_model("corrente", 200000)
            .await
            .expect("ok")
            .expect("candidato heavy con window >= 200k");
        // 'small' (1M) e' medium -> escluso dal tier heavy; vince 'grande' (400k).
        assert_eq!(pick.model, "grande");
        assert_eq!(pick.provider, "anthropic");
        assert!(pick.reason.contains("required=200000"));
        // FIX-A: il tier e' esposto come CAMPO strutturato (regola M), pari al
        // target_tier configurato ('heavy'), non ricavato dal parsing di `reason`.
        assert_eq!(pick.tier, "heavy");
    }

    #[sqlx::test]
    async fn select_upscale_none_se_migliore_e_il_corrente(pool: PgPool) {
        create_schema(&pool).await;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('agent.upscale.target_tier', 'heavy')",
        )
        .execute(&pool)
        .await
        .expect("set tier");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, context_window) VALUES \
             ('anthropic', 'grande', true, 'none', 'heavy', 400000)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let port = CatalogModelUpscalePort::new(pool.clone());
        // Il migliore coincide col corrente -> nessun upscale.
        let pick = port
            .select_upscale_model("grande", 100000)
            .await
            .expect("ok");
        assert!(pick.is_none());
    }

    #[sqlx::test]
    async fn select_upscale_esclude_thinking_exclude(pool: PgPool) {
        create_schema(&pool).await;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('agent.upscale.target_tier', 'heavy')",
        )
        .execute(&pool)
        .await
        .expect("set tier");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, context_window) VALUES \
             ('x', 'reasoner', true, 'exclude', 'heavy', 999999)",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let port = CatalogModelUpscalePort::new(pool.clone());
        // L'unico candidato e' policy=exclude -> nessun upscale (fail-open None).
        assert!(port
            .select_upscale_model("corrente", 100000)
            .await
            .expect("ok")
            .is_none());
    }
}
