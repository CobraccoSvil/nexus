//! Adapter del trait [`nexus_agent_graph::runtime::ports::MetaReasonerPort`].
//!
//! IMPLEMENTA `MetaReasonerPort::recover` — il meta-reasoner LLM di
//! recovery-da-stallo (piano meta-reasoner, blocco #7) — copiando 1:1 il
//! PARADIGMA ADR 0036 di [`crate::verify_profile::infer_call`] (regola L: stesso
//! flusso, non re-inventato):
//!   1. GATE REPLAY: in [`ExecMode::Replay`] ritorna `Ok(None)` SENZA chiamare
//!      l'LLM (vedi nota sotto).
//!   2. kill-switch `agent.stall_recovery.enabled` (opt-in, default OFF, regola G);
//!   3. risoluzione del modello via
//!      [`crate::internal_routing::resolve_purpose_model_db`] (purpose
//!      `stall_recovery`, regola G — nessun nome modello hardcoded qui);
//!   4. caricamento del template `system.stall_recovery.decide` via
//!      [`crate::prompt_templates::TemplateCache`] (punto unico loader, regola L);
//!   5. chiamata LLM one-shot con timeout clamp + `max_tokens` basso, passando
//!      SOLO lo [`StallContext`] serializzato in JSON (non l'intera history:
//!      budget/costo);
//!   6. parse con [`nexus_types::llm_json::extract_json_block`] +
//!      validazione col PUNTO UNICO
//!      [`nexus_agent_graph::decisions::meta_reason::validate_move`] (enum CHIUSO
//!      `RecoveryMove`, blocker ADR 0034; malformato -> `Fallback`).
//!
//! ## GATE REPLAY (critico, dalla scoperta verificata sul codice)
//!
//! [`crate::agent_graph_adapter::llm_gateway::ReplayLlmGateway`] rigioca SOLO le
//! completion con `purpose=="executor"`; ogni altra completion (planner,
//! reflection, e questo `stall_recovery`) ottiene una risposta ausiliaria neutra
//! VUOTA. Quindi consultare l'LLM in Replay per `stall_recovery` non produrrebbe
//! nulla di utile. Il modello corretto di replay del recovery e':
//!   - **Real** (primario): consulta l'LLM, la mossa e' persistita in
//!     `extra["stall_move::…"]` (checkpoint) dal nodo `StallRecovery`.
//!   - **Resume Rust->Rust**: il checkpoint contiene gia' la mossa -> il nodo fa
//!     cache-hit dall'`extra` (0 LLM). Deterministico.
//!   - **Shadow Rust<->Python**: il Python non ha il reasoner; qui in Replay la
//!     porta ritorna `Ok(None)` -> il nodo degrada a `Fallback` -> gerarchia fissa
//!     `progress_controller::decide` -> **matcha il Python** (parita' shadow).
//! Per questo `recover` gatta su `mode`: `Replay` -> `Ok(None)` immediato, SENZA
//! toccare DB/gateway.
//!
//! ## NotFound vs DB-down (regola G — mai un OFF mascherante)
//!
//! [`crate::internal_routing::PurposeResolution`] distingue le cause:
//!   - `NotFound` (purpose `stall_recovery` assente / privo di tier): degrado
//!     LEGITTIMO -> `Ok(None)`. Se il flag e' ON, e' un MISCONFIG (log ERROR +
//!     metrica), non un OFF silenzioso: la migrazione 0510 non e' stata applicata.
//!   - `MatrixUnavailable` (DB down): guasto infrastrutturale ->
//!     `Err(PortError::ProviderUnavailable)` (MAI `Ok(None)` mascherante).
//!   - `NoCapableModel` (tier senza modello disponibile / tutti in cooldown):
//!     provider non risolto -> `Err(PortError::ProviderUnavailable)` (rete di
//!     sicurezza a valle: il nodo degrada comunque alla gerarchia fissa).
//!
//! ## Degrado safe
//!
//! Su qualunque guasto NON infrastrutturale del percorso LLM (template assente
//! benche' enabled+purpose presenti, timeout, risposta vuota/malformata) l'impl
//! ritorna `Ok(None)`: il nodo tratta `None` e `Fallback` in modo IDENTICO (usa
//! l'euristica `pc::decide`). Non si abortisce il run: il recovery e' best-effort.
//!
//! Regola F: niente prompt/response in chiaro nei log (solo assi/contatori/esiti).
//!
//! ## Secondo metodo `orchestrate` (impl LLM, #11c)
//!
//! [`MetaReasonerPort`] espone anche `orchestrate` (decisione di plan-phase /
//! decompose / delega su [`OrchestrationContext`] -> [`OrchestrationMove`]),
//! implementato in [`PgMetaReasonerPort::consult_orch_llm`] col PARADIGMA
//! IDENTICO a `consult_llm` (regola L, stesso flusso, scope disgiunto):
//!   1. GATE REPLAY opzione A: in [`ExecMode::Replay`] -> `Ok(None)` immediato,
//!      senza I/O (il `ReplayLlmGateway` rigioca solo `purpose=="executor"`);
//!   2. kill-switch `agent.orchestration.enabled` (opt-in, default OFF, regola G):
//!      OFF -> `Ok(None)` -> il gate ricade su `is_eligible`/`should_parallelize`
//!      -> BIT-IDENTICO a oggi (vincolo primario);
//!   3. purpose `orchestration_decide` via `resolve_purpose_model_db` (regola G,
//!      STESSA distinzione NotFound/NoCapableModel/MatrixUnavailable di recover);
//!   4. template `system.orchestration.decide` via `TemplateCache`;
//!   5. chiamata LLM one-shot passando SOLO l'[`OrchestrationContext`] serializzato;
//!   6. parse con `extract_json_block` + validazione col PUNTO UNICO
//!      [`nexus_agent_graph::decisions::orchestration_reason::validate_orch_move`]
//!      (`isolation_available=false` in Fase 1 -> `ParallelIsolated` rifiutata;
//!      `delegation_forbidden` dal ctx). Malformato -> `Fallback` (il gate lo
//!      tratta come `None`). Vedi mig 0512.

