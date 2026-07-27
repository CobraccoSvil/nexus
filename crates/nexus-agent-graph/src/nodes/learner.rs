//! `LearnerNode` — porta la parte DETERMINISTICA del `learner_node`
//! (`brain/agents/nodes/__init__.py:4466-4635`).
//!
//! Il learner e' il nodo TERMINALE del grafo agentico: non instrada e produce un
//! delta minimale `{completed_at}`. Il suo unico effetto e' persistere
//! l'interazione in `brain_learning_interactions` / `brain_task_stats` via
//! `ctx.db`, fire-and-forget.
//!
//! ## Cosa faceva e non fa piu' (rimosso, non rimandato)
//!
//! Il learner Python aveva quattro effetti; il porting ne aveva portato uno solo
//! e lasciato gli altri tre come valori calcolati e scartati con `let _`, dietro
//! TODO. Misurando i destinatari si e' visto che non c'era nulla da cablare:
//!
//! - **feedback Q-learning**: `nexus_q_values` e' ferma da settimane,
//!   `feedback_score` e' valorizzato in 0 righe su 2179, e chi la interroga ha
//!   due soli chiamanti non-test (un endpoint gRPC senza client e una rotta di
//!   prova). Il routing reale passa dalla routing matrix, che non la legge. La
//!   sua chiave — `(task_type, AgentType)`, senza provider ne' modello ne' tier —
//!   non sarebbe nemmeno agganciabile al routing senza cambiarne lo schema. Il
//!   trait `RewardSink` che i TODO citavano non e' mai esistito.
//! - **upsert Qdrant**: nessuna collection di interazioni esiste, e lo stesso
//!   contenuto e' gia' indicizzato per due vie con lettori vivi
//!   (`conversation_context`, `chat_history_chunks`). Da notare che qui il
//!   porting aveva REGREDITO una funzione viva: `qdrant_id` risulta popolato
//!   fino a giugno e a zero da luglio.
//! - **closure_judge**: mai portato, nessun codice, flag spenti, nessun
//!   consumatore.

use async_trait::async_trait;
use serde_json::json;

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, StateDelta};

// `LearnerConfig` (auto_extract / min_confidence) e' stata rimossa insieme al
// salvataggio Qdrant che governava: era l'unico suo lettore. Non era comunque
// configurazione viva — i due call site la costruivano sempre con `::default()`
// e non esisteva alcun `load_learner_config` che la leggesse dal DB, a
// differenza di ogni altro nodo del grafo.

// `QdrantPayload` rimosso con il salvataggio che lo produceva. La sua
// `to_json` era gia' marcata `#[cfg(test)]` con la motivazione esplicita che a
// runtime non veniva chiamata: un tipo che esisteva solo per i propri test.

/// Nodo learner. Stateless e senza configurazione: persiste l'interazione in
/// `brain_learning_interactions`/`brain_task_stats` via `ctx.db`. Era l'unico
/// dei suoi effetti davvero eseguito.
#[derive(Default)]
pub struct LearnerNode;

impl LearnerNode {
    pub fn new() -> Self {
        Self
    }

    /// Testo input utente = content del PRIMO HumanMessage dei messages
    /// (`__init__.py:4491-4495`: itera in avanti, primo `HumanMessage`, `break`).
    /// Stringa vuota se non c'e' alcun messaggio umano.
    pub fn user_input(messages: &[Message]) -> String {
        for m in messages {
            if let Message::Human { content } = m {
                return content.flatten_text();
            }
        }
        String::new()
    }

