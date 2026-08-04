//! `StallRecoveryNode` — superstep DEDICATO del meta-reasoner di recovery-da-stallo
//! (ADR 0036-style applicato al recovery).
//!
//! ## Perche' un nodo dedicato (non mid-superstep)
//!
//! La proprieta' che rende `verify_profile` replay-safe NON e' "cache per hash":
//! e' che la decisione LLM e' presa e persistita FUORI dal percorso caldo, e il
//! percorso caldo la rilegge. Consultare il reasoner mid-superstep dentro
//! l'executor introdurrebbe una LLM-call non prevista dalla sequenza del run. Per
//! questo la sola LLM-call del reasoner vive qui, in un superstep isolato, dietro
//! [`crate::runtime::ports::MetaReasonerPort`].
//!
//! ## Idempotenza / resume
//!
//!   - **Prima esecuzione**: la porta consulta l'LLM e la [`RecoveryMove`] validata
//!     e' PERSISTITA in `extra["stall_move::…"]` (checkpoint del nodo, punto (4-5)).
//!   - **Resume**: il checkpoint contiene gia' la mossa -> il nodo fa CACHE-HIT
//!     dall'`extra` (0 LLM, punto (2)). Deterministico.
//!   - **Kill-switch OFF**: la porta ritorna `Ok(None)` -> il nodo degrada a
//!     `Fallback` -> gerarchia fissa `progress_controller::decide`.
//!
//! ## Flusso del nodo (`run`)
//!
//! L'executor (blocco successivo del piano) emette
//! `StateDelta{stop_reason=StallReason, extra += StallContext serializzato}`;
//! `route_after_executor` instrada qui. Il nodo:
//!   1. legge lo [`StallContext`] da `extra[`[`STALL_CONTEXT_KEY`]`]` (il segnale
//!      strutturato prodotto dall'executor, regola M);
//!   2. calcola la chiave-cache `stall_move::{axis}::{work_epoch}` (idempotenza/
//!      replay: `work_epoch` avanza solo sui cambi macroscopici, non sulla coda
//!      volatile) e, se una [`RecoveryMove`] e' GIA' in `extra` a quella chiave, la
//!      RIUSA senza chiamare l'LLM (0 token: replay + idempotenza);
//!   3. altrimenti chiama [`MetaReasonerPort::recover`] — UNA sola LLM-call — e
//!      valida col PUNTO UNICO [`crate::decisions::meta_reason::validate_move`];
//!   4. persiste la mossa in `extra` col pattern clone-whole-map
//!      ([`crate::state::put_extra`], regola L: `extra` e' OVERWRITE totale);
//!   5. ritorna `StopReason::StallResolved` -> self-loop rientra nell'executor,
//!      che al rientro consuma la mossa (blocco successivo del piano).
//!
//! ## Quando questo nodo viene raggiunto
//!
//! I detector che lo attivano sono `maybe_stall_reason_delta` e il gemello
//! runaway pre-LLM nell'executor: emettono `StopReason::StallReason` solo quando
//! `agent.stall_recovery.enabled` e' truthy in `settings` (valore nel DB, non un
//! default di compile-time) e il budget per-sessione non e' esaurito. Senza il
//! flag questo nodo non viene raggiunto.
//!
//! Anche quando lo e', se la porta iniettata ritorna `Ok(None)` (kill-switch
//! spento / purpose `NotFound` / stub) il nodo NON persiste alcuna mossa e
//! ritorna comunque `StallResolved`: il rientro nell'executor ricade sulla
//! gerarchia fissa `progress_controller::decide` (rete di sicurezza).
//!
//! Il nodo NON instrada: l'edge `StallRecovery -> Executor` e' dichiarato fuori,
//! in `graph.rs` (self-loop come `G1Escalated -> executor`).

use std::sync::Arc;

use async_trait::async_trait;

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::meta_reason::validate_move;
use crate::runtime::ports::{MetaReasonerPort, RecoveryMove, StallContext};
use crate::runtime::AgentNodeCtx;
use crate::state::{put_extra, AgentState, StateDelta, StopReason};

