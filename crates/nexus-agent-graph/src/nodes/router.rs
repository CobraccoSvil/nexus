//! `RouterNode` — porta la parte PORTABILE/deterministica di `router_node`
//! (`brain/agents/nodes/__init__.py:578`).
//!
//! Cosa porta QUESTO PR (deterministico, testato):
//!
//! - **Passthrough `intent_hint`** (`__init__.py:630-641`): se `intent_hint` e'
//!   presente (mcp-core ha gia' risolto l'intent, es. risposta "A"/"B" a una
//!   disambiguazione), si USA quello e si SALTA la classificazione. Replica
//!   esatta: `user_intent = hint`, `intent_confidence = 1.0`,
//!   `is_ambiguous = false`, e — punto unico action-oriented
//!   (`__init__.py:686-689`) — `action_oriented = true` (una disambiguazione
//!   risolta e' per costruzione un turno d'azione).
//! - **Caso "nessun messaggio"** (`__init__.py:589-597`): stato iniziale senza
//!   messaggi -> intent/task_type "chat", behavior_mode preservato (default
//!   "bilanciata"), token_budget 400, iterations += 1.
//! - **Stima token_budget** dal testo dell'ultimo messaggio (`__init__.py:603`).
//!
//! Cosa NON porta (delegato al PR successivo, TODO espliciti sotto):
//!
//! - La **classificazione intent via LLM** (`AgenticIntentClassifier`,
//!   `__init__.py:642-676`): richiede il classifier come porta dedicata o un uso
//!   strutturato di `ctx.llm`. Qui c'e' un TODO che indica la delega; nel caso
//!   generale (nessun hint) il nodo applica il fallback NEUTRO `agentic_default`
//!   identico al ramo "classifier non disponibile" del Python
//!   (`__init__.py:673-676`), cosi' il comportamento e' definito e testabile.
//! - **Profile selection / Q-router** (`__init__.py:787-838`): dipende dal
//!   profile_loader e dal canale gRPC del Q-learning.
//! - **RAG-KB inline** e **escalation per difficolta'** (richiedono i metadati
//!   complexity che vengono dal classifier LLM, quindi seguono il classifier).
//!
//! L'obiettivo del PR e' l'infra + il passthrough funzionante; il router
//! completo arriva dopo. Il nodo NON instrada (l'edge e' fuori, in `edge.rs`).

use async_trait::async_trait;

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, StateDelta};

/// Intent neutro usato quando la classificazione non e' (ancora) disponibile.
/// Identico al fallback Python (`agentic_default` attiva il toolkit minimal
/// cosi' l'agente interpreta e agisce da se'). Niente nome modello qui (regola
/// G): e' un intent semantico, non un modello.
const NEUTRAL_INTENT: &str = "agentic_default";

/// Behavior mode di default quando lo stato non ne porta uno (replica
/// `state.get("behavior_mode", "bilanciata")`). E' un'etichetta di tier
/// neutra, non un modello.
const DEFAULT_BEHAVIOR_MODE: &str = "bilanciata";

/// Floor del token budget (replica `max(400, ...)` del Python).
const TOKEN_BUDGET_FLOOR: i64 = 400;

/// Nodo router del grafo agentico. Stateless: tutta la dipendenza I/O passa dal
/// `AgentNodeCtx` (porte astratte). In questo PR usa solo dati dello stato; il
/// classifier LLM (via `ctx.llm`) e' un TODO delegato (vedi doc del modulo).
pub struct RouterNode;

