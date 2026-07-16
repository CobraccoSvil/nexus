//! `ScaleControlNode` — superstep DEDICATO dello SCALE-CONTROLLER bidirezionale
//! (up/down del tier modello). GEMELLO 1:1 di
//! [`crate::nodes::stall_recovery::StallRecoveryNode`] su tipi DISGIUNTI
//! ([`ScaleContext`] -> [`ScaleMove`] via [`MetaReasonerPort::assess_scale`]).
//!
//! ## Perche' un nodo dedicato (non mid-superstep)
//!
//! Identica ragione dello `stall_recovery`: la sola LLM-call dello scale-controller
//! vive qui, in un superstep isolato dietro [`MetaReasonerPort`]. Consultare il
//! reasoner mid-superstep dentro l'executor romperebbe replay e shadow-diff (una
//! LLM-call non prevista dalla sequenza del run primario). Il percorso caldo
//! (executor) rilegge la [`ScaleMove`] dal checkpoint, non la ricalcola.
//!
//! ## Modello di replay dello scale-controller (opzione A, gia' nell'adapter)
//!
//! Come per il recovery, il `ReplayLlmGateway` (mcp-core) rigioca SOLO le
//! completion con `purpose=="executor"`; la completion `scale_assess` di questo
//! nodo ottiene una risposta ausiliaria neutra. La replay-safety si regge su tre
//! regimi coerenti:
//!   - **Real** (primario): la porta consulta l'LLM e la [`ScaleMove`] validata e'
//!     PERSISTITA in `extra[scale_cache_key]` (checkpoint del nodo, punto (4-5)).
//!   - **Resume Rust->Rust**: il checkpoint contiene gia' la mossa -> CACHE-HIT
//!     dall'`extra` (0 LLM, punto (2)). Deterministico.
//!   - **Shadow / Replay**: l'impl concreta ([`PgMetaReasonerPort::assess_scale`])
//!     applica l'OPZIONE A e in `Replay` ritorna `Ok(None)` IMMEDIATO senza I/O
//!     (nessuna LLM-call) -> il nodo degrada a `resolved_only()` -> il rientro
//!     nell'executor usa lo sticky corrente (il tier resta), parita' shadow col
//!     Python che non ha il controller. Il gate `mode` NON e' nel nodo: e' gia'
//!     nell'adapter (come per `recover`/`orchestrate`).
//!
//! ## Flusso del nodo (`run`)
//!
//! L'executor (detector-emissione, PR-B3) emette
//! `StateDelta{stop_reason=ScaleReason, extra += ScaleContext + chiave-cache}`;
//! `route_after_executor` instrada qui. Il nodo:
//!   1. legge lo [`ScaleContext`] da `extra[`[`SCALE_CONTEXT_KEY`]`]` (segnale
//!      strutturato prodotto dall'executor, regola M); assente/malformato ->
//!      degrado sicuro `resolved_only()`;
//!   2. determina la chiave-cache col PUNTO UNICO
//!      [`crate::decisions::scale_reason::scale_cache_key`] (analogo a
//!      `stall_move_key`): la legge da `extra[`[`SCALE_MOVE_CACHE_KEY_KEY`]`]` se il
//!      produttore l'ha gia' calcolata (con l'`eval_every_iters` DB-driven che
//!      possiede, regola L: la formula e' una sola, calcolata una volta a monte).
//!      Se una [`ScaleMove`] e' GIA' in `extra` a quella chiave -> CACHE-HIT: la
//!      RIUSA senza LLM (0 token: replay + idempotenza);
//!   3. cache-miss: chiama [`MetaReasonerPort::assess_scale`] — UNA sola LLM-call —
//!      e valida col PUNTO UNICO
//!      [`crate::decisions::scale_reason::validate_scale_move`] seguito da
//!      [`crate::decisions::scale_reason::apply_hysteresis`] (l'LLM NON scavalca i
//!      5 gate). `KeepTier` / `Ok(None)` / `Err` -> `resolved_only()`;
//!   4. persiste la [`ScaleMove`] validata in `extra` col pattern clone-whole-map
//!      ([`crate::state::put_extra`], regola L: `extra` e' OVERWRITE totale);
//!   5. ritorna `StopReason::ScaleResolved` -> self-loop rientra nell'executor, che
//!      al rientro consuma la mossa (detector-rientro, PR-B3).
//!
//! ## Inerzia a runtime (VINCOLO PR-B2)
//!
//! PR-B2 e' INERTE PER COSTRUZIONE: NESSUN detector emette ancora
//! `StopReason::ScaleReason` (quello e' PR-B3), quindi
//! `route_after_executor` non instrada MAI qui e questo nodo non e' raggiunto:
//! il comportamento del motore resta bit-identico. Anche se raggiunto, con la porta
//! che ritorna `Ok(None)` (kill-switch `agent.scale.enabled` OFF di default / stub)
//! il nodo NON persiste alcuna mossa e ritorna comunque `ScaleResolved`: al rientro
//! l'executor mantiene lo sticky corrente (nessun cambio-tier).
//!
//! Il nodo NON instrada: l'edge `ScaleControl -> Executor` e' dichiarato fuori, in
//! `graph.rs` (self-loop come `StallRecovery -> Executor`).