    /// Stringa snake_case dello `stop_reason` per i confronti `== "end_turn"` /
    /// `== "error"` dei reward. Il brain usa `state.get("stop_reason") or
    /// "end_turn"` (`__init__.py:4502,4576`): stop_reason assente -> `"end_turn"`.
    /// L'enum Rust serializza in snake_case (parita' col Python, vedi `state/mod.rs`).
    fn stop_reason_str(state: &AgentState) -> String {
        match state.stop_reason {
            Some(sr) => serde_json::to_value(sr)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "end_turn".to_string()),
            None => "end_turn".to_string(),
        }
    }

    /// `completed_at` ISO8601 UTC (`__init__.py:4488`:
    /// `datetime.datetime.now(timezone.utc).isoformat()`). Reso col formato RFC3339
    /// (equivalente all'isoformat con offset di Python per un datetime UTC aware).
    fn now_completed_at() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for LearnerNode {
    fn id(&self) -> NodeId {
        NodeId::Learner
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // ── Raccolta dati operativi dallo stato (__init__.py:4478-4495) ───────
        let thread_id = state
            .thread_id
            .clone()
            .unwrap_or_else(|| ctx.run_id.to_string());
        let task_type = state
            .user_intent
            .clone()
            .unwrap_or_else(|| "chat".to_string());
        let behavior_mode = state
            .behavior_mode
            .clone()
            .unwrap_or_else(|| "bilanciata".to_string());
        let result = state.result.clone().unwrap_or_default();
        let provider = state.provider_used.clone();
        let model = state.model_used.clone();
        let user_input = Self::user_input(&state.messages);
        let stop_reason = Self::stop_reason_str(state);

        if result.is_empty() {
            tracing::warn!(
                target: "nexus_agent_graph::learner",
                stop = %stop_reason,
                "learner: result vuoto, skip persistenza"
            );
        }

        // Qui si costruiva un payload Qdrant che nessuno upsertava. RIMOSSO
        // invece che cablato, dopo aver misurato: nessuna collection di
        // interazioni esiste su Qdrant (10 censite, nessuna e' quella), e lo
        // stesso contenuto ("Input: ... Output: ...") e' gia' indicizzato per
        // due vie CON lettori vivi — `conversation_context`, scritta a ogni
        // turno, e `chat_history_chunks` via ContextOffload. Cablarlo avrebbe
        // aggiunto un terzo supporto per lo stesso dato, in una collection che
        // nessuno interroga.
        //
        // Il porting Rust aveva pero' REGREDITO una funzione viva: in
        // `brain_learning_interactions` il campo `qdrant_id` risulta popolato a
        // maggio (74 righe) e giugno (430), e a zero da luglio. Non e' un TODO
        // mai fatto: e' una funzione persa. Lo storico e' comunque gia' orfano —
        // quei `qdrant_id` non trovano riscontro in nessuna collection viva.

        // ── Persistenza PostgreSQL (best-effort) ──────────────────────────────
        // brain_learning_interactions (memory/storage.py:35). Gate Python:
        // `_storage is not None and user_input` (__init__.py:4546).
        // Fire-and-forget: errore -> WARN, mai fatale (parita' col Python che
        // logga e non rilancia, __init__.py:4561).
        if !user_input.is_empty() {
            self.spawn_persist_pg(
                ctx,
                &thread_id,
                &task_type,
                &behavior_mode,
                &user_input,
                &result,
                provider.as_deref(),
                model.as_deref(),
                state,
            );
        }

        // Qui si calcolava un reward fuso che nessuno riceveva. RIMOSSO invece
        // che cablato, dopo aver misurato il destinatario: la Q-table
        // (`nexus_q_values`) e' ferma da settimane, `feedback_score` e'
        // valorizzato in 0 righe su 2179, e chi la interroga
        // (`suggest_agent`/`select_agent`) ha due soli chiamanti non-test — un
        // endpoint gRPC senza client e una rotta di prova. Il routing vero passa
        // dalla routing matrix, che la Q-table non la legge mai.
        //
        // Non e' solo lavoro inerte: la chiave della Q-table e'
        // `(task_type, AgentType)`, senza provider ne' modello ne' tier, quindi
        // non e' nemmeno agganciabile al routing dei modelli senza prima
        // cambiarne lo schema. Il trait `RewardSink` che i commenti citavano non
        // e' mai esistito in questo repo.
        //
        // `closure_judge` era anch'esso solo un commento: nessun codice, flag
        // spenti, nessun consumatore. Rimosso il riferimento con lo stesso
        // criterio.

        tracing::info!(
            target: "nexus_agent_graph::learner",
            thread_id = %thread_id,
            task = %task_type,
            "learner: interazione registrata"
        );

        // ── Delta finale (__init__.py:4633-4635): SOLO completed_at ───────────
        Ok(StateDelta {
            completed_at: Some(Some(Self::now_completed_at())),
            ..Default::default()
        }
        .into_opaque())
    }
}

