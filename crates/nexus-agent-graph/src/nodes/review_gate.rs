//! Nodo REVIEW GATE: la review adversariale entra nel funnel di chiusura.
//!
//! Prima la review era post-processing del finalizzatore mcp-core: girava DOPO
//! che il grafo aveva raggiunto `End`, quindi su bocciatura poteva solo mutare
//! l'esito in memoria (nota nel resoconto + `review_panel_rejected`) senza
//! alcuna possibilita' di correzione — il resume di un run a `End` e' un no-op
//! per costruzione (`engine.rs`: `current == End -> Completed`). L'utente lo ha
//! chiesto due volte: "se non superata dovrebbe tentare di sistemare".
//!
//! Da nodo del grafo, la bocciatura usa ESATTAMENTE il meccanismo del ramo FAIL
//! del final_gate (regola L: stesso rientro, secondo chiamante): messaggio
//! Human col verdetto e i findings + `StopReason::ToolUse` +
//! `pending_tool_uses` azzerato, e l'edge condizionale rimanda all'Executor.
//! Il run non arriva mai a `End` con una bocciatura correggibile pendente.
//!
//! Anti-loop: contatore DEDICATO `review_cycle` (mai `final_gate_cycle`: il
//! residuo di un contatore altrui ha gia' prodotto un falso `FailedDiagnosed`,
//! vedi doc di `FinalGateVerdict`), cap `orchestrator.review_max_correction_cycles`
//! (DB-driven, regola G, risolto a monte). Al cap la bocciatura diventa
//! DEFINITIVA (`RejectedFinal`) e il run chiude bocciato — mai un loop.

use async_trait::async_trait;
use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::PanelOutcome;
use crate::runtime::ports::{
    ReviewPanelReport, ReviewPanelRequest, ReviewSkipReason,
};
use crate::state::{AgentState, Message, MessageContent, ReviewGateVerdict, StopReason};
use crate::state::delta::StateDelta;
use crate::AgentNodeCtx;

/// Config DB-driven del gate (regola G: risolta dal chiamante, mai letta qui).
#[derive(Debug, Clone)]
pub struct ReviewGateConfig {
    /// `orchestrator.review_panel_autoconvene_enabled` (default true). OFF ->
    /// pass-through.
    pub enabled: bool,
    /// `orchestrator.review_max_correction_cycles` (default 1): numero massimo
    /// di RIMANDI in correzione. I panel convocati sono al piu' N+1 (la
    /// ri-review dopo l'ultima correzione).
    pub max_cycles: i64,
}

pub struct ReviewGateNode {
    cfg: ReviewGateConfig,
    /// Porta del panel (mcp-core convoca i sub-run revisori).
    panel: std::sync::Arc<dyn crate::runtime::ports::ReviewPanelPort>,
    /// Narrazione live (pattern emit+persist, punto unico `emit_phase_meta`).
    meta_steps: std::sync::Arc<dyn crate::runtime::ports::MetaStepStore>,
}