/// Chiave in `AgentState::extra` sotto cui l'executor serializza lo
/// [`StallContext`] prima di instradare al nodo (PUNTO UNICO, regola L: sia
/// l'executor produttore sia questo nodo consumatore la usano, non due letterali
/// diversi). Il valore e' lo `StallContext` in forma JSON (regola M: segnali
/// strutturati, non prosa).
pub const STALL_CONTEXT_KEY: &str = "stall_context";

/// Costruisce la chiave-cache della [`RecoveryMove`] per un dato asse ed epoca di
/// lavoro (PUNTO UNICO, regola L: la stessa formula per scrivere e per rileggere).
/// La chiave NON include la coda-segnali volatile: `work_epoch` avanza solo sui
/// cambi macroscopici (nuovo todo / escalation / bump floor anti-loop), cosi' lo
/// stesso stallo non riconsulta l'LLM anche se i `tool_result` variano (anti
/// meta-loop + determinismo replay).
pub fn stall_move_key(axis: &str, work_epoch: i64) -> String {
    format!("stall_move::{axis}::{work_epoch}")
}

/// Nodo del meta-reasoner di recovery-da-stallo.
///
/// Riceve la porta [`MetaReasonerPort`] iniettata dal chiamante (mcp-core con
/// l'impl concreta `PgMetaReasonerPort`, un blocco successivo; i test/lo scaffold
/// con [`crate::runtime::StubMetaReasonerPort`], che ritorna `Ok(None)` ->
/// comportamento inerte). Il nodo NON legge il DB ne' risolve il purpose/model:
/// tutto l'I/O e' dietro la porta (inversione di dipendenza, regola L). Il nodo
/// usa SOLO [`MetaReasonerPort::recover`]; il metodo `orchestrate` della stessa
/// porta e' consumato dal nodo/gate di orchestrazione (blocco successivo).
pub struct StallRecoveryNode {
    /// Porta del meta-reasoner (I/O LLM). Con `Ok(None)` il nodo e' inerte.
    reasoner: Arc<dyn MetaReasonerPort>,
}

impl StallRecoveryNode {
    /// Costruisce il nodo con la porta del reasoner iniettata.
    pub fn new(reasoner: Arc<dyn MetaReasonerPort>) -> Self {
        Self { reasoner }
    }