use std::sync::Arc;

use async_trait::async_trait;

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::scale_reason::{
    apply_hysteresis, apply_sizing_gate, scale_cache_key, validate_scale_move,
    ScaleHysteresisConfig, ScaleSizingConfig,
};
use crate::runtime::ports::{MetaReasonerPort, ScaleContext, ScaleMove};
use crate::runtime::AgentNodeCtx;
use crate::state::{put_extra, AgentState, StateDelta, StopReason};

/// Chiave in `AgentState::extra` sotto cui l'executor serializza lo
/// [`ScaleContext`] prima di instradare al nodo (PUNTO UNICO, regola L: sia
/// l'executor produttore sia questo nodo consumatore la usano). Il valore e' lo
/// [`ScaleContext`] in forma JSON (regola M: segnali strutturati, non prosa).
/// Analogo a [`crate::nodes::stall_recovery::STALL_CONTEXT_KEY`].
pub const SCALE_CONTEXT_KEY: &str = "scale_context";

/// Chiave in `AgentState::extra` sotto cui l'executor serializza la CHIAVE-CACHE
/// dello scale-move gia' calcolata col punto unico
/// [`crate::decisions::scale_reason::scale_cache_key`] (che richiede
/// l'`eval_every_iters` DB-driven, posseduto dal produttore executor, regola G).
///
/// Perche' trasportarla e non ricalcolarla nel nodo (come fa `stall_recovery` con
/// `stall_move_key`): la formula `scale_cache_key` dipende da `eval_every_iters`,
/// un setting `agent.scale.*` che il nodo NON legge (nessun I/O nel nodo). Il
/// produttore la calcola UNA volta col valore reale e la trasporta (regola L: una
/// sola formula, un solo punto di calcolo). Se assente (guasto a monte / produttore
/// che non l'ha scritta), il nodo ricade sul default conservativo
/// `eval_every_iters` della mig 0516 via [`fallback_cache_key`] — deterministico,
/// non un magic-fallback di modello (regola G: sceglie solo la CHIAVE, non il tier).
pub const SCALE_MOVE_CACHE_KEY_KEY: &str = "scale_move_cache_key";

/// Chiave in `AgentState::extra` sotto cui l'executor serializza la
/// [`ScaleHysteresisConfig`] DB-driven (le 5 soglie `agent.scale.*`:
/// `downscale_enabled`, `min_confidence`, `change_cooldown_turns`,
/// `downscale_clean_window`, `max_reversals` + `window_overhead_ratio`).
///
/// Stesso pattern di [`SCALE_MOVE_CACHE_KEY_KEY`] (regola L, punto unico del
/// trasporto config->nodo): il nodo NON legge i settings (zero I/O per il
/// determinismo di replay), quindi il produttore executor — che possiede
/// `ExecutorConfig.scale` letto dal DB — serializza la config UNA volta e la
/// trasporta. Il gate [`apply_hysteresis`] la consuma al posto dei default
/// hardcoded, cosi' le soglie admin raggiungono davvero il punto di enforcement
/// (chiude la config muta, regola G). Se assente (guasto a monte / produttore
/// che non l'ha scritta) il nodo ricade sui seed conservativi mig 0516 via
/// [`ScaleHysteresisConfig::conservative_defaults`] — deterministico, non un
/// magic-fallback (i default coincidono coi seed DB).
pub const SCALE_HYSTERESIS_CFG_KEY: &str = "scale_hysteresis_cfg";

/// Chiave in `AgentState::extra` sotto cui l'executor serializza la
/// [`ScaleSizingConfig`] DB-driven (kill-switch nested `sizing_enabled`,
/// `min_confidence` riusato, `cooldown_turns`, `aggressiveness`). Stesso pattern di
/// [`SCALE_HYSTERESIS_CFG_KEY`] (regola L, trasporto config->nodo senza I/O nel
/// nodo): il gate [`apply_sizing_gate`] la consuma per una [`ScaleMove::AdjustSizing`].
/// Assente (guasto a monte / sizing non trasportato) -> fallback conservativo
/// [`ScaleSizingConfig::conservative_defaults`] (sizing OFF) -> AdjustSizing degrada a
/// `KeepTier` (bit-identico, non un magic-fallback: sceglie solo il gate, regola G).
pub const SCALE_SIZING_CFG_KEY: &str = "scale_sizing_cfg";

/// Chiave in `AgentState::extra` sotto cui il RIENTRO dell'executor
/// (`consume_scale_move`, ramo `AdjustSizing`) persiste gli [`SizingOverrides`]
/// concreti risolti dalla postura. Canale executor->executor (il nodo NON lo tocca):
/// il blocco di riduzione contesto e il gate g1-loop del turno successivo lo leggono
/// via gli helper `effective_*`. Sticky per il resto del run finche' una nuova
/// postura non lo sostituisce. Assente -> soglie fisse (bit-identico).
pub const SCALE_SIZING_OVERRIDES_KEY: &str = "scale_sizing";