use async_trait::async_trait;
use sqlx::PgPool;

use nexus_agent_graph::decisions::meta_reason::validate_move;
use nexus_agent_graph::decisions::orchestration_reason::validate_orch_move;
use nexus_agent_graph::decisions::scale_reason::validate_scale_move;
use nexus_agent_graph::runtime::ports::{
    ExecMode, MetaReasonerPort, OrchestrationContext, OrchestrationMove, PortError, RecoveryMove,
    ScaleContext, ScaleMove, StallContext,
};

use crate::internal_routing::{resolve_purpose_model_db, PurposeResolution};
use crate::orchestrator::NeuralCoreClient;

/// Purpose (regola G) del meta-reasoner: risolve `(provider, model)` tier-aware
/// da `nexus_purpose_model` (mig 0510). Unica fonte DB, nessun nome modello qui.
const STALL_PURPOSE: &str = "stall_recovery";

/// Chiave del template di decisione (mig 0510, schema XML fuori-chat regola D).
const STALL_TEMPLATE_KEY: &str = "system.stall_recovery.decide";

/// Setting kill-switch (opt-in, default OFF): con `false` la porta e' inerte
/// (`Ok(None)`) -> il nodo ricade sulla gerarchia fissa (comportamento storico).
const STALL_ENABLED_SETTING: &str = "agent.stall_recovery.enabled";

/// Setting del timeout (s) della chiamata LLM. Clamp `[5, 300]` lato codice.
const STALL_TIMEOUT_SETTING: &str = "agent.stall_recovery.timeout_s";

/// Timeout di default (s) se il setting manca / e' malformato. Non e' un magic
/// fallback su un comportamento di business: e' il safe-default numerico gia'
/// seminato dalla mig 0510 (stesso pattern di `verify_profile`).
const STALL_TIMEOUT_DEFAULT: u64 = 20;

/// Tetto di token in output: la decisione e' un piccolo oggetto JSON (una mossa +
/// nudge breve). Basso di proposito (budget/costo: il reasoner gira sui run
/// stallati, non deve esplodere in token).
const STALL_MAX_TOKENS: u32 = 512;

/// Purpose (regola G) del meta-reasoner di ORCHESTRAZIONE: risolve `(provider,
/// model)` tier-aware da `nexus_purpose_model` (mig 0512). Unica fonte DB, nessun
/// nome modello qui. Gemello di [`STALL_PURPOSE`] su scope disgiunto (regola L).
const ORCH_PURPOSE: &str = "orchestration_decide";

/// Chiave del template di decisione dell'orchestrazione (mig 0512, schema XML
/// fuori-chat regola D). Gemello di [`STALL_TEMPLATE_KEY`].
const ORCH_TEMPLATE_KEY: &str = "system.orchestration.decide";

/// Setting kill-switch (opt-in, default OFF): con `false` la porta e' inerte per
/// l'orchestrazione (`Ok(None)`) -> il gate ricade sull'euristica esistente
/// (`is_eligible`/`should_parallelize`) -> BIT-IDENTICO a oggi (vincolo primario).
const ORCH_ENABLED_SETTING: &str = "agent.orchestration.enabled";

/// Setting del timeout (s) della chiamata LLM di orchestrazione. Clamp `[5, 300]`.
const ORCH_TIMEOUT_SETTING: &str = "agent.orchestration.timeout_s";

/// Timeout di default (s) dell'orchestrazione se il setting manca / e' malformato.
/// Safe-default numerico seminato dalla mig 0512 (stesso pattern di stall/verify).
const ORCH_TIMEOUT_DEFAULT: u64 = 20;

/// Tetto di token in output della decisione di orchestrazione: la mossa e' un
/// piccolo oggetto JSON (enum + eventuali blocchi/task brevi). Basso di proposito
/// (budget/costo: la decisione e' all'ingresso del run, non deve esplodere).
const ORCH_MAX_TOKENS: u32 = 512;

/// Purpose (regola G) dello SCALE-CONTROLLER: risolve `(provider, model)`
/// tier-aware da `nexus_purpose_model` (mig 0516). Unica fonte DB, nessun nome
/// modello qui. Terzo scope disgiunto (regola L) su [`STALL_PURPOSE`]/[`ORCH_PURPOSE`].
const SCALE_PURPOSE: &str = "scale_assess";

/// Chiave del template dello scale-controller (mig 0516, schema XML fuori-chat
/// regola D). Gemello di [`STALL_TEMPLATE_KEY`]/[`ORCH_TEMPLATE_KEY`].
const SCALE_TEMPLATE_KEY: &str = "system.scale.assess";

/// Setting kill-switch (opt-in, default OFF): con `false` la porta e' inerte per la
/// scala (`Ok(None)`). PR-A: nessun nodo/detector la chiama comunque -> bit-identico.
const SCALE_ENABLED_SETTING: &str = "agent.scale.enabled";

/// Chiave del template ADDENDUM del SIZING (mig 0522): descrive la mossa
/// `adjust_sizing` (postura Compact/Relax/Hold). APPESO al template base SOLO quando
/// `agent.scale.sizing_enabled=true`, cosi' con sizing OFF il prompt LLM resta
/// BYTE-IDENTICO al pre-0522 (bit-identico per il flusso tier).
const SCALE_SIZING_TEMPLATE_KEY: &str = "system.scale.assess.sizing";

/// Setting kill-switch NESTED del sizing (opt-in, default OFF, regola G): con `false`
/// l'addendum sizing NON e' appeso e il gate lato motore degrada ogni AdjustSizing a
/// KeepTier.
const SCALE_SIZING_ENABLED_SETTING: &str = "agent.scale.sizing_enabled";

/// Setting del timeout (s) della chiamata LLM di scala. Clamp `[5, 300]`.
const SCALE_TIMEOUT_SETTING: &str = "agent.scale.timeout_s";

