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
//! implementato in [`PgMetaReasonerPort::consult_orch_llm`] sul FLUSSO UNICO
//! condiviso [`PgMetaReasonerPort::consult_meta_llm`] (regola L: STESSO flusso,
//! parametrizzato dalla [`MetaConsultSpec`] dello scope, non re-inventato):
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

/// Parametri STATICI (const) che distinguono i 3 scope del meta-reasoner LLM
/// (recovery / orchestrazione / scala). Il FLUSSO a 6 passi e' UNICO
/// ([`PgMetaReasonerPort::consult_meta_llm`], regola L: un solo punto di controllo);
/// qui vivono SOLO le differenze dichiarative, cosi' aggiungere un 4o scope =
/// una const in piu', non una quarta re-implementazione del flusso.
struct MetaConsultSpec {
    /// Setting kill-switch (opt-in, default OFF -> porta inerte).
    enabled_setting: &'static str,
    /// Setting del timeout (s) della chiamata LLM, clamp `[5,300]`.
    timeout_setting: &'static str,
    /// Timeout (s) di default se il setting manca / e' malformato (safe-default
    /// numerico seminato dalla migrazione dello scope, non un magic fallback).
    timeout_default: u64,
    /// Purpose (regola G) risolto tier-aware da `nexus_purpose_model`.
    purpose: &'static str,
    /// Chiave del template di sistema (schema XML fuori-chat, regola D).
    template_key: &'static str,
    /// Tetto di token in output (la mossa e' un piccolo oggetto JSON).
    max_tokens: u32,
    /// Valore del campo strutturato `metric` sui log di misconfig (preservato per
    /// scope: e' un contratto d'osservabilita', regola M).
    misconfig_metric: &'static str,
    /// Identificatore dello scope: campo `kind` dei log unificati e prefisso dei
    /// messaggi d'errore infrastrutturali.
    kind: &'static str,
    /// Prefisso del turno user che introduce il contesto serializzato.
    user_prefix: &'static str,
}

/// Spec dello scope RECOVERY-da-stallo (mig 0510). Costruita dai const dello scope
/// cosi' restano l'unica fonte dei valori (nessun doc-link orfano, regola L).
const STALL_SPEC: MetaConsultSpec = MetaConsultSpec {
    enabled_setting: STALL_ENABLED_SETTING,
    timeout_setting: STALL_TIMEOUT_SETTING,
    timeout_default: STALL_TIMEOUT_DEFAULT,
    purpose: STALL_PURPOSE,
    template_key: STALL_TEMPLATE_KEY,
    max_tokens: STALL_MAX_TOKENS,
    misconfig_metric: "stall_recovery_misconfig",
    kind: "stall_recovery",
    user_prefix: "Stato di stallo strutturato (JSON):",
};

/// Spec dello scope ORCHESTRAZIONE (mig 0512). Gemello di [`STALL_SPEC`] su scope
/// disgiunto: STESSO flusso, differenze solo dichiarative (regola L).
const ORCH_SPEC: MetaConsultSpec = MetaConsultSpec {
    enabled_setting: ORCH_ENABLED_SETTING,
    timeout_setting: ORCH_TIMEOUT_SETTING,
    timeout_default: ORCH_TIMEOUT_DEFAULT,
    purpose: ORCH_PURPOSE,
    template_key: ORCH_TEMPLATE_KEY,
    max_tokens: ORCH_MAX_TOKENS,
    misconfig_metric: "orchestration_misconfig",
    kind: "orchestration",
    user_prefix: "Contesto di orchestrazione strutturato (JSON):",
};

/// Spec dello scope SCALE-CONTROLLER (mig 0516). Terzo scope disgiunto sul flusso
/// unico (regola L).
const SCALE_SPEC: MetaConsultSpec = MetaConsultSpec {
    enabled_setting: SCALE_ENABLED_SETTING,
    timeout_setting: SCALE_TIMEOUT_SETTING,
    timeout_default: SCALE_TIMEOUT_DEFAULT,
    purpose: SCALE_PURPOSE,
    template_key: SCALE_TEMPLATE_KEY,
    max_tokens: SCALE_MAX_TOKENS,
    misconfig_metric: "scale_controller_misconfig",
    kind: "scale",
    user_prefix: "Andamento del run strutturato (JSON):",
};

