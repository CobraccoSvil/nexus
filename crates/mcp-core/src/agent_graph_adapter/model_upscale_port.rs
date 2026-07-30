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
//! `Ok(0)` / `Ok(None)` su guasto, mai `PortError`. SOLA LETTURA.

use async_trait::async_trait;
use sqlx::PgPool;

use nexus_agent_graph::runtime::ports::{ModelUpscalePort, PortError, UpscalePick};


/// Adapter [`ModelUpscalePort`] -> `ai_price_catalog` + settings `agent.upscale.*`.
pub struct CatalogModelUpscalePort {
    /// Pool Postgres su cui girano i lookup catalog/settings dell'upscale.
    db: PgPool,
    /// Il fornitore a cui l'utente ha vincolato il run, se l'ha fatto. Vuoto per
    /// ogni run non vincolato: la porta si comporta come prima.
    pin: crate::orchestrator::ProviderPin,
}

impl CatalogModelUpscalePort {
    /// Costruisce l'adapter sul pool Postgres condiviso, senza vincolo.
    pub fn new(db: PgPool) -> Self {
        Self {
            db,
            pin: crate::orchestrator::ProviderPin::none(),
        }
    }

    /// Lega la porta al fornitore scelto dall'utente per questo run.
    ///
    /// Anche l'upscale cambia fornitore: sale di tier per guadagnare finestra e
    /// il modello piu' capiente puo' essere di un altro. E' un cambio meno
    /// visibile del failover — nessun errore, nessun avviso — e quindi il piu'
    /// facile da dimenticare quando si applica un vincolo.
    pub fn con_vincolo(mut self, pin: crate::orchestrator::ProviderPin) -> Self {
        self.pin = pin;
        self
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
        let mut req = crate::orchestrator::model_service::ModelRequest::agentic(&tier)
            .tier_policy(crate::orchestrator::model_service::TierPolicy::Exact {
                why: crate::orchestrator::model_service::ExactReason::ScaleTarget,
            })
            .rank(crate::orchestrator::model_service::Rank::WidestWindow)
            .min_context_window(required_tokens);
        // VINCOLO DEL RUN: restringe la ricerca al fornitore scelto, riusando il
        // pin che il servizio di selezione ha gia' (regola L) invece di
        // aggiungere qui una seconda nozione di "solo questo provider". Se
        // dentro il vincolo non c'e' un modello abbastanza capiente, il
        // fail-open sotto lascia il run sul modello corrente: e' un upscale
        // mancato, non un run fermo.
        if let Some(pinned) = self.pin.provider() {
            req = req.pinned(pinned);
        }
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
    /// Fail-open gia' incorporato: `select_agentic_model` ritorna `None` su
    /// guasto/nessun candidato (nessun panico), che qui e' `Ok(None)` -> il
    /// chiamante ANNULLA il cambio-tier (fail-safe, mantiene il modello corrente).
    async fn select_model_for_tier(
        &self,
        tier: &str,
        min_context_window: i64,
        capability: Option<&str>,
        exclude_providers: &[String],
    ) -> Result<Option<(String, String)>, PortError> {
        // SERVIZIO UNICO (regola L). `Exact{ScaleTarget}`: il tier arriva GIA'
        // deciso dal modulo puro dello scale-controller — questa funzione lo
        // ESEGUE, non lo negozia. Degradare qui darebbe al chiamante un modello
        // di un tier diverso da quello che ha chiesto, in silenzio.
        // `CostFirst`: a tier fissato il piu' economico che soddisfa i vincoli
        // (context_window incluso), is_featured solo come tie-break.
        let mut req = crate::orchestrator::model_service::ModelRequest::agentic(tier)
            .tier_policy(crate::orchestrator::model_service::TierPolicy::Exact {
                why: crate::orchestrator::model_service::ExactReason::ScaleTarget,
            })
            .capability(capability)
            .min_context_window(min_context_window)
            .exclude(exclude_providers);
        // Come sopra: anche il cambio di tier bidirezionale resta dentro il
        // vincolo. Su fail-open il chiamante annulla il cambio-tier e tiene il
        // modello corrente, che e' del fornitore giusto.
        if let Some(pinned) = self.pin.provider() {
            req = req.pinned(pinned);
        }
        let picked = crate::orchestrator::model_service::select_model(&self.db, &req)
            .await
            .ok()
            .map(|c| (c.provider, c.model));
        Ok(picked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Schema REALE (regola O): `ai_price_catalog` e `settings` arrivano dalla
    /// migrazione, gia' popolate dai dati di produzione. Il DELETE isola il test
    /// dal loro rumore senza sostituire lo schema (colonne/CHECK/FK veri).
    async fn create_schema(pool: &PgPool) {
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(pool)
            .await
            .expect("pulizia catalog");
    }

    /// Imposta un settings applicativo sovrascrivendo l'eventuale riga reale
    /// seminata dalla migrazione (regola O: stesso schema, chiavi deterministiche
    /// per il test).
    async fn set_setting(pool: &PgPool, key: &str, value: &str) {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ($1, $2) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .expect("set setting");
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn context_window_legge_il_max(pool: PgPool) {
        create_schema(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, context_window, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('a', 'm', 100000, 1.0, 1.0, 'USD', now()), ('b', 'm', 200000, 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let port = CatalogModelUpscalePort::new(pool.clone());
        let w = port.context_window("m").await.expect("ok");
        assert_eq!(w, 200000, "prende il MAX context_window dichiarato");
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn context_window_zero_se_ignoto(pool: PgPool) {
        create_schema(&pool).await;
        let port = CatalogModelUpscalePort::new(pool.clone());
        assert_eq!(port.context_window("inesistente").await.expect("ok"), 0);
    }

    /// `select_model` (dietro `select_upscale_model`) legge `qualification_gate`
    /// dai `settings` VERI: su META_MIGRATOR il gate e' acceso di default (mig
    /// 0595), quindi ogni riga seminata deve dichiararsi 'qualified' o il pool
    /// risulta vuoto — a differenza dello specchio a mano, dove `settings` era
    /// vuota e il gate restava spento senza che il test se ne accorgesse.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn select_upscale_sceglie_window_piu_grande_nel_tier(pool: PgPool) {
        create_schema(&pool).await;
        set_setting(&pool, "agent.upscale.target_tier", "heavy").await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, \
              context_window, qualification_state, qualification_expires_at, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('openai', 'piccolo', true, 'none', 'heavy', 120000, 'qualified', now() + interval '30 days', 1.0, 1.0, 'USD', now()), \
             ('anthropic', 'grande', true, 'none', 'heavy', 400000, 'qualified', now() + interval '30 days', 1.0, 1.0, 'USD', now()), \
             ('mistral', 'small', true, 'none', 'medium', 1000000, 'qualified', now() + interval '30 days', 1.0, 1.0, 'USD', now())",
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

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn select_upscale_none_se_migliore_e_il_corrente(pool: PgPool) {
        create_schema(&pool).await;
        set_setting(&pool, "agent.upscale.target_tier", "heavy").await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, \
              context_window, qualification_state, qualification_expires_at, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('anthropic', 'grande', true, 'none', 'heavy', 400000, 'qualified', now() + interval '30 days', 1.0, 1.0, 'USD', now())",
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

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn select_upscale_esclude_thinking_exclude(pool: PgPool) {
        create_schema(&pool).await;
        set_setting(&pool, "agent.upscale.target_tier", "heavy").await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, \
              context_window, qualification_state, qualification_expires_at, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('x', 'reasoner', true, 'exclude', 'heavy', 999999, 'qualified', now() + interval '30 days', 1.0, 1.0, 'USD', now())",
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

    // ---- vincolo di provider del run ("Forza" nel composer) ----

    /// Il vincolo come nasce in produzione: dal punto unico
    /// [`crate::orchestrator::ProviderChoice::resolve`], mai costruito a mano
    /// (un vincolo coniato fuori di li' e' un vincolo che nessun utente ha
    /// dato; lo vieta anche il guard `nascita del pin duro`).
    fn vincolo_utente(provider: &str) -> crate::orchestrator::ProviderPin {
        crate::orchestrator::ProviderPin::from_choice(
            &crate::orchestrator::ProviderChoice::resolve(
                Some(provider),
                crate::orchestrator::ProviderOverrideMode::Pinned,
                None,
            ),
        )
    }

    /// Scena dei due test seguenti: nel tier di destinazione il modello con la
    /// finestra piu' grande e' di un fornitore, e ce n'e' un altro — piu' piccolo
    /// ma sufficiente — del fornitore vincolato. Senza vincolo vince il primo.
    async fn scena_due_fornitori_nel_tier(pool: &PgPool) {
        create_schema(pool).await;
        set_setting(pool, "agent.upscale.target_tier", "heavy").await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, \
              context_window, qualification_state, qualification_expires_at, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('openai', 'gpt-enorme', true, 'none', 'heavy', 900000, 'qualified', now() + interval '30 days', 1.0, 1.0, 'USD', now()), \
             ('anthropic', 'claude-ampio', true, 'none', 'heavy', 400000, 'qualified', now() + interval '30 days', 1.0, 1.0, 'USD', now())",
        )
        .execute(pool)
        .await
        .expect("insert");
    }

    /// PREMESSA: senza vincolo l'upscale esce dal fornitore e prende la finestra
    /// piu' grande. Senza questo, il test seguente potrebbe passare perche' il
    /// candidato dell'altro fornitore non c'era.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn senza_vincolo_l_upscale_prende_la_finestra_piu_grande(pool: PgPool) {
        scena_due_fornitori_nel_tier(&pool).await;
        let port = CatalogModelUpscalePort::new(pool.clone());
        let pick = port
            .select_upscale_model("corrente", 300000)
            .await
            .expect("ok")
            .expect("un heavy abbastanza capiente esiste");
        assert_eq!(pick.provider, "openai");
        assert_eq!(pick.model, "gpt-enorme");
    }

    /// L'upscale e' l'altro modo di cambiare fornitore in corsa, e il piu'
    /// silenzioso: nessun errore, nessun avviso, solo una finestra piu' larga.
    /// Col run vincolato resta dentro il fornitore scelto, anche se fuori c'e' di
    /// meglio.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn vincolo_tiene_l_upscale_dentro_il_fornitore_scelto(pool: PgPool) {
        scena_due_fornitori_nel_tier(&pool).await;
        let port =
            CatalogModelUpscalePort::new(pool.clone()).con_vincolo(vincolo_utente("anthropic"));
        let pick = port
            .select_upscale_model("corrente", 300000)
            .await
            .expect("ok")
            .expect("dentro il vincolo c'e' un heavy che regge la finestra richiesta");
        assert_eq!(
            pick.provider, "anthropic",
            "openai ha la finestra piu' grande e vincerebbe: a escluderlo e' solo il vincolo"
        );
        assert_eq!(pick.model, "claude-ampio");
    }
}