/// Timeout di default (s) della scala se il setting manca / e' malformato.
/// Safe-default numerico (stesso pattern di stall/orch/verify). Basso: la decisione
/// e' pre-crisi, non deve bloccare il run.
const SCALE_TIMEOUT_DEFAULT: u64 = 15;

/// Tetto di token in output della decisione di scala: la mossa e' un piccolo
/// oggetto JSON (`{"move","tier","confidence"}`). Basso di proposito (budget/costo:
/// il controller gira di frequente, non deve esplodere in token).
const SCALE_MAX_TOKENS: u32 = 256;

/// Adapter [`MetaReasonerPort`] -> LLM via [`NeuralCoreClient`] (paradigma
/// ADR 0036 di `verify_profile`). Legge config/purpose/template dal `db`.
/// `recover` (recovery-da-stallo) e' implementato; `orchestrate` e' STUB (#11c).
pub struct PgMetaReasonerPort {
    /// Pool Postgres: kill-switch (`settings`), risoluzione purpose
    /// (`nexus_purpose_model`) e caricamento template (`nexus_prompt_templates`).
    db: PgPool,
    /// Client LLM one-shot (zero-sized: risolve il Nexus LLM Gateway dal bridge
    /// globale, porta da `settings`, regola G). Stesso client di `verify_profile`.
    neural: NeuralCoreClient,
}

impl PgMetaReasonerPort {
    /// Costruisce l'adapter sul pool Postgres condiviso e sul client LLM.
    pub fn new(db: PgPool, neural: NeuralCoreClient) -> Self {
        Self { db, neural }
    }