impl RouterNode {
    /// Testo dell'ultimo messaggio (qualunque ruolo), per la stima budget.
    /// Replica `last_message.content` del Python (`__init__.py:599-600`).
    fn last_message_text(messages: &[Message]) -> String {
        match messages.last() {
            Some(Message::Human { content }) => content.flatten_text(),
            Some(Message::Ai { content, .. }) => content.flatten_text(),
            Some(Message::Tool { content, .. }) => content.flatten_text(),
            None => String::new(),
        }
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for RouterNode {
    fn id(&self) -> NodeId {
        NodeId::Router
    }

    async fn run(&self, state: &AgentState, _ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // ── Caso "nessun messaggio" (__init__.py:589-597) ────────────────────
        // Stato iniziale senza messaggi: routing conversazionale minimale.
        if state.messages.is_empty() {
            let behavior_mode = state
                .behavior_mode
                .clone()
                .unwrap_or_else(|| DEFAULT_BEHAVIOR_MODE.to_string());
            let iterations = state.iterations.unwrap_or(0) + 1;
            return Ok(StateDelta {
                user_intent: Some(Some("chat".to_string())),
                task_type: Some(Some("chat".to_string())),
                behavior_mode: Some(Some(behavior_mode)),
                token_budget: Some(Some(TOKEN_BUDGET_FLOOR)),
                iterations: Some(Some(iterations)),
                ..Default::default()
            }
            .into_opaque());
        }

        // Stima token budget dal testo dell'ultimo messaggio (1 token ~ 4 char,
        // floor 400) — __init__.py:602-603.
        let text = Self::last_message_text(&state.messages);
        let token_budget = TOKEN_BUDGET_FLOOR.max((text.chars().count() / 4) as i64);

        // ── Passthrough intent_hint (__init__.py:630-641) ────────────────────
        // mcp-core ha gia' risolto l'intent (es. risposta a disambiguazione):
        // si usa quello, NIENTE ri-classificazione. action_oriented = true:
        // una disambiguazione risolta e' per costruzione un turno d'azione
        // (__init__.py:686-689, punto unico action-oriented, regola L).
        let hint = state
            .intent_hint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(hint) = hint {
            tracing::info!(
                target: "nexus_agent_graph::router",
                intent = %hint,
                "router: intent_hint da mcp-core (disambiguazione risolta) -> salto la classificazione"
            );
            return Ok(StateDelta {
                user_intent: Some(Some(hint.to_string())),
                intent_confidence: Some(Some(1.0)),
                is_ambiguous: Some(Some(false)),
                action_oriented: Some(Some(true)),
                token_budget: Some(Some(token_budget)),
                ..Default::default()
            }
            .into_opaque());
        }

        // ── Caso generale: classificazione intent (TODO delegato) ────────────
        // TODO(PR successivo): portare la classificazione LLM via
        //   `AgenticIntentClassifier` (__init__.py:642-676). Le opzioni:
        //   (a) una porta dedicata `IntentClassifier` accanto a `LlmGateway`, o
        //   (b) un uso strutturato di `ctx.llm` col prompt classifier dal DB
        //       (nexus_prompt_templates, mai hardcoded — regola G).
        //   Insieme alla classificazione arrivano i metadati complexity/
        //   agentic_score/is_ambiguous che alimentano l'escalation per
        //   difficolta' (__init__.py:760-771) e il gate del planner.
        // TODO(PR successivo): profile selection + Q-router (__init__.py:787-838)
        //   e RAG-KB inline; dipendono dal profile_loader e dal canale gRPC.
        //
        // ── action_oriented: rispetta la pre-derivazione fedele a monte ─────────
        // Il primario Rust pre-deriva `action_oriented` nello stato INIZIALE dai
        // dati completi del classifier del turno (Tappa 1b punto B,
        // `build_initial_state` -> `intent_classifier::derive_action_oriented`,
        // porting 1:1 del primario Python). Quando lo stato lo porta gia', il
        // RouterNode NON lo sovrascrive: cosi' converge col primario Python
        // (niente G1 sui turni conversazionali read-only). Senza dati del
        // classifier `action_oriented` arriva None -> si applica il fallback
        // conservativo sotto, comportamento INVARIATO. `None` nel delta = "non
        // toccare lo stato".
        let action_oriented_delta: Option<Option<bool>> = match state.action_oriented {
            Some(_) => None,          // gia' derivato a monte -> preserva
            None => Some(Some(true)), // fallback NEUTRO conservativo (Python degradato)
        };

        // Intent RISOLTO A MONTE dal classifier mcp-core (`intent_classifier`),
        // propagato nello stato iniziale (native_engine: user_intent = classifier
        // del turno). Quando lo stato lo porta gia', il RouterNode lo PRESERVA
        // (stesso pattern di action_oriented sopra, regola L) invece di forzare il
        // neutro: cosi' `is_eligible` vede l'intent vero (es. scaffold_app) e il
        // gate d'orchestrazione riceve un segnale d'intento reale. Il fallback
        // NEUTRO `agentic_default` (identico al ramo "classifier non disponibile"
        // del Python, __init__.py:673-707) resta solo quando NESSUN intent e'
        // risolto (sub-run/resume): comportamento invariato.
        let user_intent_delta = match state.user_intent.as_deref().map(str::trim) {
            Some(intent) if !intent.is_empty() => {
                tracing::info!(
                    target: "nexus_agent_graph::router",
                    intent,
                    "router: intent dal classifier mcp-core -> preservo (no fallback neutro)"
                );
                None // "non toccare": preserva l'intent gia' nello stato
            }
            _ => {
                tracing::warn!(
                    target: "nexus_agent_graph::router",
                    "router: nessun intent risolto a monte -> intent neutro '{}'",
                    NEUTRAL_INTENT
                );
                Some(Some(NEUTRAL_INTENT.to_string()))
            }
        };
        Ok(StateDelta {
            user_intent: user_intent_delta,
            intent_confidence: Some(Some(0.5)),
            action_oriented: action_oriented_delta,
            token_budget: Some(Some(token_budget)),
            ..Default::default()
        }
        .into_opaque())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::routing::config::RoutingConfig;
    use crate::runtime::test_doubles::{NullEventSink, StubLlmGateway, StubToolExecutor};
    use crate::runtime::AgentNodeCtx;
    use crate::state::{AgentState, Message, MessageContent};

    /// Applica il delta OPACO prodotto dal nodo a uno stato e lo ritorna: cosi'
    /// i test verificano end-to-end `into_opaque` + reducer (punto unico merge).
    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    /// Costruisce un ctx di test senza toccare il DB: il `PgPool` e' creato
    /// LAZY (`connect_lazy`) — il RouterNode di questo PR non esegue query, e
    /// `connect_lazy` non apre connessioni finche' non si interroga il DB.
    fn test_ctx() -> AgentNodeCtx {
        // URL fittizio: `connect_lazy` non si connette, quindi e' innocuo. Non
        // e' un fallback hardcoded di produzione (regola G): e' solo per
        // soddisfare il tipo `PgPool` in un test che NON tocca il DB.
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette davvero");
        AgentNodeCtx {
            isolation_available: false,
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("irrilevante")),
            tools: Arc::new(StubToolExecutor::with_success(serde_json::json!("ok"))),
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            advisory_gate: None,
        step_gate: None,
        }
    }

    fn human(text: &str) -> Message {
        Message::Human {
            content: MessageContent::text(text),
        }
    }

    /// Passthrough: con `intent_hint` presente il nodo NON ri-classifica e
    /// produce esattamente intent=hint, conf=1.0, ambiguous=false, action=true.
    #[tokio::test]
    async fn passthrough_intent_hint_salta_classificazione() {
        let node = RouterNode;
        let ctx = test_ctx();
        let state = AgentState {
            messages: vec![human("A")],
            intent_hint: Some("code_write".to_string()),
            ..Default::default()
        };

        let delta = node.run(&state, &ctx).await.expect("run ok");
        // Il passthrough NON deve riclassificare: nel delta non compare nessuna
        // chiave estranea (solo i 5 campi attesi).
        assert_eq!(delta.as_map().len(), 5, "delta passthrough = 5 chiavi");
        let out = apply(state, delta);

        assert_eq!(
            out.user_intent.as_deref(),
            Some("code_write"),
            "deve usare l'intent_hint senza riclassificare"
        );
        assert_eq!(out.intent_confidence, Some(1.0));
        assert_eq!(out.is_ambiguous, Some(false));
        assert_eq!(
            out.action_oriented,
            Some(true),
            "una disambiguazione risolta e' un turno d'azione"
        );
        // token_budget calcolato: testo "A" (1 char) -> floor 400.
        assert_eq!(out.token_budget, Some(400));
    }

    /// Tappa 1b (B): nel ramo generale, se lo stato porta GIA' `action_oriented`
    /// (pre-derivato a monte dai dati del classifier), il RouterNode NON lo
    /// sovrascrive -> converge col primario Python (niente G1 forzato a true
    /// sui turni read-only). Lo stato iniziale con action_oriented=Some(false)
    /// resta false dopo il router.
    #[tokio::test]
    async fn ramo_generale_preserva_action_oriented_prederivato() {
        let node = RouterNode;
        let ctx = test_ctx();
        let state = AgentState {
            messages: vec![human("riassumi cosa hai fatto")],
            // Nessun intent_hint -> ramo generale; action_oriented pre-derivato.
            action_oriented: Some(false),
            ..Default::default()
        };

        let delta = node.run(&state, &ctx).await.expect("run ok");
        // Il delta NON deve includere action_oriented (None = "non toccare").
        assert!(
            !delta.as_map().contains_key("action_oriented"),
            "ramo generale con action_oriented gia' presente -> non sovrascrive"
        );
        let out = apply(state, delta);
        assert_eq!(
            out.action_oriented,
            Some(false),
            "action_oriented pre-derivato preservato"
        );
    }

    /// Tappa 1b (B): nel ramo generale SENZA action_oriented pre-derivato (caso
    /// primario), il RouterNode applica il fallback conservativo true
    /// (comportamento INVARIATO del primario).
    #[tokio::test]
    async fn ramo_generale_fallback_action_oriented_true() {
        let node = RouterNode;
        let ctx = test_ctx(); // primario
        let state = AgentState {
            messages: vec![human("ciao")],
            action_oriented: None,
            ..Default::default()
        };

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let out = apply(state, delta);
        assert_eq!(
            out.action_oriented,
            Some(true),
            "primario: fallback conservativo true (decide il RouterNode)"
        );
    }

    /// intent_hint vuoto/whitespace NON e' un passthrough valido: cade nel ramo
    /// generale (fallback neutro), come il Python (`.strip()` -> falsy).
    #[tokio::test]
    async fn intent_hint_vuoto_non_e_passthrough() {
        let node = RouterNode;
        let ctx = test_ctx();
        let state = AgentState {
            messages: vec![human("ciao")],
            intent_hint: Some("   ".to_string()),
            ..Default::default()
        };

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let out = apply(state, delta);

        assert_eq!(
            out.user_intent.as_deref(),
            Some(NEUTRAL_INTENT),
            "hint vuoto -> ramo generale con intent neutro"
        );
        assert_eq!(out.intent_confidence, Some(0.5));
    }

    /// Caso "nessun messaggio": routing conversazionale minimale, iterations +1.
    #[tokio::test]
    async fn nessun_messaggio_routing_chat() {
        let node = RouterNode;
        let ctx = test_ctx();
        let state = AgentState {
            iterations: Some(2),
            ..Default::default()
        };

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let out = apply(state, delta);

        assert_eq!(out.user_intent.as_deref(), Some("chat"));
        assert_eq!(out.task_type.as_deref(), Some("chat"));
        assert_eq!(out.behavior_mode.as_deref(), Some(DEFAULT_BEHAVIOR_MODE));
        assert_eq!(out.token_budget, Some(400));
        assert_eq!(out.iterations, Some(3), "iterations += 1");
    }

    /// Stima token_budget: un testo lungo supera il floor di 400.
    #[tokio::test]
    async fn token_budget_stimato_da_testo_lungo() {
        let node = RouterNode;
        let ctx = test_ctx();
        // 2000 caratteri -> 2000/4 = 500 > 400.
        let lungo = "x".repeat(2000);
        let state = AgentState {
            messages: vec![human(&lungo)],
            intent_hint: Some("fix".to_string()),
            ..Default::default()
        };

        let delta = node.run(&state, &ctx).await.expect("run ok");
        let out = apply(state, delta);
        assert_eq!(out.token_budget, Some(500));
    }

}