impl ReviewGateNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante.
    pub fn new(
        cfg: ReviewGateConfig,
        panel: std::sync::Arc<dyn crate::runtime::ports::ReviewPanelPort>,
        meta_steps: std::sync::Arc<dyn crate::runtime::ports::MetaStepStore>,
    ) -> Self {
        Self {
            cfg,
            panel,
            meta_steps,
        }
    }

    /// Emissione narrativa del gate (punto unico del kind e del payload:
    /// i letterali "review_gate"/"verdict" vivono solo qui).
    async fn emit(
        &self,
        ctx: &AgentNodeCtx,
        title: String,
        mut payload: serde_json::Map<String, serde_json::Value>,
        verdict: Option<&str>,
    ) {
        if let Some(v) = verdict {
            payload.insert("verdict".to_string(), serde_json::Value::String(v.to_string()));
        }
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            "review_gate",
            title,
            serde_json::Value::Object(payload),
        )
        .await;
    }

    fn pass_through() -> OpaqueDelta {
        StateDelta::default().into_opaque()
    }

    /// Delta di solo verdetto (nessun rimando): il run prosegue verso la
    /// chiusura, l'esito strutturato resta leggibile dal finalizzatore.
    fn verdict_delta(cycle: Option<i64>, verdict: ReviewGateVerdict) -> OpaqueDelta {
        StateDelta {
            review_gate_cycle: cycle.map(Some),
            review_gate_verdict: Some(Some(verdict)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Salva l'esito del panel nello stato (`extra.review_panel_last`) per il
    /// titolo onesto del resoconto lato finalizzatore. `put_extra` (punto
    /// unico): il delta `extra` ha semantica overwrite TOTALE, una mappa
    /// parziale cancellerebbe le altre chiavi dello schema aperto.
    fn extra_with_panel(
        state: &AgentState,
        panel: &PanelOutcome,
    ) -> serde_json::Map<String, serde_json::Value> {
        crate::state::delta::put_extra(state, "review_panel_last", panel.to_value())
    }

    /// Blocco di correzione iniettato come messaggio Human (gemello del
    /// `render_failed_block` del final_gate): verdetto + findings con file ed
    /// evidenza, e la consegna esplicita di correggere e ridichiarare.
    fn render_correction_block(panel: &PanelOutcome, cycle: i64, max_cycles: i64) -> String {
        let mut lines = vec![format!(
            "## Review adversariale NON superata (tentativo {cycle}/{max_cycles})\n\n\
             Un panel di revisori indipendenti ha esaminato le modifiche di questo run \
             e ha emesso verdetto '{}' ({} voti validi su {}).\n\nDifetti rilevati:",
            panel.verdict.as_str(),
            panel.valid,
            panel.total_reviews
        )];
        for f in panel.findings.iter().take(12) {
            let file = f.get("file").and_then(|v| v.as_str()).unwrap_or("?");
            let severity = f.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
            let desc = f
                .get("description")
                .or_else(|| f.get("evidence"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            lines.push(format!("- [{severity}] {file}: {desc}"));
        }
        if panel.findings.len() > 12 {
            lines.push(format!("- (altri {} findings omessi)", panel.findings.len() - 12));
        }
        lines.push(
            "\nCORREGGI i difetti elencati usando i tool disponibili, poi dichiara di nuovo \
             la chiusura con task_complete. La review verra' ripetuta sulle modifiche."
                .to_string(),
        );
        lines.join("\n")
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for ReviewGateNode {
    fn id(&self) -> NodeId {
        NodeId::ReviewGate
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        if !self.cfg.enabled {
            return Ok(Self::pass_through());
        }
        // Solo su una chiusura dichiarata come RIUSCITA: rivedere un lavoro
        // dichiarato incompleto (blocked/needs_input/partial) e' rumore; un run
        // gia' bocciato dal final_gate sta gia' chiudendo fallito.
        let declared = crate::routing::declared_outcome_kind(state);
        if matches!(
            declared.as_deref(),
            Some("blocked") | Some("needs_input") | Some("partial")
        ) || state.final_gate_passed == Some(false)
        {
            return Ok(Self::verdict_delta(None, ReviewGateVerdict::NotApplicable));
        }

        // GUARD ANTI-LOOP: se il run e' GIA' stato bocciato in modo DEFINITIVO
        // (RejectedFinal = cycle > max_cycles gia' raggiunto), NON ri-convocare il
        // panel. Senza, ogni re-ingresso nel funnel di chiusura (ondate
        // todo-isolation, rientri) ri-convocava i 2 revisori, incrementava
        // `review_gate_cycle` e ri-bocciava -> loop "(4/3), (5/3), ... (N/3)" visto
        // in UI, che brucia token (panel avversario a ogni giro). Il commento di
        // modulo ("DEFINITIVA -> il run chiude bocciato, mai un loop") era l'INTENTO;
        // questo guard lo rende vero: il verdetto resta RejectedFinal, si esce senza
        // nuova spesa.
        if state.review_gate_verdict == Some(ReviewGateVerdict::RejectedFinal) {
            return Ok(Self::pass_through());
        }

        let cycle = state.review_gate_cycle.unwrap_or(0) + 1;
        let max_cycles = self.cfg.max_cycles.max(0);

        let panel = match self.convoca(state, cycle).await {
            Ok(panel) => panel,
            Err(delta) => return Ok(delta),
        };

        if !panel.verdict.rejects() {
            return Ok(self.close_not_rejected(state, ctx, cycle, &panel).await);
        }
        let definitiva = cycle > max_cycles;
        Ok(self
            .boccia(state, ctx, cycle, max_cycles, &panel, definitiva)
            .await)
    }
}

impl ReviewGateNode {
    /// Convoca il panel via porta. `Err(delta)` = esito gia' deciso senza
    /// giudizio (porta in errore -> Unavailable; skip -> NotApplicable, salvo
    /// NoValidVerdict -> Unavailable). Best-effort come il post-processing
    /// storico: un guasto della porta non uccide un run in chiusura, ma l'esito
    /// resta ONESTO, mai un silenzioso "approvato".
    async fn convoca(
        &self,
        state: &AgentState,
        cycle: i64,
    ) -> Result<PanelOutcome, OpaqueDelta> {
        let report = match self
            .panel
            .review(ReviewPanelRequest {
                run_id: state.thread_id.clone().unwrap_or_default(),
                cost_spent_usd: state.total_cost_usd.unwrap_or(0.0),
                cycle,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    target: "nexus_agent_graph::review_gate",
                    error = %e,
                    "review_gate: porta panel in errore, giudizio non disponibile"
                );
                return Err(Self::verdict_delta(None, ReviewGateVerdict::Unavailable));
            }
        };
        match report {
            ReviewPanelReport::Skipped(reason) => {
                let verdict = match reason {
                    ReviewSkipReason::AutoconveneDisabled
                    | ReviewSkipReason::NoCodeChanges
                    | ReviewSkipReason::AlreadyReviewed
                    | ReviewSkipReason::SizedToZero => ReviewGateVerdict::NotApplicable,
                    ReviewSkipReason::NoValidVerdict => ReviewGateVerdict::Unavailable,
                };
                Err(Self::verdict_delta(None, verdict))
            }
            ReviewPanelReport::Convened(panel) => Ok(panel),
        }
    }

    /// Esito non-rifiuto (Approved/Inconclusive): il run chiude, verdetto
    /// registrato per il finalizzatore.
    async fn close_not_rejected(
        &self,
        state: &AgentState,
        ctx: &AgentNodeCtx,
        cycle: i64,
        panel: &PanelOutcome,
    ) -> OpaqueDelta {
        let verdict = if panel.verdict.is_approved() {
            ReviewGateVerdict::Approved
        } else {
            // Inconclusive: quorum non raggiunto, limite infra (mai rifiuto).
            ReviewGateVerdict::Inconclusive
        };
        let mut payload = serde_json::Map::new();
        payload.insert("cycle".into(), cycle.into());
        payload.insert("phase".into(), "closed".into());
        payload.insert("valid".into(), panel.valid.into());
        payload.insert("total".into(), panel.total_reviews.into());
        self.emit(
            ctx,
            format!(
                "Review adversariale: {} ({}/{} voti validi)",
                panel.verdict.as_str(),
                panel.valid,
                panel.total_reviews
            ),
            payload,
            Some(panel.verdict.as_str()),
        )
        .await;
        StateDelta {
            review_gate_cycle: Some(Some(cycle)),
            review_gate_verdict: Some(Some(verdict)),
            extra: Some(Self::extra_with_panel(state, panel)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Bocciatura, nelle sue due nature. `definitiva=true` (cap dei rimandi
    /// raggiunto): il run chiude bocciato, l'edge NON rimanda. `definitiva=false`:
    /// rimando in correzione con lo STESSO meccanismo del ramo FAIL del
    /// final_gate (messaggio Human + ToolUse + pending azzerato), regola L.
    async fn boccia(
        &self,
        state: &AgentState,
        ctx: &AgentNodeCtx,
        cycle: i64,
        max_cycles: i64,
        panel: &PanelOutcome,
        definitiva: bool,
    ) -> OpaqueDelta {
        let mut payload = serde_json::Map::new();
        payload.insert("cycle".into(), cycle.into());
        payload.insert("max_cycles".into(), max_cycles.into());
        let (titolo, phase) = if definitiva {
            (
                format!(
                    "Review NON superata al cap dei tentativi ({}/{}): il run chiude bocciato",
                    cycle - 1,
                    max_cycles
                ),
                "rejected_final",
            )
        } else {
            tracing::info!(
                target: "nexus_agent_graph::review_gate",
                cycle,
                max_cycles,
                "review_gate: bocciata -> re-executor per correzione"
            );
            payload.insert("findings".into(), panel.findings.len().into());
            (
                format!("Review NON superata: rimando in correzione ({cycle}/{max_cycles})"),
                "failed",
            )
        };
        payload.insert("phase".into(), phase.into());
        self.emit(ctx, titolo, payload, Some(panel.verdict.as_str()))
            .await;
        Self::boccia_delta(state, cycle, max_cycles, panel, definitiva)
    }

    /// Delta della bocciatura (puro). Sul rimando: messaggio Human + ToolUse +
    /// pending azzerato -- `Some(Some(vec![]))` e' AZZERA, distinto da None
    /// (no-op): senza, il route dell'executor cadrebbe su tool_dispatch.
    fn boccia_delta(
        state: &AgentState,
        cycle: i64,
        max_cycles: i64,
        panel: &PanelOutcome,
        definitiva: bool,
    ) -> OpaqueDelta {
        let mut delta = StateDelta {
            review_gate_cycle: Some(Some(cycle)),
            review_gate_verdict: Some(Some(if definitiva {
                ReviewGateVerdict::RejectedFinal
            } else {
                ReviewGateVerdict::PendingCorrection
            })),
            extra: Some(Self::extra_with_panel(state, panel)),
            ..Default::default()
        };
        if !definitiva {
            let block = Self::render_correction_block(panel, cycle, max_cycles);
            delta.messages = Some(vec![Message::Human {
                content: MessageContent::text(block),
            }]);
            delta.stop_reason = Some(Some(StopReason::ToolUse));
            delta.pending_tool_uses = Some(Some(vec![]));
        }
        delta.into_opaque()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use serde_json::{json, Value};
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::decisions::{compose_panel_verdict, QuorumPolicy};
    use crate::runtime::test_doubles::{
        NullEventSink, StubLlmGateway, StubMetaStepStore, StubToolExecutor,
    };
    use crate::runtime::AgentNodeCtx;
    use crate::routing::RoutingConfig;

    fn ctx_with() -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            isolation_available: false,
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("non usato")),
            tools: Arc::new(StubToolExecutor::with_success(json!("ok"))),
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            advisory_gate: None,
        }
    }

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    /// Porta stub che ritorna sempre lo stesso report.
    struct StubPanel(ReviewPanelReport);
    #[async_trait::async_trait]
    impl crate::runtime::ports::ReviewPanelPort for StubPanel {
        async fn review(
            &self,
            _req: ReviewPanelRequest,
        ) -> Result<ReviewPanelReport, crate::runtime::ports::PortError> {
            Ok(self.0.clone())
        }
    }

    /// Il verdetto arriva dal PRODUTTORE di produzione (`compose_panel_verdict`,
    /// regola O), a partire dagli outcome nella forma che `run_single_subagent`
    /// mette in `outcome.review` -- mai un PanelOutcome fabbricato a mano.
    fn panel_bocciato() -> PanelOutcome {
        let outcomes: Vec<Value> = vec![json!({
            "success": true,
            "review": {
                "verdict": "fail",
                "findings": [{
                    "file": "backend/server.cjs",
                    "severity": "alta",
                    "description": "request_port non definita: ReferenceError all'avvio",
                }],
            },
        })];
        compose_panel_verdict(
            &outcomes,
            &QuorumPolicy {
                min_valid_verdicts: 1,
                fail_on_high_severity: true,
            },
        )
        .expect("panel di review valido")
    }

    fn panel_approvato() -> PanelOutcome {
        let outcomes: Vec<Value> = vec![json!({
            "success": true,
            "review": { "verdict": "pass", "findings": [] },
        })];
        compose_panel_verdict(
            &outcomes,
            &QuorumPolicy {
                min_valid_verdicts: 1,
                fail_on_high_severity: true,
            },
        )
        .expect("panel valido")
    }

    fn stato_done() -> AgentState {
        AgentState {
            thread_id: Some(Uuid::new_v4().to_string()),
            declared_outcome: Some(json!({"outcome": "done", "summary": "fatto"})),
            ..Default::default()
        }
    }

    fn nodo(max_cycles: i64, report: ReviewPanelReport) -> ReviewGateNode {
        ReviewGateNode::new(
            ReviewGateConfig {
                enabled: true,
                max_cycles,
            },
            Arc::new(StubPanel(report)),
            Arc::new(StubMetaStepStore::default()),
        )
    }

    /// REGRESSIONE (il difetto chiesto due volte dall'utente: "se non superata
    /// dovrebbe tentare di sistemare"): la bocciatura RIMANDA in correzione.
    /// Si asserisce la CONSEGUENZA: l'edge del ReviewGate risolve su Executor,
    /// il verdetto e' PendingCorrection, e il messaggio Human porta il file del
    /// finding (la consegna di correzione e' azionabile).
    #[tokio::test]
    async fn bocciatura_rimanda_in_correzione() {
        let node = nodo(1, ReviewPanelReport::Convened(panel_bocciato()));
        let delta = node
            .run(&stato_done(), &ctx_with())
            .await
            .expect("nodo ok");
        let s = apply(stato_done(), delta);

        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::PendingCorrection));
        assert_eq!(s.review_gate_cycle, Some(1));
        // Il rimando usa il predicato UNICO dei gate: l'edge deve andare a Executor.
        assert!(
            crate::routing::gate_rimanda_in_correzione(&s),
            "stop_reason ToolUse atteso: senza, l'edge chiude su Reflection e la \
             correzione non avviene mai"
        );
        assert_eq!(
            s.pending_tool_uses.as_deref(),
            Some(&[][..]),
            "pending azzerato: senza, il route cade su tool_dispatch"
        );
        let ultimo_human = s
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::Human { content } => Some(content.flatten_text()),
                _ => None,
            })
            .expect("messaggio di correzione presente");
        assert!(
            ultimo_human.contains("backend/server.cjs"),
            "la consegna deve citare il file del finding: {ultimo_human}"
        );
    }

    /// Al cap dei rimandi la bocciatura e' DEFINITIVA: nessun rimando (il run
    /// chiude), verdetto RejectedFinal. E' l'anti-loop.
    #[tokio::test]
    async fn al_cap_la_bocciatura_diventa_definitiva() {
        let node = nodo(1, ReviewPanelReport::Convened(panel_bocciato()));
        let gia_rimandato = AgentState {
            review_gate_cycle: Some(1),
            ..stato_done()
        };
        let delta = node
            .run(&gia_rimandato, &ctx_with())
            .await
            .expect("nodo ok");
        let s = apply(gia_rimandato, delta);

        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::RejectedFinal));
        assert!(
            !crate::routing::gate_rimanda_in_correzione(&s),
            "al cap NON si rimanda: il run deve chiudere (bocciato), mai un loop"
        );
    }

    /// Anti-loop: un run GIA' bocciato in modo definitivo (RejectedFinal) NON
    /// ri-convoca il panel a un nuovo ingresso. Senza il guard, `run` convocherebbe
    /// di nuovo i revisori e incrementerebbe `review_gate_cycle` (4/3, 5/3, ...) ->
    /// il loop visto in UI. Test di mutazione: rimuovendo il guard, il cycle passa
    /// da 5 a 6 e questo assert rosseggia.
    #[tokio::test]
    async fn gia_rejected_final_non_riconvoca() {
        // Panel che boccerebbe SE convocato: il guard deve impedire la convocazione.
        let node = nodo(1, ReviewPanelReport::Convened(panel_bocciato()));
        let gia_definitivo = AgentState {
            review_gate_verdict: Some(ReviewGateVerdict::RejectedFinal),
            review_gate_cycle: Some(5),
            ..stato_done()
        };
        let delta = node
            .run(&gia_definitivo, &ctx_with())
            .await
            .expect("nodo ok");
        let s = apply(gia_definitivo, delta);
        // pass_through: verdetto e cycle INVARIATI (nessuna ri-review, nessuna spesa).
        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::RejectedFinal));
        assert_eq!(
            s.review_gate_cycle,
            Some(5),
            "il cycle NON incrementa: nessuna ri-convocazione del panel"
        );
        assert!(!crate::routing::gate_rimanda_in_correzione(&s));
    }

    /// Approvazione: nessun rimando, verdetto Approved, il run chiude pulito.
    #[tokio::test]
    async fn approvazione_chiude_senza_rimando() {
        let node = nodo(1, ReviewPanelReport::Convened(panel_approvato()));
        let delta = node
            .run(&stato_done(), &ctx_with())
            .await
            .expect("nodo ok");
        let s = apply(stato_done(), delta);
        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::Approved));
        assert!(!crate::routing::gate_rimanda_in_correzione(&s));
    }

    /// Dichiarazione non-done (blocked): il gate non si applica, mai un panel.
    #[tokio::test]
    async fn dichiarazione_blocked_non_convoca() {
        let node = nodo(1, ReviewPanelReport::Convened(panel_bocciato()));
        let blocked = AgentState {
            declared_outcome: Some(json!({"outcome": "blocked", "summary": "x"})),
            ..stato_done()
        };
        let delta = node.run(&blocked, &ctx_with()).await.expect("nodo ok");
        let s = apply(blocked, delta);
        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::NotApplicable));
        assert!(!crate::routing::gate_rimanda_in_correzione(&s));
    }
}