    /// `true` se il kill-switch e' attivo (`agent.stall_recovery.enabled`).
    /// Default OFF (opt-in): setting assente / malformato -> `false` (regola G:
    /// unica fonte DB, nessun fallback che accenda una feature).
    async fn enabled(&self) -> bool {
        nexus_auth::get_setting(&self.db, STALL_ENABLED_SETTING)
            .await
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    /// Timeout (s) clampato in `[5, 300]`. Setting assente / malformato ->
    /// [`STALL_TIMEOUT_DEFAULT`] (safe-default numerico seminato dalla mig 0510).
    async fn timeout_s(&self) -> u64 {
        nexus_auth::get_setting(&self.db, STALL_TIMEOUT_SETTING)
            .await
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(STALL_TIMEOUT_DEFAULT)
            .clamp(5, 300)
    }

    /// Consulta l'LLM (Real): risolve modello + template, chiama il gateway,
    /// parsa e valida. Ritorna `Ok(Some(move))` (validata; `Fallback` incluso e'
    /// legittimo, il nodo lo tratta come "usa euristica") oppure `Ok(None)`
    /// (degrado safe: kill-switch OFF, purpose NotFound, template assente,
    /// timeout, risposta vuota/malformata), oppure `Err` solo su guasto
    /// INFRASTRUTTURALE (DB down, provider non risolto per cooldown).
    async fn consult_llm(&self, ctx: &StallContext) -> Result<Option<RecoveryMove>, PortError> {
        // (1) Kill-switch: opt-in, default OFF -> inerte (regola G).
        if !self.enabled().await {
            return Ok(None);
        }

        // (2) Modello dal purpose (regola G). Distinzione delle cause (regola G,
        // niente OFF mascherante):
        //   - Resolved     -> procedi;
        //   - NotFound      -> misconfig (flag ON ma mig 0510 non applicata):
        //                      log ERROR + metrica, poi degrado Ok(None);
        //   - NoCapableModel-> provider non risolto (cooldown) -> Err (infra);
        //   - MatrixUnavail.-> DB down -> Err (infra), MAI Ok(None) mascherante.
        let (provider, model) = match resolve_purpose_model_db(&self.db, STALL_PURPOSE).await {
            PurposeResolution::Resolved {
                provider, model, ..
            } => (provider, model),
            PurposeResolution::NotFound => {
                // Flag ON ma purpose assente: la config e' incompleta (mig 0510).
                // NON un OFF silenzioso invisibile: ERROR + metrica misconfig.
                tracing::error!(
                    target: "nexus_stall_recovery",
                    purpose = STALL_PURPOSE,
                    metric = "stall_recovery_misconfig",
                    "stall_recovery: flag ON ma purpose '{}' assente in nexus_purpose_model \
                     (applicare la migrazione 0510); degrado alla gerarchia fissa",
                    STALL_PURPOSE
                );
                return Ok(None);
            }
            PurposeResolution::NoCapableModel { tier } => {
                // Tier senza modello disponibile (capability mancante / tutti in
                // cooldown): guasto di risoluzione provider -> Err (il nodo degrada
                // comunque alla gerarchia fissa, ma non maschera il guasto).
                return Err(PortError::ProviderUnavailable(format!(
                    "stall_recovery: nessun modello del tier '{tier}' per purpose '{STALL_PURPOSE}'"
                )));
            }
            PurposeResolution::MatrixUnavailable(e) => {
                // DB down / routing non disponibile: guasto INFRASTRUTTURALE.
                // MAI Ok(None) (regola G): il chiamante lo tratta come best-effort.
                return Err(PortError::ProviderUnavailable(format!(
                    "stall_recovery: routing non disponibile: {e}"
                )));
            }
        };

        // (3) Template (punto unico loader, regola L). Cache monouso: la
        // consultazione e' rara (solo su stallo), il punto unico resta rispettato.
        let tpl_cache = crate::prompt_templates::TemplateCache::new();
        let system_text = crate::prompt_templates::get_template_or_default(
            &self.db,
            &tpl_cache,
            STALL_TEMPLATE_KEY,
        )
        .await;
        if system_text.trim().is_empty() {
            // Enabled + purpose risolto MA template assente: misconfig parziale.
            // ERROR (non WARN) + degrado safe (Ok(None)): il nodo usa l'euristica.
            tracing::error!(
                target: "nexus_stall_recovery",
                key = STALL_TEMPLATE_KEY,
                metric = "stall_recovery_misconfig",
                "stall_recovery: template '{}' assente/vuoto (applicare la migrazione 0510); \
                 degrado alla gerarchia fissa",
                STALL_TEMPLATE_KEY
            );
            return Ok(None);
        }

        // (4) Payload: SOLO lo StallContext serializzato (regola M: segnali
        // strutturati, non l'intera history -> budget/costo). Un unico turno user.
        let ctx_json = match serde_json::to_string(ctx) {
            Ok(j) => j,
            Err(err) => {
                tracing::warn!(
                    target: "nexus_stall_recovery",
                    error = %err,
                    "stall_recovery: serializzazione StallContext fallita, degrado"
                );
                return Ok(None);
            }
        };
        let user_text = format!("Stato di stallo strutturato (JSON):\n{ctx_json}");
        let messages =
            serde_json::json!([{ "role": "user", "content": user_text }]).to_string();
        let timeout_s = self.timeout_s().await;

        // (5) Chiamata LLM one-shot col paradigma verify_profile (generate_agent_turn
        // -> pin_provider provider gia' risolto, regola G; nessun tool). Timeout +
        // degrado safe: ogni esito non utile -> Ok(None), il nodo usa l'euristica.
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_s),
            self.neural.generate_agent_turn(
                &provider,
                &model,
                &messages,
                "[]",
                STALL_MAX_TOKENS,
                &system_text,
            ),
        )
        .await;
        let value = match resp {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "nexus_stall_recovery",
                    error = %e,
                    "stall_recovery: chiamata LLM fallita, degrado alla gerarchia fissa"
                );
                return Ok(None);
            }
            Err(_) => {
                tracing::warn!(
                    target: "nexus_stall_recovery",
                    timeout_s,
                    "stall_recovery: chiamata LLM oltre il timeout, degrado alla gerarchia fissa"
                );
                return Ok(None);
            }
        };

        // (6) Estrai il testo (forma neural_service: `content`/`text`) e parsa il
        // blocco JSON col punto unico; valida con meta_reason::validate_move (enum
        // CHIUSO + blocker ADR 0034). Testo assente / JSON assente -> Fallback.
        let text = value
            .get("content")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("text").and_then(|v| v.as_str()))
            .unwrap_or("");
        let parsed = match nexus_types::llm_json::extract_json_block(text) {
            Some(v) => v,
            None => {
                tracing::debug!(
                    target: "nexus_stall_recovery",
                    axis = %ctx.axis,
                    "stall_recovery: risposta senza JSON parsabile, degrado (Fallback)"
                );
                return Ok(Some(RecoveryMove::Fallback));
            }
        };
        // validate_move: qualunque forma malformata / enum sconosciuto / campo
        // vuoto / blocker fuori vocabolario -> RecoveryMove::Fallback (il nodo lo
        // tratta come "usa euristica"). Il nodo ri-valida (idempotente, robustezza).
        let mv = validate_move(&parsed);
        tracing::info!(
            target: "nexus_stall_recovery",
            axis = %ctx.axis,
            work_epoch = ctx.work_epoch,
            "stall_recovery: mossa decisa dal meta-reasoner"
        );
        Ok(Some(mv))
    }

    /// `true` se il kill-switch di orchestrazione e' attivo
    /// (`agent.orchestration.enabled`). Default OFF (opt-in): setting assente /
    /// malformato -> `false` (regola G: nessun fallback che accenda una feature).
    /// Con `false` la porta e' inerte -> gate ricade su `is_eligible` -> oggi.
    async fn orch_enabled(&self) -> bool {
        nexus_auth::get_setting(&self.db, ORCH_ENABLED_SETTING)
            .await
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    /// Timeout (s) dell'orchestrazione clampato in `[5, 300]`. Setting assente /
    /// malformato -> [`ORCH_TIMEOUT_DEFAULT`] (safe-default numerico, mig 0512).
    async fn orch_timeout_s(&self) -> u64 {
        nexus_auth::get_setting(&self.db, ORCH_TIMEOUT_SETTING)
            .await
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(ORCH_TIMEOUT_DEFAULT)
            .clamp(5, 300)
    }

    /// Consulta l'LLM (Real) per la decisione di ORCHESTRAZIONE: risolve modello +
    /// template, chiama il gateway, parsa e valida col PUNTO UNICO
    /// [`validate_orch_move`]. Gemello di [`Self::consult_llm`] su scope disgiunto
    /// (regola L: STESSO flusso, non re-inventato). Ritorna `Ok(Some(move))`
    /// (validata; `Fallback` incluso e' legittimo, il gate lo tratta come "usa
    /// euristica") oppure `Ok(None)` (degrado safe: kill-switch OFF, purpose
    /// NotFound, template assente, timeout, risposta vuota/malformata), oppure
    /// `Err` solo su guasto INFRASTRUTTURALE (DB down, provider non risolto per
    /// cooldown). In Fase 1 `isolation_available` e' SEMPRE `false`
    /// ([`ORCH_ISOLATION_AVAILABLE`]) -> `ParallelIsolated` e' sempre rifiutata.
    async fn consult_orch_llm(
        &self,
        ctx: &OrchestrationContext,
    ) -> Result<Option<OrchestrationMove>, PortError> {
        // (1) Kill-switch: opt-in, default OFF -> inerte (regola G). Vincolo
        // primario: OFF => Ok(None) => gate su is_eligible => bit-identico a oggi.
        if !self.orch_enabled().await {
            return Ok(None);
        }

        // (2) Modello dal purpose (regola G). STESSA distinzione delle cause di
        // consult_llm (niente OFF mascherante):
        //   - Resolved     -> procedi;
        //   - NotFound      -> misconfig (flag ON ma mig 0512 non applicata):
        //                      log ERROR + metrica, poi degrado Ok(None);
        //   - NoCapableModel-> provider non risolto (cooldown) -> Err (infra);
        //   - MatrixUnavail.-> DB down -> Err (infra), MAI Ok(None) mascherante.
        let (provider, model) = match resolve_purpose_model_db(&self.db, ORCH_PURPOSE).await {
            PurposeResolution::Resolved {
                provider, model, ..
            } => (provider, model),
            PurposeResolution::NotFound => {
                tracing::error!(
                    target: "nexus_orchestration",
                    purpose = ORCH_PURPOSE,
                    metric = "orchestration_misconfig",
                    "orchestration: flag ON ma purpose '{}' assente in nexus_purpose_model \
                     (applicare la migrazione 0512); degrado all'euristica esistente",
                    ORCH_PURPOSE
                );
                return Ok(None);
            }
            PurposeResolution::NoCapableModel { tier } => {
                return Err(PortError::ProviderUnavailable(format!(
                    "orchestration: nessun modello del tier '{tier}' per purpose '{ORCH_PURPOSE}'"
                )));
            }
            PurposeResolution::MatrixUnavailable(e) => {
                return Err(PortError::ProviderUnavailable(format!(
                    "orchestration: routing non disponibile: {e}"
                )));
            }
        };

        // (3) Template (punto unico loader, regola L). Cache monouso: la
        // consultazione e' rara (una volta per run all'ingresso).
        let tpl_cache = crate::prompt_templates::TemplateCache::new();
        let system_text = crate::prompt_templates::get_template_or_default(
            &self.db,
            &tpl_cache,
            ORCH_TEMPLATE_KEY,
        )
        .await;
        if system_text.trim().is_empty() {
            tracing::error!(
                target: "nexus_orchestration",
                key = ORCH_TEMPLATE_KEY,
                metric = "orchestration_misconfig",
                "orchestration: template '{}' assente/vuoto (applicare la migrazione 0512); \
                 degrado all'euristica esistente",
                ORCH_TEMPLATE_KEY
            );
            return Ok(None);
        }

        // (4) Payload: SOLO l'OrchestrationContext serializzato (regola M: segnali
        // strutturati, non l'intera history -> budget/costo). Un unico turno user.
        let ctx_json = match serde_json::to_string(ctx) {
            Ok(j) => j,
            Err(err) => {
                tracing::warn!(
                    target: "nexus_orchestration",
                    error = %err,
                    "orchestration: serializzazione OrchestrationContext fallita, degrado"
                );
                return Ok(None);
            }
        };
        let user_text = format!("Contesto di orchestrazione strutturato (JSON):\n{ctx_json}");
        let messages =
            serde_json::json!([{ "role": "user", "content": user_text }]).to_string();
        let timeout_s = self.orch_timeout_s().await;

        // (5) Chiamata LLM one-shot col paradigma verify_profile/recover
        // (generate_agent_turn -> pin_provider gia' risolto, regola G; nessun tool).
        // Timeout + degrado safe: ogni esito non utile -> Ok(None) (gate: euristica).
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_s),
            self.neural.generate_agent_turn(
                &provider,
                &model,
                &messages,
                "[]",
                ORCH_MAX_TOKENS,
                &system_text,
            ),
        )
        .await;
        let value = match resp {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "nexus_orchestration",
                    error = %e,
                    "orchestration: chiamata LLM fallita, degrado all'euristica esistente"
                );
                return Ok(None);
            }
            Err(_) => {
                tracing::warn!(
                    target: "nexus_orchestration",
                    timeout_s,
                    "orchestration: chiamata LLM oltre il timeout, degrado all'euristica esistente"
                );
                return Ok(None);
            }
        };

        // (6) Estrai il testo (forma neural_service: `content`/`text`) e parsa il
        // blocco JSON col punto unico; valida con orchestration_reason::validate_orch_move
        // (enum CHIUSO OrchestrationMove). Testo/JSON assente -> Fallback (il gate lo
        // tratta come "usa euristica", identico a Ok(None)). isolation_available e
        // delegation_forbidden arrivano dal ctx (segnali strutturati risolti a monte,
        // regola M): in Fase 1 ctx.isolation_available e' sempre false ->
        // ParallelIsolated degrada a Sequential dentro validate_orch_move.
        let text = value
            .get("content")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("text").and_then(|v| v.as_str()))
            .unwrap_or("");
        let parsed = match nexus_types::llm_json::extract_json_block(text) {
            Some(v) => v,
            None => {
                tracing::debug!(
                    target: "nexus_orchestration",
                    phase = ?ctx.phase,
                    "orchestration: risposta senza JSON parsabile, degrado (Fallback)"
                );
                return Ok(Some(OrchestrationMove::Fallback));
            }
        };
        // validate_orch_move: qualunque forma malformata / collezione vuota / mossa
        // non applicabile per una guard deterministica (delegation_forbidden /
        // ParallelIsolated senza isolamento) -> OrchestrationMove::Fallback (il gate
        // lo tratta come "usa euristica"). Il nodo/gate ri-valida (idempotente).
        let mv = validate_orch_move(&parsed, ctx.isolation_available, ctx.delegation_forbidden);
        tracing::info!(
            target: "nexus_orchestration",
            phase = ?ctx.phase,
            behavior_mode = %ctx.behavior_mode,
            "orchestration: mossa decisa dal meta-reasoner"
        );
        Ok(Some(mv))
    }

    /// `true` se il kill-switch della scala e' attivo (`agent.scale.enabled`).
    /// Default OFF (opt-in): setting assente / malformato -> `false` (regola G:
    /// nessun fallback che accenda una feature). Con `false` la porta e' inerte.
    async fn scale_enabled(&self) -> bool {
        nexus_auth::get_setting(&self.db, SCALE_ENABLED_SETTING)
            .await
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    /// `true` se il SIZING agentico e' abilitato (`agent.scale.sizing_enabled`, mig
    /// 0522). Default OFF (opt-in, regola G): con `false` l'addendum sizing NON e'
    /// appeso al prompt -> prompt byte-identico al pre-0522 (bit-identico).
    async fn scale_sizing_enabled(&self) -> bool {
        nexus_auth::get_setting(&self.db, SCALE_SIZING_ENABLED_SETTING)
            .await
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    /// Timeout (s) della scala clampato in `[5, 300]`. Setting assente / malformato
    /// -> [`SCALE_TIMEOUT_DEFAULT`] (safe-default numerico, mig 0516).
    async fn scale_timeout_s(&self) -> u64 {
        nexus_auth::get_setting(&self.db, SCALE_TIMEOUT_SETTING)
            .await
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(SCALE_TIMEOUT_DEFAULT)
            .clamp(5, 300)
    }

    /// Consulta l'LLM per la SCALA-TIER (Real). PARADIGMA IDENTICO a
    /// [`Self::consult_llm`]/[`Self::consult_orch_llm`] (regola L, scope disgiunto):
    /// kill-switch -> purpose (`scale_assess`, regola G) -> template
    /// (`system.scale.assess`) -> LLM one-shot (SOLO lo [`ScaleContext`]
    /// serializzato, regola M) -> `extract_json_block` + [`validate_scale_move`]
    /// (punto unico, enum CHIUSO; malformato -> `KeepTier`). Degrado safe su ogni
    /// guasto NON infrastrutturale (`Ok(None)`); `Err` solo su DB down / provider
    /// non risolto (regola G: mai OFF mascherante).
    async fn consult_scale_llm(&self, ctx: &ScaleContext) -> Result<Option<ScaleMove>, PortError> {
        // (1) Kill-switch: opt-in, default OFF -> inerte (regola G).
        if !self.scale_enabled().await {
            return Ok(None);
        }

        // (2) Modello dal purpose (regola G). STESSA distinzione delle cause di
        // recover/orchestrate (niente OFF mascherante).
        let (provider, model) = match resolve_purpose_model_db(&self.db, SCALE_PURPOSE).await {
            PurposeResolution::Resolved {
                provider, model, ..
            } => (provider, model),
            PurposeResolution::NotFound => {
                tracing::error!(
                    target: "nexus_scale_controller",
                    purpose = SCALE_PURPOSE,
                    metric = "scale_controller_misconfig",
                    "scale: flag ON ma purpose '{}' assente in nexus_purpose_model \
                     (applicare la migrazione 0516); degrado (nessuna scala)",
                    SCALE_PURPOSE
                );
                return Ok(None);
            }
            PurposeResolution::NoCapableModel { tier } => {
                return Err(PortError::ProviderUnavailable(format!(
                    "scale: nessun modello del tier '{tier}' per purpose '{SCALE_PURPOSE}'"
                )));
            }
            PurposeResolution::MatrixUnavailable(e) => {
                return Err(PortError::ProviderUnavailable(format!(
                    "scale: routing non disponibile: {e}"
                )));
            }
        };

        // (3) Template (punto unico loader, regola L).
        let tpl_cache = crate::prompt_templates::TemplateCache::new();
        let mut system_text = crate::prompt_templates::get_template_or_default(
            &self.db,
            &tpl_cache,
            SCALE_TEMPLATE_KEY,
        )
        .await;
        if system_text.trim().is_empty() {
            tracing::error!(
                target: "nexus_scale_controller",
                key = SCALE_TEMPLATE_KEY,
                metric = "scale_controller_misconfig",
                "scale: template '{}' assente/vuoto (applicare la migrazione 0516); \
                 degrado (nessuna scala)",
                SCALE_TEMPLATE_KEY
            );
            return Ok(None);
        }

        // SIZING (mig 0522): appende l'ADDENDUM che descrive la mossa `adjust_sizing`
        // SOLO se il sizing e' abilitato. Con sizing OFF il prompt resta byte-identico
        // al pre-0522 (bit-identico per il flusso tier); il gate lato motore degrada
        // comunque ogni AdjustSizing a KeepTier. Se l'addendum manca (misconfig) si
        // procede col solo template tier (degrado safe, non un errore).
        if self.scale_sizing_enabled().await {
            let addendum = crate::prompt_templates::get_template_or_default(
                &self.db,
                &tpl_cache,
                SCALE_SIZING_TEMPLATE_KEY,
            )
            .await;
            if addendum.trim().is_empty() {
                tracing::warn!(
                    target: "nexus_scale_controller",
                    key = SCALE_SIZING_TEMPLATE_KEY,
                    "scale: sizing ON ma addendum template assente (applicare la mig 0522); \
                     procedo col solo template tier"
                );
            } else {
                system_text.push_str("\n\n");
                system_text.push_str(&addendum);
            }
        }

        // (4) Payload: SOLO lo ScaleContext serializzato (regola M: segnali
        // strutturati, non l'intera history -> budget/costo).
        let ctx_json = match serde_json::to_string(ctx) {
            Ok(j) => j,
            Err(err) => {
                tracing::warn!(
                    target: "nexus_scale_controller",
                    error = %err,
                    "scale: serializzazione ScaleContext fallita, degrado"
                );
                return Ok(None);
            }
        };
        let user_text = format!("Andamento del run strutturato (JSON):\n{ctx_json}");
        let messages =
            serde_json::json!([{ "role": "user", "content": user_text }]).to_string();
        let timeout_s = self.scale_timeout_s().await;

        // (5) Chiamata LLM one-shot (paradigma verify_profile; nessun tool). Timeout
        // + degrado safe: ogni esito non utile -> Ok(None).
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_s),
            self.neural.generate_agent_turn(
                &provider,
                &model,
                &messages,
                "[]",
                SCALE_MAX_TOKENS,
                &system_text,
            ),
        )
        .await;
        let value = match resp {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "nexus_scale_controller",
                    error = %e,
                    "scale: chiamata LLM fallita, degrado (nessuna scala)"
                );
                return Ok(None);
            }
            Err(_) => {
                tracing::warn!(
                    target: "nexus_scale_controller",
                    timeout_s,
                    "scale: chiamata LLM oltre il timeout, degrado (nessuna scala)"
                );
                return Ok(None);
            }
        };

        // (6) Estrai il testo e parsa il blocco JSON; valida col punto unico
        // scale_reason::validate_scale_move (enum CHIUSO; tier fuori vocabolario /
        // confidence fuori [0,1] -> KeepTier). Testo/JSON assente -> KeepTier.
        let text = value
            .get("content")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("text").and_then(|v| v.as_str()))
            .unwrap_or("");
        let parsed = match nexus_types::llm_json::extract_json_block(text) {
            Some(v) => v,
            None => {
                tracing::debug!(
                    target: "nexus_scale_controller",
                    "scale: risposta senza JSON parsabile, degrado (KeepTier)"
                );
                return Ok(Some(ScaleMove::KeepTier));
            }
        };
        let mv = validate_scale_move(&parsed);
        tracing::info!(
            target: "nexus_scale_controller",
            current_tier = ctx.current_tier.as_str(),
            iterations = ctx.iterations,
            "scale: mossa decisa dallo scale-controller"
        );
        Ok(Some(mv))
    }
}