impl LearnerNode {
    /// Persiste l'interazione in `brain_learning_interactions` come task
    /// best-effort fire-and-forget (`memory/storage.py:35` `save_interaction`).
    /// Delega a `ctx.db`.
    ///
    /// NOTA divergenza CONTROLLATA: `save_interaction` Python esegue anche un
    /// secondo upsert su `brain_task_stats` (`_update_stats`, storage.py:79). Lo
    /// portiamo per parita' di side-effect (l'INSERT principale + lo stats upsert),
    /// entrambi nello stesso task fire-and-forget. `metadata={"iterations": ...}`
    /// (__init__.py:4559) -> jsonb.
    #[allow(clippy::too_many_arguments)]
    fn spawn_persist_pg(
        &self,
        ctx: &AgentNodeCtx,
        thread_id: &str,
        task_type: &str,
        behavior_mode: &str,
        user_input: &str,
        result: &str,
        provider: Option<&str>,
        model: Option<&str>,
        state: &AgentState,
    ) {
        let pool = ctx.db.clone();
        let thread_id = thread_id.to_string();
        let task_type = task_type.to_string();
        let behavior_mode = behavior_mode.to_string();
        let user_input = user_input.to_string();
        // `agent_output = result or ""` (__init__.py:4553).
        let agent_output = result.to_string();
        let provider = provider.map(str::to_string);
        let model = model.map(str::to_string);
        let latency_ms = state.latency_ms;
        let token_usage = state.token_usage;
        // metadata = {"iterations": state.get("iterations", 1)} (__init__.py:4559).
        // Il Python usa default 1 quando la chiave manca.
        let iterations = state.iterations.unwrap_or(1);
        let metadata = json!({ "iterations": iterations });

        // Fire-and-forget: niente await nel nodo, errore loggato come WARN.
        tokio::spawn(async move {
            let res = sqlx::query(
                "INSERT INTO brain_learning_interactions \
                 (thread_id, task_type, behavior_mode, user_input, agent_output, \
                  provider, model, latency_ms, token_usage, qdrant_id, metadata) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::jsonb)",
            )
            .bind(&thread_id)
            .bind(&task_type)
            .bind(&behavior_mode)
            .bind(&user_input)
            .bind(&agent_output)
            .bind(&provider)
            .bind(&model)
            .bind(latency_ms)
            .bind(token_usage)
            // qdrant_id NULL: l'upsert Qdrant e' un TODO, quindi non c'e' id.
            .bind(Option::<String>::None)
            .bind(&metadata)
            .execute(&pool)
            .await;
            if let Err(err) = res {
                tracing::warn!(
                    target: "nexus_agent_graph::learner",
                    error = %err,
                    "learner: persistenza PostgreSQL fallita (best-effort)"
                );
                return;
            }
            // Stats aggregate (parita' col `_update_stats` Python, storage.py:79).
            let stats = sqlx::query(
                "INSERT INTO brain_task_stats \
                 (task_type, total_count, success_count, avg_latency_ms, last_updated) \
                 VALUES ($1, 1, 1, $2, NOW()) \
                 ON CONFLICT(task_type) DO UPDATE SET \
                   total_count = brain_task_stats.total_count + 1, \
                   success_count = brain_task_stats.success_count + 1, \
                   avg_latency_ms = (brain_task_stats.avg_latency_ms * brain_task_stats.total_count \
                     + COALESCE(EXCLUDED.avg_latency_ms, 0)) / (brain_task_stats.total_count + 1), \
                   last_updated = EXCLUDED.last_updated",
            )
            .bind(&task_type)
            // `latency_ms or 0.0` (storage.py:95).
            .bind(latency_ms.unwrap_or(0.0))
            .execute(&pool)
            .await;
            if let Err(err) = stats {
                tracing::warn!(
                    target: "nexus_agent_graph::learner",
                    error = %err,
                    "learner: aggiornamento brain_task_stats fallito (best-effort)"
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::runtime::ports::{
        EventSink, LlmGateway, LlmRequest, LlmResponse, PortError, SseEvent,
    };
    use crate::runtime::test_doubles::StubToolExecutor;
    use crate::runtime::AgentNodeCtx;
    use crate::state::{AgentState, Message, MessageContent, StopReason};

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    fn human(text: &str) -> Message {
        Message::Human {
            content: MessageContent::text(text),
        }
    }

    /// LLM che non viene mai chiamato dal learner (nessun I/O LLM nel nodo): lo
    /// stub serve solo a riempire il ctx.
    struct UnusedLlm;
    #[async_trait]
    impl LlmGateway for UnusedLlm {
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, PortError> {
            panic!("il learner non deve chiamare l'LLM");
        }
    }

    struct Sink;
    impl EventSink for Sink {
        fn emit(&self, _ev: SseEvent) {}
    }

    /// Ctx di test con PgPool lazy (i test che NON innescano persistenza non
    /// toccano il DB; quelli con persistenza la spawnano fire-and-forget e non
    /// attendono l'esito, quindi il pool lazy non si connette mai davvero).
    fn ctx_with() -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            isolation_available: false,
            db: pool,
            llm: Arc::new(UnusedLlm),
            tools: Arc::new(StubToolExecutor::with_success(json!("ok"))),
            emit: Arc::new(Sink),
            cfg: crate::routing::config::RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            advisory_gate: None,
        }
    }

    /// Stato base: un HumanMessage iniziale, result presente, end_turn.
    fn base_state() -> AgentState {
        AgentState {
            messages: vec![human("implementa la cache")],
            result: Some("ho implementato la cache con Redis".to_string()),
            user_intent: Some("code_write".to_string()),
            behavior_mode: Some("bilanciata".to_string()),
            iterations: Some(3),
            stop_reason: Some(StopReason::EndTurn),
            thread_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            ..Default::default()
        }
    }

    // ── Delta finale (happy path) ──────────────────────────────────────────────

    #[tokio::test]
    async fn happy_path_delta_solo_completed_at() {
        let node = LearnerNode::new();
        let ctx = ctx_with();
        let st = base_state();
        let delta_typed: StateDelta = {
            // Verifichiamo che il delta tipizzato porti SOLO completed_at.
            let opaque = node.run(&st, &ctx).await.expect("run ok");
            let map = serde_json::Value::Object(opaque.as_map().clone());
            serde_json::from_value(map).expect("delta tipizzato")
        };
        // completed_at presente e non-null.
        assert!(matches!(delta_typed.completed_at, Some(Some(_))));
        // Nessun altro campo scritto (gli altri restano None = no-op).
        assert!(delta_typed.result.is_none());
        assert!(delta_typed.final_reward.is_none());
        assert!(delta_typed.reflection_score.is_none());

        // Applicato allo stato, popola completed_at.
        let out = apply(st, node.run(&base_state(), &ctx).await.expect("run ok"));
        assert!(out.completed_at.is_some(), "completed_at popolato");
    }

    /// result vuoto: il nodo non persiste ma chiude pulito.
    #[tokio::test]
    async fn result_vuoto_chiude_pulito() {
        let node = LearnerNode::new();
        let ctx = ctx_with();
        let mut st = base_state();
        st.result = Some(String::new());
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert!(out.completed_at.is_some());
    }

    // ── Funzioni deterministiche unitarie ──────────────────────────────────────

    #[test]
    fn user_input_primo_human() {
        let msgs = vec![human("primo"), human("secondo")];
        assert_eq!(LearnerNode::user_input(&msgs), "primo");
        // Nessun human -> stringa vuota.
        assert_eq!(LearnerNode::user_input(&[]), "");
    }





}

// Il `mod golden` del learner e' stato rimosso con le funzioni che esercitava
// (fuse_reward, should_save_qdrant, interaction_text, build_qdrant_payload).
// Era comunque un test che non poteva girare: `#[ignore]`, dipendente da
// `/tmp/golden_learner.json` e da uno script generatore Python cancellato col
// porting zero-Python (ADR 0041), su un percorso che su Windows non esiste.