/// Nodo dello scale-controller.
///
/// Riceve la porta [`MetaReasonerPort`] iniettata dal chiamante (mcp-core con
/// l'impl concreta `PgMetaReasonerPort`; i test con
/// [`crate::runtime::StubMetaReasonerPort`], che ritorna `Ok(None)` -> comportamento
/// inerte). Il nodo NON legge il DB ne' risolve tier/model: tutto l'I/O e' dietro la
/// porta (inversione di dipendenza, regola L). Usa SOLO
/// [`MetaReasonerPort::assess_scale`]; `recover`/`orchestrate` della stessa porta
/// sono consumati dagli altri nodi (regola L: UNA sola porta, tre scope disgiunti).
pub struct ScaleControlNode {
    /// Porta del meta-reasoner (I/O LLM). Con `Ok(None)` il nodo e' inerte.
    reasoner: Arc<dyn MetaReasonerPort>,
}

impl ScaleControlNode {
    /// Costruisce il nodo con la porta del reasoner iniettata.
    pub fn new(reasoner: Arc<dyn MetaReasonerPort>) -> Self {
        Self { reasoner }
    }

    /// Delta di sola risoluzione (nessuna mossa persistita): rientra nell'executor
    /// via `ScaleResolved`. Usato quando manca lo [`ScaleContext`] (guasto di
    /// costruzione a monte), quando il reasoner ritorna `Ok(None)` (kill-switch OFF
    /// / stub / Replay opzione A), o quando la mossa e' `KeepTier` (rete di
    /// sicurezza: al rientro l'executor mantiene lo sticky corrente, nessun
    /// cambio-tier).
    fn resolved_only() -> OpaqueDelta {
        StateDelta {
            stop_reason: Some(Some(StopReason::ScaleResolved)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Delta che persiste `mv` in `extra` alla chiave-cache (clone-whole-map) e
    /// risolve la scala. La mossa e' serializzata come JSON (`ScaleMove` deriva
    /// `Serialize`): il rientro nell'executor la rileggera' e la consumera' (PR-B3).
    /// PUNTO UNICO della scrittura extra: `put_extra` clona l'intera mappa e non
    /// azzera gli altri canali.
    fn persisted(state: &AgentState, key: &str, mv: &ScaleMove) -> OpaqueDelta {
        // `ScaleMove` serializza sempre a un oggetto JSON (enum taggato): in caso
        // improbabile di errore di serializzazione, degradiamo a `resolved_only`
        // (nessuna mossa) invece di panicare (regola errori).
        let value = match serde_json::to_value(mv) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "nexus_agent_graph::scale_control",
                    error = %err,
                    "scale_control: serializzazione ScaleMove fallita, degrado a resolved-only"
                );
                return Self::resolved_only();
            }
        };
        let extra = put_extra(state, key, value);
        StateDelta {
            extra: Some(extra),
            stop_reason: Some(Some(StopReason::ScaleResolved)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Estrae lo [`ScaleContext`] serializzato da `extra[SCALE_CONTEXT_KEY]`.
    /// `None` se assente o non deserializzabile (guasto di costruzione a monte:
    /// il nodo degrada a `resolved_only`, mai un panico).
    fn read_context(state: &AgentState) -> Option<ScaleContext> {
        let raw = state.extra.get(SCALE_CONTEXT_KEY)?;
        serde_json::from_value::<ScaleContext>(raw.clone()).ok()
    }

    /// Rilegge una [`ScaleMove`] gia' persistita in `extra` alla chiave-cache
    /// (cache-hit / replay). `None` se assente o non deserializzabile.
    fn cached_move(state: &AgentState, key: &str) -> Option<ScaleMove> {
        let raw = state.extra.get(key)?;
        serde_json::from_value::<ScaleMove>(raw.clone()).ok()
    }

    /// Legge la [`ScaleHysteresisConfig`] DB-driven trasportata dal produttore in
    /// `extra[SCALE_HYSTERESIS_CFG_KEY]` (le 5 soglie `agent.scale.*` del gate
    /// anti-oscillazione). Se assente o malformata (guasto a monte / produttore che
    /// non l'ha scritta) ricade sui seed conservativi mig 0516: non e' un
    /// magic-fallback di modello (regola G), solo la scelta deterministica dei
    /// gate quando la config non e' arrivata. Sul percorso reale il detector la
    /// trasporta sempre col valore DB, cosi' `agent.scale.downscale_enabled=true`
    /// e le altre soglie raggiungono il punto di enforcement.
    fn read_hysteresis_cfg(state: &AgentState) -> ScaleHysteresisConfig {
        state
            .extra
            .get(SCALE_HYSTERESIS_CFG_KEY)
            .and_then(|v| serde_json::from_value::<ScaleHysteresisConfig>(v.clone()).ok())
            .unwrap_or_else(ScaleHysteresisConfig::conservative_defaults)
    }

    /// Legge la [`ScaleSizingConfig`] DB-driven trasportata dal produttore in
    /// `extra[SCALE_SIZING_CFG_KEY]` (kill-switch nested + soglie del sizing). Assente
    /// o malformata -> fallback conservativo (sizing OFF): il gate degrada ogni
    /// `AdjustSizing` a `KeepTier`. Non e' un magic-fallback (regola G): sceglie solo
    /// il gate quando la config non e' arrivata.
    fn read_sizing_cfg(state: &AgentState) -> ScaleSizingConfig {
        state
            .extra
            .get(SCALE_SIZING_CFG_KEY)
            .and_then(|v| serde_json::from_value::<ScaleSizingConfig>(v.clone()).ok())
            .unwrap_or_else(ScaleSizingConfig::conservative_defaults)
    }

    /// Determina la chiave-cache dello scale-move. Preferisce la chiave TRASPORTATA
    /// dal produttore in `extra[SCALE_MOVE_CACHE_KEY_KEY]` (calcolata col punto
    /// unico `scale_cache_key` e l'`eval_every_iters` reale). Se assente, ricalcola
    /// con [`fallback_cache_key`] (default conservativo mig 0516): il nodo non legge
    /// il setting, quindi il fallback e' l'unica opzione deterministica senza I/O.
    fn cache_key(state: &AgentState, ctx: &ScaleContext) -> String {
        state
            .extra
            .get(SCALE_MOVE_CACHE_KEY_KEY)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| fallback_cache_key(ctx))
    }
}

/// Chiave-cache di fallback quando il produttore non ha trasportato la chiave.
/// Usa il default conservativo `eval_every_iters` della mig 0516 (documentato in
/// [`crate::decisions::scale_reason::ScaleTriggerConfig::conservative_defaults`]):
/// il nodo non legge i settings, quindi qui non c'e' un magic-fallback di MODELLO
/// (regola G), solo la scelta deterministica della CHIAVE (idempotenza replay).
fn fallback_cache_key(ctx: &ScaleContext) -> String {
    // `conservative_defaults().eval_every_iters` = 4 (seed mig 0516). Il fallback
    // scatta solo se il produttore non ha trasportato la chiave (guasto a monte);
    // sul percorso reale la chiave arriva sempre gia' calcolata col valore DB.
    scale_cache_key(
        ctx,
        crate::decisions::scale_reason::ScaleTriggerConfig::conservative_defaults()
            .eval_every_iters,
    )
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for ScaleControlNode {
    fn id(&self) -> NodeId {
        NodeId::ScaleControl
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // (1) Contesto strutturato dell'executor. Assente/malformato -> degrado
        // sicuro (risolvi senza mossa): il rientro mantiene lo sticky corrente.
        let scale_ctx = match Self::read_context(state) {
            Some(c) => c,
            None => {
                tracing::warn!(
                    target: "nexus_agent_graph::scale_control",
                    "scale_control: ScaleContext assente in extra, degrado a resolved-only"
                );
                return Ok(Self::resolved_only());
            }
        };
        let key = Self::cache_key(state, &scale_ctx);

        // (2) Cache-hit: mossa gia' decisa per questa chiave -> riusa senza LLM
        // (idempotenza + replay: la mossa e' gia' nello stato checkpointato). Non
        // serve rileggerne il valore qui (l'executor la consumera' dall'extra al
        // rientro): basta constatarne la presenza e risolvere.
        if Self::cached_move(state, &key).is_some() {
            tracing::debug!(
                target: "nexus_agent_graph::scale_control",
                current_tier = scale_ctx.current_tier.as_str(),
                "scale_control: cache-hit, riuso ScaleMove senza LLM"
            );
            return Ok(Self::resolved_only());
        }

        // (3) Cache-miss: UNA sola LLM-call via la porta. `mode` deriva dal flag
        // shadow (punto unico, regola L): in Replay l'adapter (opzione A) ritorna
        // Ok(None) IMMEDIATO senza I/O -> qui degradiamo a resolved-only (parita'
        // shadow). A flag OFF / stub questo ramo ritorna comunque Ok(None).
        let raw_move = match self
            .reasoner
            .assess_scale(scale_ctx.clone(), ctx.exec_mode())
            .await
        {
            // Mossa proposta: valida col PUNTO UNICO (enum CHIUSO + confidence in
            // [0,1]) e applica l'anti-oscillazione (5 gate deterministici; l'LLM NON
            // li scavalca). `KeepTier` post-gate -> resolved-only (nessun cambio).
            Ok(Some(raw)) => raw,
            // Nessuna mossa (kill-switch `agent.scale.enabled` OFF / purpose
            // NotFound / stub inerte / Replay opzione A): degrado LEGITTIMO (mantieni
            // il tier corrente). Non e' un errore (regola G): e' il comportamento a
            // flag OFF.
            Ok(None) => {
                tracing::debug!(
                    target: "nexus_agent_graph::scale_control",
                    current_tier = scale_ctx.current_tier.as_str(),
                    "scale_control: reasoner Ok(None) (inerte/OFF/replay), mantengo il tier"
                );
                return Ok(Self::resolved_only());
            }
            // Errore di porta (provider indisponibile / DB-down / ReplayMissing): NON
            // abortiamo il run (lo scale e' best-effort pre-crisi, lo sticky corrente
            // e' la rete di sicurezza). Loggato come WARN.
            Err(err) => {
                tracing::warn!(
                    target: "nexus_agent_graph::scale_control",
                    current_tier = scale_ctx.current_tier.as_str(),
                    error = %err,
                    "scale_control: porta reasoner in errore, mantengo il tier corrente"
                );
                return Ok(Self::resolved_only());
            }
        };

        // Validazione + anti-oscillazione (PUNTI UNICI scale_reason, regola L).
        // Config anti-oscillazione DB-driven (FIX-B/F2/F3): le 5 soglie
        // `agent.scale.*` arrivano dal produttore executor (che le legge dal DB in
        // `ExecutorConfig.scale`) via `extra[SCALE_HYSTERESIS_CFG_KEY]` — cosi'
        // `downscale_enabled`, `min_confidence`, `change_cooldown_turns`,
        // `downscale_clean_window`, `max_reversals` raggiungono il gate reale (prima
        // erano mute: il nodo usava `conservative_defaults()` hardcoded, viola G).
        // Fallback ai seed mig 0516 solo se il trasporto manca (guasto a monte).
        let validated = validate_scale_move(&raw_move_to_value(&raw_move));
        // Instrada per DIREZIONE (regola L): il SIZING (AdjustSizing) passa dal gate
        // dedicato `apply_sizing_gate` (kill-switch nested + confidenza + cooldown
        // anti-thrash), NON dai 5 gate TIER di `apply_hysteresis`. I tier
        // (Upscale/Downscale/KeepTier) passano dall'anti-oscillazione.
        let is_sizing = matches!(validated, ScaleMove::AdjustSizing { .. });
        let mv = if is_sizing {
            let sizing_cfg = Self::read_sizing_cfg(state);
            apply_sizing_gate(validated, &scale_ctx, &sizing_cfg)
        } else {
            let hyst_cfg = Self::read_hysteresis_cfg(state);
            apply_hysteresis(validated, &scale_ctx, &hyst_cfg)
        };
        if mv == ScaleMove::KeepTier {
            tracing::debug!(
                target: "nexus_agent_graph::scale_control",
                current_tier = scale_ctx.current_tier.as_str(),
                "scale_control: mossa non valida o gate non superato (KeepTier), mantengo il tier"
            );
            return Ok(Self::resolved_only());
        }

        // (4)+(5) Persisti la mossa validata (clone-whole-map) e risolvi -> l'executor
        // la consumera' al rientro (PR-B3).
        tracing::info!(
            target: "nexus_agent_graph::scale_control",
            current_tier = scale_ctx.current_tier.as_str(),
            "scale_control: ScaleMove decisa e persistita"
        );
        Ok(Self::persisted(state, &key, &mv))
    }
}

/// Serializza una [`ScaleMove`] in JSON per la ri-validazione col punto unico
/// [`validate_scale_move`]. `ScaleMove` serializza sempre a un oggetto (enum
/// taggato): in caso improbabile di errore ritorna `Null`, che
/// [`validate_scale_move`] degrada a `KeepTier` (rete di sicurezza, mai un panico).
fn raw_move_to_value(mv: &ScaleMove) -> serde_json::Value {
    serde_json::to_value(mv).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use uuid::Uuid;

    use crate::routing::config::RoutingConfig;
    use crate::runtime::ports::{ExecMode, PortError};
    use crate::runtime::ports::{OrchestrationContext, OrchestrationMove};
    use crate::runtime::ports::{RecoveryMove, ScaleTier, StallContext, SupervisorContext};
    use crate::runtime::{AgentNodeCtx, NullEventSink, StubMetaReasonerPort};
    use crate::state::AgentState;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;

    /// Porta reasoner che ritorna una `ScaleMove` fissa (per il ramo cache-miss). Il
    /// nodo ScaleControl usa SOLO `assess_scale`; `recover`/`orchestrate` inerti.
    struct FixedScaleReasoner(ScaleMove);

    #[async_trait]
    impl MetaReasonerPort for FixedScaleReasoner {
        async fn recover(
            &self,
            _ctx: StallContext,
            _mode: ExecMode,
        ) -> Result<Option<RecoveryMove>, PortError> {
            Ok(None)
        }

        async fn orchestrate(
            &self,
            _ctx: OrchestrationContext,
            _mode: ExecMode,
        ) -> Result<Option<OrchestrationMove>, PortError> {
            Ok(None)
        }

        async fn assess_scale(
            &self,
            _ctx: ScaleContext,
            _mode: ExecMode,
        ) -> Result<Option<ScaleMove>, PortError> {
            Ok(Some(self.0.clone()))
        }

        async fn supervise(
            &self,
            _ctx: SupervisorContext,
            _mode: ExecMode,
        ) -> Result<Option<crate::decisions::supervisor::SupervisorDecision>, PortError> {
            Ok(Some(crate::decisions::supervisor::SupervisorDecision::Continue))
        }
    }

    /// Porta reasoner che ritorna sempre errore su `assess_scale` (ramo degrado).
    struct FailingScaleReasoner;

    #[async_trait]
    impl MetaReasonerPort for FailingScaleReasoner {
        async fn recover(
            &self,
            _ctx: StallContext,
            _mode: ExecMode,
        ) -> Result<Option<RecoveryMove>, PortError> {
            Ok(None)
        }

        async fn orchestrate(
            &self,
            _ctx: OrchestrationContext,
            _mode: ExecMode,
        ) -> Result<Option<OrchestrationMove>, PortError> {
            Ok(None)
        }

        async fn assess_scale(
            &self,
            _ctx: ScaleContext,
            _mode: ExecMode,
        ) -> Result<Option<ScaleMove>, PortError> {
            Err(PortError::ProviderUnavailable("test".to_string().into()))
        }

        async fn supervise(
            &self,
            _ctx: SupervisorContext,
            _mode: ExecMode,
        ) -> Result<Option<crate::decisions::supervisor::SupervisorDecision>, PortError> {
            Ok(Some(crate::decisions::supervisor::SupervisorDecision::Continue))
        }
    }

    /// Pool lazy: i nodi del percorso testato non toccano il DB (la porta e'
    /// stubata). `connect_lazy` non apre connessioni. Non e' un fallback hardcoded
    /// (regola G): serve solo a soddisfare il tipo `PgPool` del ctx nei test.
    fn lazy_pool() -> sqlx::PgPool {
        PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette davvero")
    }

    /// ToolExecutor no-op per il ctx (mai invocato: il nodo non esegue tool).
    struct StubToolNoop;

    #[async_trait]
    impl crate::runtime::ports::ToolExecutor for StubToolNoop {
        async fn execute(
            &self,
            call: crate::runtime::ports::ToolCall,
            _mode: ExecMode,
        ) -> Result<crate::runtime::ports::ToolOutcome, PortError> {
            Ok(crate::runtime::ports::ToolOutcome {
                tool_call_id: call.id,
                ..Default::default()
            })
        }
    }

    /// Ctx Real minimale: la porta del reasoner ignora `ctx.llm` (gli stub non lo
    /// consultano), ma `run` legge `ctx.exec_mode()` -> serve un ctx valido.
    fn real_ctx() -> AgentNodeCtx {
        let run_id = Uuid::new_v4();
        AgentNodeCtx {
            isolation_available: false,
            db: lazy_pool(),
            llm: Arc::new(crate::runtime::test_doubles::StubLlmGateway::with_text("")),
            tools: Arc::new(StubToolNoop),
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id,
            session_id: Uuid::new_v4(),
            thread_id: run_id,
            shadow: false,
            advisory_gate: None,
        }
    }

    /// Stato con lo `ScaleContext` serializzato in extra (come farebbe l'executor).
    /// `turns_since_change`/`current_tier` scelti perche' l'upscale passi i gate.
    fn state_with_context(current: ScaleTier) -> AgentState {
        let scale_ctx = ScaleContext {
            current_tier: current,
            intent_tier_floor: ScaleTier::Light,
            behavior_mode: "automatic".to_string(),
            iterations: 8,
            iteration_cap: 20,
            tail_headroom: 12,
            turns_since_change: 5,
            requires_tool_use: true,
            ..Default::default()
        };
        let mut extra = serde_json::Map::new();
        extra.insert(
            SCALE_CONTEXT_KEY.to_string(),
            serde_json::to_value(&scale_ctx).expect("serialize ScaleContext"),
        );
        AgentState {
            extra,
            ..Default::default()
        }
    }

    /// Applica il delta opaco ritornato dal nodo a uno stato (per asserire l'esito).
    fn apply(state: &AgentState, delta: OpaqueDelta) -> AgentState {
        use nexus_graph::GraphState as _;
        let mut s = state.clone();
        s.merge(delta);
        s
    }

    #[tokio::test]
    async fn stub_reasoner_e_inerte_risolve_senza_mossa() {
        // Con lo StubMetaReasonerPort (Ok(None)) il nodo NON persiste mossa e
        // ritorna ScaleResolved: comportamento inerte (mantiene il tier).
        let node = ScaleControlNode::new(Arc::new(StubMetaReasonerPort));
        let ctx = real_ctx();
        let state = state_with_context(ScaleTier::Medium);

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);

        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        // Nessuna mossa scritta in extra (solo lo ScaleContext preesistente resta).
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        assert!(after.extra.get(&key).is_none());
    }

    #[tokio::test]
    async fn context_assente_degrada_a_resolved() {
        // Nessuno ScaleContext in extra -> degrado sicuro (resolved-only).
        let node = ScaleControlNode::new(Arc::new(StubMetaReasonerPort));
        let ctx = real_ctx();
        let state = AgentState::default();

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
    }

    #[tokio::test]
    async fn mossa_valida_persistita_in_extra() {
        // Reasoner che propone UpscaleTo Heavy (da Medium, confidence alta): il nodo
        // la valida, applica l'anti-oscillazione (passa: cooldown ok, 1 gradino) e la
        // persiste alla chiave-cache, poi risolve. Il clone-whole-map preserva le
        // altre chiavi.
        // Target adiacente al corrente (Medium->High) per isolare la PERSISTENZA
        // dal gate clamp (Medium->Heavy verrebbe clampato a High).
        let node = ScaleControlNode::new(Arc::new(FixedScaleReasoner(ScaleMove::UpscaleTo {
            tier: ScaleTier::High,
            confidence: 0.9,
        })));
        let ctx = real_ctx();
        let mut state = state_with_context(ScaleTier::Medium);
        // Chiave preesistente nell'extra (deve sopravvivere al clone-whole-map).
        state.extra.insert("auto_escalations".to_string(), json!(2));

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);

        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        let persisted = after
            .extra
            .get(&key)
            .expect("la mossa deve essere persistita");
        let mv: ScaleMove =
            serde_json::from_value(persisted.clone()).expect("mossa deserializzabile");
        assert_eq!(
            mv,
            ScaleMove::UpscaleTo {
                tier: ScaleTier::High,
                confidence: 0.9
            }
        );
        // Il clone-whole-map NON ha azzerato le altre chiavi extra.
        assert_eq!(after.extra.get("auto_escalations"), Some(&json!(2)));
    }

    #[tokio::test]
    async fn mossa_gate_non_superato_non_persiste() {
        // Reasoner che propone UpscaleTo con confidence sotto min_confidence (0.5 <
        // 0.70): l'anti-oscillazione degrada a KeepTier -> il nodo NON persiste e
        // risolve (mantiene il tier).
        let node = ScaleControlNode::new(Arc::new(FixedScaleReasoner(ScaleMove::UpscaleTo {
            tier: ScaleTier::Heavy,
            confidence: 0.5,
        })));
        let ctx = real_ctx();
        let state = state_with_context(ScaleTier::Medium);

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        assert!(
            after.extra.get(&key).is_none(),
            "gate non superato -> niente mossa"
        );
    }

    #[tokio::test]
    async fn cache_hit_riusa_senza_persistere_di_nuovo() {
        // Se la mossa e' GIA' in extra alla chiave-cache, il nodo la riusa (0 LLM) e
        // risolve senza toccare l'extra. Reasoner "fallito": se venisse chiamato il
        // nodo degraderebbe comunque a resolved, ma la cache-hit lo evita.
        let node = ScaleControlNode::new(Arc::new(FailingScaleReasoner));
        let ctx = real_ctx();
        let mut state = state_with_context(ScaleTier::Medium);
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        state.extra.insert(
            key.clone(),
            serde_json::to_value(ScaleMove::UpscaleTo {
                tier: ScaleTier::Heavy,
                confidence: 0.9,
            })
            .expect("serialize mossa cache"),
        );

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        // La mossa in cache e' invariata (il nodo l'ha riusata, non riscritta).
        let mv: ScaleMove =
            serde_json::from_value(after.extra.get(&key).expect("mossa cache presente").clone())
                .expect("mossa cache deserializzabile");
        assert_eq!(
            mv,
            ScaleMove::UpscaleTo {
                tier: ScaleTier::Heavy,
                confidence: 0.9
            }
        );
    }

    #[tokio::test]
    async fn porta_in_errore_degrada_a_resolved() {
        // La porta ritorna Err(ProviderUnavailable): il nodo NON abortisce il run,
        // degrada a resolved-only (lo sticky corrente copre la scala).
        let node = ScaleControlNode::new(Arc::new(FailingScaleReasoner));
        let ctx = real_ctx();
        let state = state_with_context(ScaleTier::Medium);

        let delta = node.run(&state, &ctx).await.expect("run ok (best-effort)");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        assert!(after.extra.get(&key).is_none());
    }

    #[tokio::test]
    async fn chiave_trasportata_ha_precedenza_sul_fallback() {
        // Se il produttore ha trasportato la chiave in extra, il nodo la usa per il
        // cache-hit (regola L: una sola formula, calcolata a monte). Qui la chiave
        // trasportata e' diversa dal fallback e ha una mossa in cache: cache-hit.
        let node = ScaleControlNode::new(Arc::new(FailingScaleReasoner));
        let ctx = real_ctx();
        let mut state = state_with_context(ScaleTier::Medium);
        let transported = "scale::99::0::low::0::0".to_string();
        state.extra.insert(
            SCALE_MOVE_CACHE_KEY_KEY.to_string(),
            json!(transported.clone()),
        );
        state.extra.insert(
            transported.clone(),
            serde_json::to_value(ScaleMove::UpscaleTo {
                tier: ScaleTier::Heavy,
                confidence: 0.9,
            })
            .expect("serialize mossa cache"),
        );

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        // La mossa alla chiave trasportata e' invariata (cache-hit, non ricalcolata).
        assert!(after.extra.get(&transported).is_some());
    }

    /// Stato con lo `ScaleContext` a BANDA PULITA per il downscale (context Low,
    /// streak pulita, zero escalation, progresso reale, cooldown ok): con
    /// `downscale_enabled=true` il gate `apply_hysteresis` deve lasciar passare il
    /// downscale. `current`=Medium, `floor`=Light.
    fn state_downscale_clean() -> AgentState {
        let scale_ctx = ScaleContext {
            current_tier: ScaleTier::Medium,
            intent_tier_floor: ScaleTier::Light,
            behavior_mode: "automatic".to_string(),
            iterations: 8,
            iteration_cap: 60,
            tail_headroom: 52,
            context_pressure: crate::runtime::ports::ContextPressure::Low,
            error_free_streak: 5,
            escalations_done: 0,
            files_modified_delta: 3,
            todos_closed: 2,
            turns_since_change: 5,
            requires_tool_use: true,
            ..Default::default()
        };
        let mut extra = serde_json::Map::new();
        extra.insert(
            SCALE_CONTEXT_KEY.to_string(),
            serde_json::to_value(&scale_ctx).expect("serialize ScaleContext"),
        );
        AgentState {
            extra,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn downscale_passa_col_gate_config_db_downscale_enabled() {
        // FIX-B (F2/F3): il nodo usa la ScaleHysteresisConfig DB-driven TRASPORTATA in
        // extra, non i default hardcoded. Con `downscale_enabled=true` (config DB) e
        // banda pulita, un DownscaleTo Light dall'LLM PASSA il gate ed e' persistito.
        // Prima del fix il nodo usava `conservative_defaults()` (downscale_enabled=
        // false) e ogni DownscaleTo degradava a KeepTier: la config DB era muta.
        let node = ScaleControlNode::new(Arc::new(FixedScaleReasoner(ScaleMove::DownscaleTo {
            tier: ScaleTier::Light,
            confidence: 0.95,
        })));
        let ctx = real_ctx();
        let mut state = state_downscale_clean();
        // Config DB trasportata dal produttore: downscale ABILITATO.
        let cfg_db = ScaleHysteresisConfig {
            downscale_enabled: true,
            ..ScaleHysteresisConfig::conservative_defaults()
        };
        state.extra.insert(
            SCALE_HYSTERESIS_CFG_KEY.to_string(),
            serde_json::to_value(cfg_db).expect("serialize cfg"),
        );

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        let persisted = after
            .extra
            .get(&key)
            .expect("il downscale DEVE essere persistito con downscale_enabled=true");
        let mv: ScaleMove =
            serde_json::from_value(persisted.clone()).expect("mossa deserializzabile");
        assert_eq!(
            mv,
            ScaleMove::DownscaleTo {
                tier: ScaleTier::Light,
                confidence: 0.95
            },
            "la config DB (downscale_enabled=true) raggiunge il gate reale (FIX-B)"
        );
    }

    #[tokio::test]
    async fn downscale_muore_senza_config_trasportata_fallback_conservativo() {
        // Contro-prova: SENZA la config trasportata (guasto a monte) il nodo ricade
        // sui seed conservativi mig 0516 (downscale_enabled=false) -> il downscale
        // degrada a KeepTier e NON e' persistito. Deterministico, non un magic-fallback
        // (sceglie solo i gate, non il modello, regola G).
        let node = ScaleControlNode::new(Arc::new(FixedScaleReasoner(ScaleMove::DownscaleTo {
            tier: ScaleTier::Light,
            confidence: 0.95,
        })));
        let ctx = real_ctx();
        let state = state_downscale_clean(); // nessuna SCALE_HYSTERESIS_CFG_KEY in extra

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        assert!(
            after.extra.get(&key).is_none(),
            "senza config trasportata il fallback conservativo tiene il downscale OFF"
        );
    }

    #[tokio::test]
    async fn adjust_sizing_persistito_con_sizing_cfg_on() {
        // Reasoner propone AdjustSizing{Compact}; con ScaleSizingConfig ON trasportata
        // e confidenza alta il gate dedicato lascia passare -> il nodo persiste la
        // mossa alla chiave-cache (poi consumata dall'executor).
        let node = ScaleControlNode::new(Arc::new(FixedScaleReasoner(ScaleMove::AdjustSizing {
            posture: crate::runtime::ports::SizingPosture::Compact,
            confidence: 0.9,
        })));
        let ctx = real_ctx();
        let mut state = state_with_context(ScaleTier::Medium);
        let cfg_on = ScaleSizingConfig {
            enabled: true,
            ..ScaleSizingConfig::conservative_defaults()
        };
        state.extra.insert(
            SCALE_SIZING_CFG_KEY.to_string(),
            serde_json::to_value(cfg_on).expect("serialize sizing cfg"),
        );

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        let persisted = after
            .extra
            .get(&key)
            .expect("AdjustSizing deve essere persistito");
        let mv: ScaleMove =
            serde_json::from_value(persisted.clone()).expect("mossa deserializzabile");
        assert_eq!(
            mv,
            ScaleMove::AdjustSizing {
                posture: crate::runtime::ports::SizingPosture::Compact,
                confidence: 0.9
            }
        );
    }

    #[tokio::test]
    async fn adjust_sizing_degrada_senza_sizing_cfg() {
        // Senza ScaleSizingConfig trasportata (sizing OFF di default) il gate degrada
        // l'AdjustSizing a KeepTier -> nessuna mossa persistita (bit-identico anche con
        // scale ON).
        let node = ScaleControlNode::new(Arc::new(FixedScaleReasoner(ScaleMove::AdjustSizing {
            posture: crate::runtime::ports::SizingPosture::Compact,
            confidence: 0.9,
        })));
        let ctx = real_ctx();
        let state = state_with_context(ScaleTier::Medium); // nessuna SCALE_SIZING_CFG_KEY

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::ScaleResolved));
        let scale_ctx = ScaleControlNode::read_context(&state).expect("ctx presente");
        let key = fallback_cache_key(&scale_ctx);
        assert!(
            after.extra.get(&key).is_none(),
            "sizing OFF -> AdjustSizing degrada a KeepTier, niente mossa"
        );
    }
}