/// Esito del percorso LLM one-shot condiviso ([`PgMetaReasonerPort::consult_meta_llm`]),
/// PRIMA della validazione tipizzata (che resta nel wrapper — punto unico per-scope
/// `validate_move`/`validate_orch_move`/`validate_scale_move`, regola L):
///   - `Degrade`  -> il wrapper ritorna `Ok(None)` (kill-switch OFF, misconfig
///                   purpose/template, timeout, risposta vuota, serializzazione fallita);
///   - `NoJson`   -> l'LLM ha risposto ma senza blocco JSON parsabile: il wrapper
///                   ritorna il proprio fallback tipizzato (`Fallback`/`KeepTier`);
///   - `Json(v)`  -> blocco JSON estratto: il wrapper lo valida col punto unico dello
///                   scope. Il guasto INFRASTRUTTURALE (DB down, provider non risolto)
///                   NON entra qui: e' un `Err(PortError)` propagato (regola G).
enum MetaLlmParse {
    Degrade,
    NoJson,
    Json(serde_json::Value),
}

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

    /// `true` se il kill-switch `key` e' attivo. Default OFF (opt-in): setting
    /// assente / malformato -> `false` (regola G: unica fonte DB, nessun fallback
    /// che accenda una feature). PUNTO UNICO dei 3 kill-switch del meta-reasoner
    /// (`agent.{stall_recovery,orchestration,scale}.enabled`, regola L).
    async fn setting_enabled(&self, key: &str) -> bool {
        nexus_auth::get_setting(&self.db, key)
            .await
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    /// Timeout (s) del setting `key` clampato in `[5, 300]`. Setting assente /
    /// malformato -> `default` (safe-default numerico seminato dalla migrazione
    /// dello scope). PUNTO UNICO dei 3 timeout del meta-reasoner (regola L).
    async fn setting_timeout_s(&self, key: &str, default: u64) -> u64 {
        nexus_auth::get_setting(&self.db, key)
            .await
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(default)
            .clamp(5, 300)
    }

    /// FLUSSO UNICO a 6 passi del meta-reasoner LLM (paradigma ADR 0036 di
    /// `verify_profile`), condiviso dai 3 scope (regola L: STESSO flusso, un solo
    /// punto di controllo — prima era re-implementato 3 volte). I wrapper
    /// [`Self::consult_llm`]/[`Self::consult_orch_llm`]/[`Self::consult_scale_llm`]
    /// passano la [`MetaConsultSpec`] del proprio scope e applicano la validazione
    /// tipizzata sull'esito ([`MetaLlmParse`]).
    ///
    /// Passi: (1) kill-switch (opt-in, OFF -> `Degrade`); (2) modello dal purpose
    /// (regola G, distinzione delle cause: NotFound -> misconfig+`Degrade`,
    /// NoCapableModel/MatrixUnavailable -> `Err` infrastrutturale, MAI `Degrade`
    /// mascherante); (3) template (punto unico loader, assente -> misconfig+`Degrade`);
    /// (4) payload = SOLO il contesto serializzato (regola M, non l'intera history);
    /// (5) chiamata LLM one-shot col timeout clamp (ogni esito non utile -> `Degrade`);
    /// (6) `extract_json_block` -> `Json`/`NoJson`.
    ///
    /// I log del percorso di degrado usano il target UNIFICATO `nexus_meta_reasoner`
    /// col campo `kind` (il `target:` di tracing deve essere const, percio' e' unico;
    /// il campo strutturato `metric` per-scope resta preservato). I log finali di
    /// esito (mossa decisa / risposta senza JSON) restano nei wrapper col target
    /// storico per-scope.
    async fn consult_meta_llm<C>(
        &self,
        spec: &MetaConsultSpec,
        ctx: &C,
    ) -> Result<MetaLlmParse, PortError>
    where
        C: serde::Serialize,
    {
        // (1) Kill-switch: opt-in, default OFF -> inerte (regola G).
        if !self.setting_enabled(spec.enabled_setting).await {
            return Ok(MetaLlmParse::Degrade);
        }

        // (2) Modello dal purpose (regola G). Distinzione delle cause (niente OFF
        // mascherante): Resolved -> procedi; NotFound -> misconfig (flag ON ma
        // migrazione non applicata): ERROR + metrica, poi degrado; NoCapableModel
        // (cooldown) / MatrixUnavailable (DB down) -> Err INFRASTRUTTURALE.
        let (provider, model) = match resolve_purpose_model_db(&self.db, spec.purpose).await {
            PurposeResolution::Resolved {
                provider, model, ..
            } => (provider, model),
            PurposeResolution::NotFound => {
                tracing::error!(
                    target: "nexus_meta_reasoner",
                    kind = spec.kind,
                    purpose = spec.purpose,
                    metric = spec.misconfig_metric,
                    "meta-reasoner: flag ON ma purpose assente in nexus_purpose_model \
                     (migrazione dello scope non applicata); degrado all'euristica esistente"
                );
                return Ok(MetaLlmParse::Degrade);
            }
            PurposeResolution::NoCapableModel { tier } => {
                return Err(PortError::ProviderUnavailable(format!(
                    "{}: nessun modello del tier '{tier}' per purpose '{}'",
                    spec.kind, spec.purpose
                )));
            }
            PurposeResolution::MatrixUnavailable(e) => {
                return Err(PortError::ProviderUnavailable(format!(
                    "{}: routing non disponibile: {e}",
                    spec.kind
                )));
            }
        };

        // (3) Template (punto unico loader, regola L). Cache monouso: la
        // consultazione e' rara. Assente/vuoto benche' enabled+purpose risolti ->
        // misconfig parziale: ERROR + degrado safe (il chiamante usa l'euristica).
        let tpl_cache = crate::prompt_templates::TemplateCache::new();
        let system_text = crate::prompt_templates::get_template_or_default(
            &self.db,
            &tpl_cache,
            spec.template_key,
        )
        .await;
        if system_text.trim().is_empty() {
            tracing::error!(
                target: "nexus_meta_reasoner",
                kind = spec.kind,
                key = spec.template_key,
                metric = spec.misconfig_metric,
                "meta-reasoner: template assente/vuoto (migrazione dello scope non \
                 applicata); degrado all'euristica esistente"
            );
            return Ok(MetaLlmParse::Degrade);
        }

        // (4) Payload: SOLO il contesto serializzato (regola M: segnali strutturati,
        // non l'intera history -> budget/costo). Un unico turno user.
        let ctx_json = match serde_json::to_string(ctx) {
            Ok(j) => j,
            Err(err) => {
                tracing::warn!(
                    target: "nexus_meta_reasoner",
                    kind = spec.kind,
                    error = %err,
                    "meta-reasoner: serializzazione contesto fallita, degrado"
                );
                return Ok(MetaLlmParse::Degrade);
            }
        };
        let user_text = format!("{}\n{ctx_json}", spec.user_prefix);
        let messages =
            serde_json::json!([{ "role": "user", "content": user_text }]).to_string();
        let timeout_s = self
            .setting_timeout_s(spec.timeout_setting, spec.timeout_default)
            .await;

        // (5) Chiamata LLM one-shot col paradigma verify_profile (generate_agent_turn
        // -> pin_provider gia' risolto, regola G; nessun tool). Timeout + degrado
        // safe: ogni esito non utile -> Degrade, il chiamante usa l'euristica.
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_s),
            self.neural.generate_agent_turn(
                &provider,
                &model,
                &messages,
                "[]",
                spec.max_tokens,
                &system_text,
            ),
        )
        .await;
        let value = match resp {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "nexus_meta_reasoner",
                    kind = spec.kind,
                    error = %e,
                    "meta-reasoner: chiamata LLM fallita, degrado all'euristica esistente"
                );
                return Ok(MetaLlmParse::Degrade);
            }
            Err(_) => {
                tracing::warn!(
                    target: "nexus_meta_reasoner",
                    kind = spec.kind,
                    timeout_s,
                    "meta-reasoner: chiamata LLM oltre il timeout, degrado all'euristica esistente"
                );
                return Ok(MetaLlmParse::Degrade);
            }
        };

        // (6) Estrai il testo (forma neural_service: `content`/`text`) e parsa il
        // blocco JSON col punto unico. La validazione tipizzata resta nel wrapper
        // (punto unico per-scope): testo/JSON assente -> NoJson.
        let text = value
            .get("content")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("text").and_then(|v| v.as_str()))
            .unwrap_or("");
        match nexus_types::llm_json::extract_json_block(text) {
            Some(v) => Ok(MetaLlmParse::Json(v)),
            None => Ok(MetaLlmParse::NoJson),
        }
    }

    /// Wrapper RECOVERY sul flusso unico [`Self::consult_meta_llm`] (regola L).
    /// Applica la validazione tipizzata col punto unico `validate_move` (enum CHIUSO
    /// `RecoveryMove` + blocker ADR 0034). Ritorna `Ok(Some(move))` (validata;
    /// `Fallback` incluso e' legittimo, il nodo lo tratta come "usa euristica")
    /// oppure `Ok(None)` (degrado safe) oppure `Err` solo su guasto INFRASTRUTTURALE.
    async fn consult_llm(&self, ctx: &StallContext) -> Result<Option<RecoveryMove>, PortError> {
        match self.consult_meta_llm(&STALL_SPEC, ctx).await? {
            MetaLlmParse::Degrade => Ok(None),
            MetaLlmParse::NoJson => {
                tracing::debug!(
                    target: "nexus_stall_recovery",
                    axis = %ctx.axis,
                    "stall_recovery: risposta senza JSON parsabile, degrado (Fallback)"
                );
                Ok(Some(RecoveryMove::Fallback))
            }
            MetaLlmParse::Json(parsed) => {
                // validate_move: forma malformata / enum sconosciuto / blocker fuori
                // vocabolario -> RecoveryMove::Fallback. Il nodo ri-valida (idempotente).
                let mv = validate_move(&parsed);
                tracing::info!(
                    target: "nexus_stall_recovery",
                    axis = %ctx.axis,
                    work_epoch = ctx.work_epoch,
                    "stall_recovery: mossa decisa dal meta-reasoner"
                );
                Ok(Some(mv))
            }
        }
    }

    /// Wrapper ORCHESTRAZIONE sul flusso unico [`Self::consult_meta_llm`] (regola L).
    /// Applica la validazione col punto unico `validate_orch_move` (enum CHIUSO
    /// `OrchestrationMove`), passando i segnali strutturati `isolation_available` /
    /// `delegation_forbidden` dal ctx (regola M): in Fase 1 `ctx.isolation_available`
    /// e' sempre `false` -> `ParallelIsolated` degrada a `Sequential`. Vincolo
    /// primario: kill-switch OFF (default) -> `Ok(None)` -> il gate ricade su
    /// `is_eligible` -> BIT-IDENTICO a oggi.
    async fn consult_orch_llm(
        &self,
        ctx: &OrchestrationContext,
    ) -> Result<Option<OrchestrationMove>, PortError> {
        match self.consult_meta_llm(&ORCH_SPEC, ctx).await? {
            MetaLlmParse::Degrade => Ok(None),
            MetaLlmParse::NoJson => {
                tracing::debug!(
                    target: "nexus_orchestration",
                    phase = ?ctx.phase,
                    "orchestration: risposta senza JSON parsabile, degrado (Fallback)"
                );
                Ok(Some(OrchestrationMove::Fallback))
            }
            MetaLlmParse::Json(parsed) => {
                let mv =
                    validate_orch_move(&parsed, ctx.isolation_available, ctx.delegation_forbidden);
                tracing::info!(
                    target: "nexus_orchestration",
                    phase = ?ctx.phase,
                    behavior_mode = %ctx.behavior_mode,
                    "orchestration: mossa decisa dal meta-reasoner"
                );
                Ok(Some(mv))
            }
        }
    }

    /// Wrapper SCALE-CONTROLLER sul flusso unico [`Self::consult_meta_llm`] (regola
    /// L). Applica la validazione col punto unico `validate_scale_move` (enum CHIUSO
    /// `ScaleMove`; tier fuori vocabolario / confidence fuori `[0,1]` -> `KeepTier`).
    /// I 5 gate deterministici dell'anti-oscillazione sono a valle nel nodo PR-B.
    async fn consult_scale_llm(&self, ctx: &ScaleContext) -> Result<Option<ScaleMove>, PortError> {
        match self.consult_meta_llm(&SCALE_SPEC, ctx).await? {
            MetaLlmParse::Degrade => Ok(None),
            MetaLlmParse::NoJson => {
                tracing::debug!(
                    target: "nexus_scale_controller",
                    "scale: risposta senza JSON parsabile, degrado (KeepTier)"
                );
                Ok(Some(ScaleMove::KeepTier))
            }
            MetaLlmParse::Json(parsed) => {
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