#[async_trait]
impl MetaReasonerPort for PgMetaReasonerPort {
    /// Consulta il meta-reasoner di RECOVERY secondo `mode`.
    ///
    /// - `Replay` -> `Ok(None)` IMMEDIATO, senza toccare DB/gateway: il
    ///   `ReplayLlmGateway` rigioca solo `purpose=="executor"`, quindi consultare
    ///   l'LLM qui darebbe vuoto. Il nodo usa la cache-in-extra (resume) o degrada
    ///   a `Fallback` -> euristica (shadow, parita' col Python che non ha il
    ///   reasoner).
    /// - `Real` -> consulta l'LLM (kill-switch, purpose, template, parse+validate).
    async fn recover(
        &self,
        ctx: StallContext,
        mode: ExecMode,
    ) -> Result<Option<RecoveryMove>, PortError> {
        if mode != ExecMode::Real {
            // GATE REPLAY (vedi doc-modulo): niente LLM in Replay.
            return Ok(None);
        }
        self.consult_llm(&ctx).await
    }

    /// Consulta il meta-reasoner di ORCHESTRAZIONE (plan-phase / decompose /
    /// delega) secondo `mode`. GEMELLO di [`Self::recover`] su scope disgiunto
    /// (regola L: STESSO flusso).
    ///
    /// - `Replay` -> `Ok(None)` IMMEDIATO, senza toccare DB/gateway (GATE REPLAY,
    ///   opzione A): il `ReplayLlmGateway` rigioca solo `purpose=="executor"`,
    ///   quindi consultare l'LLM qui darebbe vuoto. Il gate degrada all'euristica
    ///   esistente (`is_eligible`/`should_parallelize`) -> parita' shadow col
    ///   percorso Python (che non ha il reasoner di orchestrazione).
    /// - `Real` -> consulta l'LLM (kill-switch `agent.orchestration.enabled`,
    ///   purpose `orchestration_decide`, template `system.orchestration.decide`,
    ///   parse + `validate_orch_move` con `isolation_available=false` in Fase 1).
    ///
    /// Vincolo primario: kill-switch OFF (default) -> `Ok(None)` -> il gate ricade
    /// su `is_eligible` -> comportamento BIT-IDENTICO a oggi.
    async fn orchestrate(
        &self,
        ctx: OrchestrationContext,
        mode: ExecMode,
    ) -> Result<Option<OrchestrationMove>, PortError> {
        if mode != ExecMode::Real {
            // GATE REPLAY (opzione A, vedi doc-metodo): niente LLM in Replay.
            return Ok(None);
        }
        self.consult_orch_llm(&ctx).await
    }