    /// Delta di sola risoluzione (nessuna mossa persistita): rientra nell'executor
    /// via `StallResolved`. Usato quando manca lo [`StallContext`] (guasto di
    /// costruzione a monte), quando il reasoner ritorna `Ok(None)` (kill-switch OFF
    /// / stub), o quando la mossa e' `Fallback` (rete di sicurezza: al rientro
    /// l'executor usa `progress_controller::decide`).
    fn resolved_only() -> OpaqueDelta {
        StateDelta {
            stop_reason: Some(Some(StopReason::StallResolved)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Delta che persiste `mv` in `extra` alla chiave-cache (clone-whole-map) e
    /// risolve lo stallo. La mossa e' serializzata come JSON (`RecoveryMove`
    /// deriva `Serialize`): il rientro nell'executor la rileggera' e la consumera'
    /// (blocco successivo). PUNTO UNICO della scrittura extra: `put_extra` clona
    /// l'intera mappa e non azzera gli altri canali (`auto_escalations`,
    /// `repeat_scan_floor`, ...).
    fn persisted(state: &AgentState, key: &str, mv: &RecoveryMove) -> OpaqueDelta {
        // `RecoveryMove` serializza sempre a un oggetto JSON (enum taggato):
        // in caso improbabile di errore di serializzazione, degradiamo a
        // `resolved_only` (nessuna mossa) invece di panicare (regola errori).
        let value = match serde_json::to_value(mv) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "nexus_agent_graph::stall_recovery",
                    error = %err,
                    "stall_recovery: serializzazione RecoveryMove fallita, degrado a resolved-only"
                );
                return Self::resolved_only();
            }
        };
        let extra = put_extra(state, key, value);
        StateDelta {
            extra: Some(extra),
            stop_reason: Some(Some(StopReason::StallResolved)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Estrae lo [`StallContext`] serializzato da `extra[STALL_CONTEXT_KEY]`.
    /// `None` se assente o non deserializzabile (guasto di costruzione a monte:
    /// il nodo degrada a `resolved_only`, mai un panico).
    fn read_context(state: &AgentState) -> Option<StallContext> {
        let raw = state.extra.get(STALL_CONTEXT_KEY)?;
        serde_json::from_value::<StallContext>(raw.clone()).ok()
    }

    /// Rilegge una [`RecoveryMove`] gia' persistita in `extra` alla chiave-cache
    /// (cache-hit / replay). `None` se assente o non deserializzabile.
    fn cached_move(state: &AgentState, key: &str) -> Option<RecoveryMove> {
        let raw = state.extra.get(key)?;
        serde_json::from_value::<RecoveryMove>(raw.clone()).ok()
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for StallRecoveryNode {
    fn id(&self) -> NodeId {
        NodeId::StallRecovery
    }

    async fn run(&self, state: &AgentState, _ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // (1) Contesto strutturato dell'executor. Assente/malformato -> degrado
        // sicuro (risolvi senza mossa): il rientro usa la gerarchia fissa.
        let stall = match Self::read_context(state) {
            Some(c) => c,
            None => {
                tracing::warn!(
                    target: "nexus_agent_graph::stall_recovery",
                    "stall_recovery: StallContext assente in extra, degrado a resolved-only"
                );
                return Ok(Self::resolved_only());
            }
        };
        let key = stall_move_key(&stall.axis, stall.work_epoch);

        // (2) Cache-hit: mossa gia' decisa per questo (asse, epoca) -> riusa senza
        // LLM (idempotenza + replay: la mossa e' gia' nello stato checkpointato).
        // Non serve rileggerne il valore qui (l'executor la consumera' dall'extra al
        // rientro): basta constatarne la presenza e risolvere.
        if Self::cached_move(state, &key).is_some() {
            tracing::debug!(
                target: "nexus_agent_graph::stall_recovery",
                axis = %stall.axis,
                work_epoch = stall.work_epoch,
                "stall_recovery: cache-hit, riuso RecoveryMove senza LLM"
            );
            // Rientro senza riscrivere l'extra (la mossa e' gia' li'): risolvi.
            return Ok(Self::resolved_only());
        }

        // (3) Cache-miss: UNA sola LLM-call via la porta (che usa ctx.llm). A flag
        // OFF/stub la porta ritorna Ok(None) e questo ramo degrada alla gerarchia
        // fissa.
        let mv = match self.reasoner.recover(stall.clone()).await {
            // Mossa proposta: valida col PUNTO UNICO (enum chiuso + blocker ADR
            // 0034); qualunque forma sospetta e' gia' degradata a Fallback
            // dall'impl, ma ri-validiamo qui per robustezza (idempotente).
            Ok(Some(raw_move)) => match validate_move(&serialize_move(&raw_move)) {
                RecoveryMove::Fallback => {
                    tracing::debug!(
                        target: "nexus_agent_graph::stall_recovery",
                        axis = %stall.axis,
                        "stall_recovery: mossa non valida (Fallback), degrado alla gerarchia fissa"
                    );
                    return Ok(Self::resolved_only());
                }
                valid => valid,
            },
            // Nessuna mossa (kill-switch OFF / purpose NotFound / stub inerte):
            // degrado LEGITTIMO alla gerarchia fissa (opt-in, regola G: NON e' un
            // errore, e' il comportamento a flag OFF).
            Ok(None) => {
                tracing::debug!(
                    target: "nexus_agent_graph::stall_recovery",
                    axis = %stall.axis,
                    "stall_recovery: reasoner Ok(None) (inerte/OFF), degrado alla gerarchia fissa"
                );
                return Ok(Self::resolved_only());
            }
            // Errore di porta (provider indisponibile / DB-down): NON abortiamo il
            // run (il recovery e' best-effort, la rete di sicurezza `pc::decide`
            // copre lo stallo). Loggato come WARN.
            Err(err) => {
                tracing::warn!(
                    target: "nexus_agent_graph::stall_recovery",
                    axis = %stall.axis,
                    error = %err,
                    "stall_recovery: porta reasoner in errore, degrado alla gerarchia fissa"
                );
                return Ok(Self::resolved_only());
            }
        };

        // (4)+(5) Persisti la mossa validata (clone-whole-map) e risolvi -> l'executor
        // la consumera' al rientro (blocco successivo del piano).
        tracing::info!(
            target: "nexus_agent_graph::stall_recovery",
            axis = %stall.axis,
            work_epoch = stall.work_epoch,
            "stall_recovery: RecoveryMove decisa e persistita"
        );
        Ok(Self::persisted(state, &key, &mv))
    }
}

/// Serializza una [`RecoveryMove`] in JSON per la ri-validazione col punto unico
/// [`validate_move`]. `RecoveryMove` serializza sempre a un oggetto (enum
/// taggato): in caso improbabile di errore ritorna `Null`, che `validate_move`
/// degrada a `Fallback` (rete di sicurezza, mai un panico).
fn serialize_move(mv: &RecoveryMove) -> serde_json::Value {
    serde_json::to_value(mv).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use uuid::Uuid;

    use crate::routing::config::RoutingConfig;
    use crate::runtime::ports::PortError;
    use crate::runtime::ports::{OrchestrationContext, OrchestrationMove};
    use crate::runtime::ports::{ScaleContext, ScaleMove, SupervisorContext};
    use crate::runtime::{AgentNodeCtx, NullEventSink, StubMetaReasonerPort};
    use crate::state::AgentState;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;

    /// Porta reasoner che ritorna una mossa fissa (per il ramo cache-miss). Il
    /// nodo StallRecovery usa SOLO `recover`; `orchestrate` e' inerte (fuori scope).
    struct FixedReasoner(RecoveryMove);

    #[async_trait]
    impl MetaReasonerPort for FixedReasoner {
        async fn recover(
            &self,
            _ctx: StallContext,
        ) -> Result<Option<RecoveryMove>, PortError> {
            Ok(Some(self.0.clone()))
        }

        async fn orchestrate(
            &self,
            _ctx: OrchestrationContext,
        ) -> Result<Option<OrchestrationMove>, PortError> {
            Ok(None)
        }

        async fn assess_scale(
            &self,
            _ctx: ScaleContext,
        ) -> Result<Option<ScaleMove>, PortError> {
            Ok(None)
        }

        async fn supervise(
            &self,
            _ctx: SupervisorContext,
        ) -> Result<Option<crate::decisions::supervisor::SupervisorDecision>, PortError> {
            Ok(Some(crate::decisions::supervisor::SupervisorDecision::Continue))
        }
    }

    /// Porta reasoner che ritorna sempre errore (ramo degrado). `orchestrate` e
    /// `assess_scale` inerti (il nodo StallRecovery non li consulta).
    struct FailingReasoner;

    #[async_trait]
    impl MetaReasonerPort for FailingReasoner {
        async fn recover(
            &self,
            _ctx: StallContext,
        ) -> Result<Option<RecoveryMove>, PortError> {
            Err(PortError::ProviderUnavailable("test".to_string().into()))
        }

        async fn orchestrate(
            &self,
            _ctx: OrchestrationContext,
        ) -> Result<Option<OrchestrationMove>, PortError> {
            Ok(None)
        }

        async fn assess_scale(
            &self,
            _ctx: ScaleContext,
        ) -> Result<Option<ScaleMove>, PortError> {
            Ok(None)
        }

        async fn supervise(
            &self,
            _ctx: SupervisorContext,
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

    /// Ctx minimale: la porta del reasoner ignora `ctx.llm` (gli stub non lo
    /// consultano), ma `run` richiede un ctx valido.
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
            advisory_gate: None,
        step_gate: None,
        }
    }

    /// ToolExecutor no-op per il ctx (mai invocato: il nodo non esegue tool).
    struct StubToolNoop;

    #[async_trait]
    impl crate::runtime::ports::ToolExecutor for StubToolNoop {
        async fn execute(
            &self,
            call: crate::runtime::ports::ToolCall,
        ) -> Result<crate::runtime::ports::ToolOutcome, PortError> {
            Ok(crate::runtime::ports::ToolOutcome {
                tool_call_id: call.id,
                ..Default::default()
            })
        }
    }

    /// Stato con lo `StallContext` serializzato in extra (come farebbe l'executor).
    fn state_with_context(ctx_axis: &str, work_epoch: i64) -> AgentState {
        let stall = StallContext {
            axis: ctx_axis.to_string(),
            work_epoch,
            ..Default::default()
        };
        let mut extra = serde_json::Map::new();
        extra.insert(
            STALL_CONTEXT_KEY.to_string(),
            serde_json::to_value(&stall).expect("serialize StallContext"),
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
        // ritorna StallResolved: comportamento inerte (rete di sicurezza).
        let node = StallRecoveryNode::new(Arc::new(StubMetaReasonerPort));
        let ctx = real_ctx();
        let state = state_with_context("signature", 3);

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);

        assert_eq!(after.stop_reason, Some(StopReason::StallResolved));
        // Nessuna mossa scritta in extra (solo lo StallContext preesistente resta).
        assert!(after.extra.get(&stall_move_key("signature", 3)).is_none());
    }

    #[tokio::test]
    async fn context_assente_degrada_a_resolved() {
        // Nessuno StallContext in extra -> degrado sicuro (resolved-only).
        let node = StallRecoveryNode::new(Arc::new(StubMetaReasonerPort));
        let ctx = real_ctx();
        let state = AgentState::default();

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::StallResolved));
    }

    #[tokio::test]
    async fn mossa_valida_persistita_in_extra() {
        // Reasoner che propone EscalateModel: il nodo la valida e la persiste alla
        // chiave-cache, poi risolve. Il clone-whole-map preserva le altre chiavi.
        let node = StallRecoveryNode::new(Arc::new(FixedReasoner(RecoveryMove::EscalateModel)));
        let ctx = real_ctx();
        let mut state = state_with_context("signature", 5);
        // Chiave preesistente nell'extra (deve sopravvivere al clone-whole-map).
        state.extra.insert("auto_escalations".to_string(), json!(2));

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);

        assert_eq!(after.stop_reason, Some(StopReason::StallResolved));
        let persisted = after
            .extra
            .get(&stall_move_key("signature", 5))
            .expect("la mossa deve essere persistita");
        let mv: RecoveryMove =
            serde_json::from_value(persisted.clone()).expect("mossa deserializzabile");
        assert_eq!(mv, RecoveryMove::EscalateModel);
        // Il clone-whole-map NON ha azzerato le altre chiavi extra.
        assert_eq!(after.extra.get("auto_escalations"), Some(&json!(2)));
    }

    #[tokio::test]
    async fn cache_hit_riusa_senza_persistere_di_nuovo() {
        // Se la mossa e' GIA' in extra alla chiave-cache, il nodo la riusa (0 LLM)
        // e risolve senza toccare l'extra. Reasoner "fallito": se venisse chiamato
        // il nodo degraderebbe comunque a resolved, ma la cache-hit lo evita.
        let node = StallRecoveryNode::new(Arc::new(FailingReasoner));
        let ctx = real_ctx();
        let mut state = state_with_context("repeated_action", 7);
        state.extra.insert(
            stall_move_key("repeated_action", 7),
            serde_json::to_value(RecoveryMove::AskUser {
                question: "email?".to_string(),
            })
            .expect("serialize mossa cache"),
        );

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::StallResolved));
        // La mossa in cache e' invariata (il nodo l'ha riusata, non riscritta).
        let mv: RecoveryMove = serde_json::from_value(
            after
                .extra
                .get(&stall_move_key("repeated_action", 7))
                .expect("mossa cache presente")
                .clone(),
        )
        .expect("mossa cache deserializzabile");
        assert_eq!(
            mv,
            RecoveryMove::AskUser {
                question: "email?".to_string()
            }
        );
    }

    #[tokio::test]
    async fn porta_in_errore_degrada_a_resolved() {
        // La porta ritorna Err(ProviderUnavailable): il nodo NON abortisce il run,
        // degrada a resolved-only (la gerarchia fissa copre lo stallo).
        let node = StallRecoveryNode::new(Arc::new(FailingReasoner));
        let ctx = real_ctx();
        let state = state_with_context("exploration", 1);

        let delta = node.run(&state, &ctx).await.expect("run ok (best-effort)");
        let after = apply(&state, delta);
        assert_eq!(after.stop_reason, Some(StopReason::StallResolved));
        assert!(after.extra.get(&stall_move_key("exploration", 1)).is_none());
    }
}