    /// Consulta lo SCALE-CONTROLLER (scala-tier up/down) secondo `mode`. GEMELLO di
    /// [`Self::recover`]/[`Self::orchestrate`] su scope disgiunto (regola L: STESSO
    /// flusso).
    ///
    /// - `Replay` -> `Ok(None)` IMMEDIATO senza I/O (GATE REPLAY opzione A): la
    ///   scala-move e' persistita in `extra`/`sticky` dal primario; il rientro
    ///   rilegge sticky -> stesso modello per costruzione (parita' shadow col
    ///   Python, che non ha il controller).
    /// - `Real` -> consulta l'LLM (kill-switch `agent.scale.enabled` OFF di default,
    ///   purpose `scale_assess`, template `system.scale.assess`, parse +
    ///   `validate_scale_move`; i 5 gate deterministici sono a valle nel nodo PR-B).
    ///
    /// PR-A: nessun nodo/detector chiama questo metodo (quello e' PR-B) e il flag e'
    /// OFF di default -> BIT-IDENTICO a oggi (vincolo primario).
    async fn assess_scale(
        &self,
        ctx: ScaleContext,
        mode: ExecMode,
    ) -> Result<Option<ScaleMove>, PortError> {
        if mode != ExecMode::Real {
            // GATE REPLAY (opzione A, vedi doc-metodo): niente LLM in Replay.
            return Ok(None);
        }
        self.consult_scale_llm(&ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    /// Pool lazy: i rami testati non aprono connessioni (Replay gata prima di
    /// toccare il DB). `connect_lazy` non si connette davvero. Non e' un fallback
    /// hardcoded (regola G): serve solo a costruire il tipo `PgPool`.
    fn lazy_pool() -> PgPool {
        PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette davvero")
    }

    fn port() -> PgMetaReasonerPort {
        PgMetaReasonerPort::new(lazy_pool(), NeuralCoreClient)
    }

    fn stall_ctx() -> StallContext {
        StallContext {
            axis: "signature".to_string(),
            work_epoch: 3,
            ..Default::default()
        }
    }

    fn orch_ctx() -> OrchestrationContext {
        OrchestrationContext {
            behavior_mode: "automatic".to_string(),
            task_complexity: 7,
            ..Default::default()
        }
    }

    /// In `Replay` la porta ritorna `Ok(None)` SENZA toccare DB/gateway (il gate
    /// scatta prima di qualunque I/O: il pool lazy non connesso non e' mai usato).
    #[tokio::test]
    async fn replay_ritorna_none_senza_llm() {
        let res = port().recover(stall_ctx(), ExecMode::Replay).await;
        assert_eq!(res.expect("ok"), None, "Replay -> Ok(None) senza consultare l'LLM");
    }

    /// Il gate `Replay` precede ogni accesso al DB: anche con un pool non
    /// connesso non c'e' errore (nessuna query). Robustezza del gate.
    #[tokio::test]
    async fn replay_non_dipende_dal_db() {
        // Chiamata ripetuta: deterministica, nessun panico, nessuna connessione.
        for _ in 0..3 {
            assert_eq!(
                port().recover(stall_ctx(), ExecMode::Replay).await.expect("ok"),
                None
            );
        }
    }

    /// Con risposta malformata `validate_move` degrada a `Fallback` (il nodo lo
    /// tratta come "usa euristica"). Verifica del punto unico di validazione
    /// riusato dall'impl (regola L): il parse+validate NON re-implementa la logica.
    #[test]
    fn validate_move_su_risposta_malformata_e_fallback() {
        // Forma JSON che NON deserializza in RecoveryMove -> Fallback.
        let parsed = nexus_types::llm_json::extract_json_block(r#"{"move":"boh"}"#)
            .expect("json parsabile");
        assert_eq!(validate_move(&parsed), RecoveryMove::Fallback);
        // Testo senza JSON -> extract_json_block None (l'impl mappa a Fallback).
        assert!(nexus_types::llm_json::extract_json_block("nessun json qui").is_none());
    }

    // ── orchestrate (#11c) ────────────────────────────────────────────────────

    /// In `Replay` la porta di orchestrazione ritorna `Ok(None)` SENZA toccare
    /// DB/gateway (GATE REPLAY opzione A: gata prima di qualunque I/O, il pool
    /// lazy non connesso non e' mai usato).
    #[tokio::test]
    async fn orchestrate_replay_ritorna_none_senza_llm() {
        let res = port().orchestrate(orch_ctx(), ExecMode::Replay).await;
        assert_eq!(
            res.expect("ok"),
            None,
            "Replay -> Ok(None) senza consultare l'LLM"
        );
    }

    /// Il gate `Replay` dell'orchestrazione precede ogni accesso al DB: anche con
    /// un pool non connesso non c'e' errore (nessuna query). Robustezza del gate.
    #[tokio::test]
    async fn orchestrate_replay_non_dipende_dal_db() {
        for _ in 0..3 {
            assert_eq!(
                port()
                    .orchestrate(orch_ctx(), ExecMode::Replay)
                    .await
                    .expect("ok"),
                None
            );
        }
    }

    /// Con risposta malformata `validate_orch_move` degrada a `Fallback` (il gate
    /// lo tratta come "usa euristica"). Verifica del punto unico di validazione
    /// riusato dall'impl (regola L). In Fase 1 `isolation_available=false`: una
    /// delega `parallel_isolated` con task validi NON e' `Fallback` ma DEGRADA a
    /// `Sequential` (la delega resta, cade solo il parallelismo).
    #[test]
    fn validate_orch_move_su_risposta_malformata_e_fallback() {
        // Enum sconosciuto -> Fallback (non deserializza in OrchestrationMove).
        let parsed = nexus_types::llm_json::extract_json_block(r#"{"move":"boh"}"#)
            .expect("json parsabile");
        assert_eq!(
            validate_orch_move(&parsed, false, false),
            OrchestrationMove::Fallback
        );
        // Testo senza JSON -> extract_json_block None (l'impl mappa a Fallback).
        assert!(nexus_types::llm_json::extract_json_block("nessun json qui").is_none());
        // Delega parallela senza isolamento fisico (Fase 1): la delega resta valida,
        // cade solo il parallelismo -> Sequential (NON Fallback).
        let par = nexus_types::llm_json::extract_json_block(
            r#"{"move":"delegate_subagents","tasks":[{"task_description":"x","kind":"coder","write_scope":["src/a"]}],"coordination":"parallel_isolated"}"#,
        )
        .expect("json parsabile");
        assert!(matches!(
            validate_orch_move(&par, false, false),
            OrchestrationMove::DelegateSubagents {
                coordination: nexus_agent_graph::runtime::ports::Coordination::Sequential,
                ..
            }
        ));
    }

    // ── assess_scale (SCALE-CONTROLLER PR-A) ────────────────────────────────────

    fn scale_ctx() -> ScaleContext {
        ScaleContext {
            behavior_mode: "automatic".to_string(),
            iterations: 8,
            iteration_cap: 20,
            requires_tool_use: true,
            ..Default::default()
        }
    }

    /// In `Replay` la porta di scala ritorna `Ok(None)` SENZA toccare DB/gateway
    /// (GATE REPLAY opzione A: gata prima di qualunque I/O, il pool lazy non
    /// connesso non e' mai usato).
    #[tokio::test]
    async fn assess_scale_replay_ritorna_none_senza_llm() {
        let res = port().assess_scale(scale_ctx(), ExecMode::Replay).await;
        assert_eq!(
            res.expect("ok"),
            None,
            "Replay -> Ok(None) senza consultare l'LLM"
        );
    }

    /// Il gate `Replay` della scala precede ogni accesso al DB: anche con un pool
    /// non connesso non c'e' errore (nessuna query). Robustezza del gate.
    #[tokio::test]
    async fn assess_scale_replay_non_dipende_dal_db() {
        for _ in 0..3 {
            assert_eq!(
                port()
                    .assess_scale(scale_ctx(), ExecMode::Replay)
                    .await
                    .expect("ok"),
                None
            );
        }
    }

    /// Con risposta malformata `validate_scale_move` degrada a `KeepTier` (il nodo
    /// lo tratta come "nessuna scala"). Verifica del punto unico di validazione
    /// riusato dall'impl (regola L): il parse+validate NON re-implementa la logica.
    #[test]
    fn validate_scale_move_su_risposta_malformata_e_keep() {
        // Enum sconosciuto -> KeepTier (non deserializza in ScaleMove).
        let parsed = nexus_types::llm_json::extract_json_block(r#"{"move":"boh"}"#)
            .expect("json parsabile");
        assert_eq!(validate_scale_move(&parsed), ScaleMove::KeepTier);
        // Testo senza JSON -> extract_json_block None (l'impl mappa a KeepTier).
        assert!(nexus_types::llm_json::extract_json_block("nessun json qui").is_none());
    }
}
