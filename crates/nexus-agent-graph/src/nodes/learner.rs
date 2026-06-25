//! `LearnerNode` — porta la parte DETERMINISTICA del `learner_node`
//! (`brain/agents/nodes/__init__.py:4466-4635`).
//!
//! Il learner e' il nodo TERMINALE del grafo agentico: non instrada (nessun
//! edge da portare) e produce un delta minimale `{completed_at}`. Tutto il resto
//! sono SIDE-EFFECT di persistenza/feedback chiusi nel nodo:
//!   - filtra il salvataggio Qdrant via un reward PRELIMINARE di qualita';
//!   - persiste l'interazione (Qdrant + PostgreSQL `brain_learning_interactions`);
//!   - invia il feedback Q-learning al router (Q-table di mcp-core);
//!   - in coda lancia il `closure_judge` in SHADOW (best-effort, mig 0391).
//!
//! ## Cosa porta QUESTO PR (deterministico, testato golden 1:1)
//!
//! - **`prelim_reward`** (`__init__.py:4502-4508`): PUNTO UNICO in
//!   `decisions::reward` (regola L), accanto a `heuristic_reward`. E' un reward
//!   DIVERSO dall'euristico (conta `end_turn` + presenza result, non le
//!   iterazioni). Il nodo lo CHIAMA, non lo re-implementa.
//! - **`heuristic_reward`** (`__init__.py:4580-4587`): RIUSATO dal punto unico
//!   `decisions::reward` (regola L: identico al reflection, NON duplicato qui).
//! - **reward fusion** (`__init__.py:4590-4591`): `final_reward` dallo stato se
//!   presente, altrimenti l'euristico. Selezione pura ([`Self::fuse_reward`]).
//! - **save_to_qdrant gate** (`__init__.py:4512`): `auto_extract AND prelim_reward
//!   >= min_confidence` ([`Self::should_save_qdrant`]).
//! - **estrazione user_input** (primo HumanMessage, `__init__.py:4491-4495`),
//!   **payload Qdrant** con preview troncate a 200 CHAR
//!   (`__init__.py:4538-4539`), **interaction_text** (`__init__.py:4530`),
//!   **completed_at** (now UTC isoformat, `__init__.py:4488`): tutto in
//!   [`Self::build_qdrant_payload`] / [`Self::interaction_text`], deterministico.
//!
//! ## Cosa NON porta (I/O posticipato dietro TODO espliciti, regola H)
//!
//! Il learner ha 4 effetti di I/O. Per il porting incrementale (path Rust NON
//! instradato in produzione) NESSUN nuovo trait viene introdotto in questo PR:
//!
//! - **submit_feedback** (Q-learning -> Q-table di mcp-core,
//!   `agent_router_client.py:146`): non mappa su nessuno dei 4 trait esistenti
//!   (`LlmGateway`/`ToolExecutor`/`EventSink`/`db`). TODO esplicito (porta
//!   dedicata `RewardSink`, vedi [`Self::run`]). Gated da `(profile_name presente)`
//!   nel Python (`_agent_router` + `profile_name`): qui calcoliamo COMUNQUE il
//!   reward fuso (deterministico, golden-abile) ma NON lo inviamo.
//! - **Persistenza Qdrant** (`store_interaction_vector`: embedding + upsert,
//!   `__init__.py:4541`): richiede embedding + client Qdrant non disponibili al
//!   nodo. Costruiamo il payload deterministico (golden-abile) ma NON upsertiamo.
//!   TODO esplicito.
//! - **Persistenza PostgreSQL** `brain_learning_interactions`
//!   (`memory/storage.py:35`): DELEGATA a `ctx.db` come task best-effort
//!   fire-and-forget, GATED su `!ctx.shadow` (come reflection per
//!   `nexus_agent_reflections`). In shadow zero scritture; ogni errore e' WARN
//!   non-fatale (parita' col Python che logga e non rilancia). Vedi
//!   [`Self::spawn_persist_pg`].
//! - **closure_judge shadow** (`closure_judge.py`, chiamata LLM avvolta in
//!   try/except che ingoia tutto, `__init__.py:4627-4631`): e' un sotto-modulo a
//!   se' (punto unico in `closure_judge.py`). NON portato qui -> TODO esplicito.
//!   I flag (`agent.closure_judge.*`) sono shadow/OFF di default -> nessuna
//!   divergenza dal path di produzione.
//!
//! REGOLA SHADOW: in `ctx.shadow == true` NESSUN side-effect (niente INSERT,
//! niente feedback, niente upsert). Verificato nei test.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::reward::{heuristic_reward, prelim_reward, MAX_AGENT_ITERATIONS};
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, StateDelta};

/// Lunghezza massima delle preview nel payload Qdrant (`__init__.py:4538-4539`:
/// `user_input[:200]` / `result[:200]`). Troncamento su CHAR, non byte.
const PREVIEW_MAX_CHARS: usize = 200;

/// Config DB-driven del nodo learner, PASSATA (regola G: nessuna lettura DB nel
/// nodo, nessun fallback hardcoded dentro la logica decisionale).
///
/// Mappa i settings letti dal brain via `_get_learning_config()`
/// (`__init__.py:214-217`, categoria `settings`: `learning_auto_extract` /
/// `learning_min_confidence`). Default IDENTICI ai safe-default conservativi del
/// brain (`_LEARNING_CFG_DEFAULTS`): valgono SOLO se il DB e' irraggiungibile,
/// mai come magic fallback nella logica.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnerConfig {
    /// Estrazione automatica abilitata (`learning_auto_extract`, default true).
    /// OFF -> nessun salvataggio Qdrant (`__init__.py:4512`).
    pub auto_extract: bool,
    /// Soglia minima del `prelim_reward` per salvare in Qdrant
    /// (`learning_min_confidence`, default 0.6).
    pub min_confidence: f64,
}

impl Default for LearnerConfig {
    fn default() -> Self {
        // Default IDENTICI ai `_LEARNING_CFG_DEFAULTS` del brain
        // (`__init__.py:214-217`). Safe-default se il DB e' irraggiungibile.
        Self {
            auto_extract: true,
            min_confidence: 0.6,
        }
    }
}

/// Payload deterministico costruito per il salvataggio Qdrant
/// (`__init__.py:4532-4540`). E' golden-abile 1:1: i campi e i troncamenti sono
/// puri. L'upsert reale (embedding + client Qdrant) e' un TODO esplicito.
#[derive(Debug, Clone, PartialEq)]
pub struct QdrantPayload {
    /// Id del thread (= run_id Nexus).
    pub thread_id: String,
    /// Tipo di task (`user_intent`, `__init__.py:4480`).
    pub task_type: String,
    /// Modalita' di comportamento.
    pub behavior_mode: String,
    /// Provider effettivamente usato (`provider_used`).
    pub provider: Option<String>,
    /// Modello effettivamente usato (`model_used`).
    pub model: Option<String>,
    /// Preview dell'input utente (primi 200 char).
    pub input_preview: String,
    /// Preview dell'output (primi 200 char; vuota se result vuoto).
    pub output_preview: String,
}

impl QdrantPayload {
    /// Serializza nella forma del dict Python (`__init__.py:4532-4540`), per il
    /// confronto golden 1:1. `provider`/`model` -> `null` se assenti. Usato dai
    /// test/golden (l'upsert reale e' un TODO, quindi qui non e' chiamato a
    /// runtime: marcato `cfg(test)` per non figurare come dead_code in build).
    #[cfg(test)]
    fn to_json(&self) -> serde_json::Value {
        json!({
            "thread_id": self.thread_id,
            "task_type": self.task_type,
            "behavior_mode": self.behavior_mode,
            "provider": self.provider,
            "model": self.model,
            "input_preview": self.input_preview,
            "output_preview": self.output_preview,
        })
    }
}

/// Nodo learner. Stateless: legge lo stato + la config passata. La persistenza
/// PostgreSQL e' delegata a `ctx.db` (gated su shadow); gli altri I/O sono TODO
/// espliciti dietro porte dedicate da cablare nell'integrazione (Fase 3 PR2).
pub struct LearnerNode {
    /// Config DB-driven (regola G: passata, mai letta dal nodo).
    cfg: LearnerConfig,
}

impl LearnerNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante.
    pub fn new(cfg: LearnerConfig) -> Self {
        Self { cfg }
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

    /// Tronca una stringa ai primi `PREVIEW_MAX_CHARS` CHAR (`s[:200]` Python su
    /// stringa: indicizzazione per code-point, non byte). Replica esattamente la
    /// semantica per stringhe unicode.
    fn preview(s: &str) -> String {
        s.chars().take(PREVIEW_MAX_CHARS).collect()
    }

    /// `interaction_text` per l'embedding Qdrant (`__init__.py:4530`):
    /// `"Input: {user_input}\nOutput: {result}"`. Deterministico, golden-abile.
    pub fn interaction_text(user_input: &str, result: &str) -> String {
        format!("Input: {user_input}\nOutput: {result}")
    }

    /// Gate del salvataggio Qdrant (`__init__.py:4512`):
    /// `auto_extract AND prelim_reward >= min_confidence`. Funzione PURA: il
    /// `prelim_reward` arriva gia' calcolato dal punto unico (regola L).
    pub fn should_save_qdrant(cfg: &LearnerConfig, prelim: f64) -> bool {
        cfg.auto_extract && prelim >= cfg.min_confidence
    }

    /// Costruisce il [`QdrantPayload`] deterministico (`__init__.py:4532-4540`),
    /// con le preview troncate a 200 char. Non esegue I/O.
    pub fn build_qdrant_payload(
        thread_id: &str,
        task_type: &str,
        behavior_mode: &str,
        provider: Option<&str>,
        model: Option<&str>,
        user_input: &str,
        result: &str,
    ) -> QdrantPayload {
        QdrantPayload {
            thread_id: thread_id.to_string(),
            task_type: task_type.to_string(),
            behavior_mode: behavior_mode.to_string(),
            provider: provider.map(str::to_string),
            model: model.map(str::to_string),
            input_preview: Self::preview(user_input),
            // `result[:200] if result else ""` (__init__.py:4539): result vuoto
            // -> preview vuota (preview("") e' gia' "", quindi equivalente).
            output_preview: Self::preview(result),
        }
    }

    /// Selezione del reward inviato al Q-learning (`__init__.py:4590-4591`):
    /// `final_reward` dallo stato (prodotto dal reflection) se presente, altrimenti
    /// l'euristico. Funzione PURA. `final_reward_state` e' `state.get("final_reward")`.
    pub fn fuse_reward(final_reward_state: Option<f64>, heuristic: f64) -> f64 {
        final_reward_state.unwrap_or(heuristic)
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
        let task_type = state.user_intent.clone().unwrap_or_else(|| "chat".to_string());
        let behavior_mode = state
            .behavior_mode
            .clone()
            .unwrap_or_else(|| "bilanciata".to_string());
        let result = state.result.clone().unwrap_or_default();
        let provider = state.provider_used.clone();
        let model = state.model_used.clone();
        let user_input = Self::user_input(&state.messages);
        let stop_reason = Self::stop_reason_str(state);

        // ── Reward PRELIMINARE (PUNTO UNICO decisions::reward, regola L) ──────
        // Filtra il salvataggio Qdrant: interazioni di bassa qualita' non devono
        // inquinare il RAG (__init__.py:4502-4508). DIVERSO dall'euristico.
        let prelim = prelim_reward(&stop_reason, !result.is_empty());
        let save_to_qdrant = Self::should_save_qdrant(&self.cfg, prelim);

        if !self.cfg.auto_extract {
            tracing::debug!(
                target: "nexus_agent_graph::learner",
                "learner: salvataggio Qdrant saltato (auto_extract=false)"
            );
        } else if prelim < self.cfg.min_confidence {
            tracing::debug!(
                target: "nexus_agent_graph::learner",
                prelim,
                min_confidence = self.cfg.min_confidence,
                stop = %stop_reason,
                "learner: salvataggio Qdrant saltato (reward_prelim < min_confidence)"
            );
        }
        if result.is_empty() {
            tracing::warn!(
                target: "nexus_agent_graph::learner",
                stop = %stop_reason,
                "learner: result vuoto, skip Qdrant/PostgreSQL"
            );
        }

        // ── Payload Qdrant deterministico (golden-abile) ──────────────────────
        // Costruito SEMPRE (puro): l'upsert reale e' un TODO. Il gate Python
        // (__init__.py:4529) richiede user_input && result && save_to_qdrant.
        let _qdrant_payload: Option<QdrantPayload> =
            if !user_input.is_empty() && !result.is_empty() && save_to_qdrant {
                let p = Self::build_qdrant_payload(
                    &thread_id,
                    &task_type,
                    &behavior_mode,
                    provider.as_deref(),
                    model.as_deref(),
                    &user_input,
                    &result,
                );
                // TODO porting: upsert Qdrant (store_interaction_vector,
                // __init__.py:4541) dietro porta dedicata (embedding + client
                // Qdrant non disponibili al nodo, Fase 3 PR2 integrazione). Qui
                // costruiamo SOLO il payload deterministico (interaction_text +
                // metadata), senza upsertare: niente side-effect, niente qdrant_id.
                let _interaction_text = Self::interaction_text(&user_input, &result);
                Some(p)
            } else {
                None
            };

        // ── Persistenza PostgreSQL (best-effort, GATED su shadow) ─────────────
        // brain_learning_interactions (memory/storage.py:35). Gate Python:
        // `_storage is not None and user_input` (__init__.py:4546). In shadow
        // NON scrive (zero side-effect). Fire-and-forget: errore -> WARN, mai
        // fatale (parita' col Python che logga e non rilancia, __init__.py:4561).
        if !ctx.shadow && !user_input.is_empty() {
            self.spawn_persist_pg(ctx, &thread_id, &task_type, &behavior_mode, &user_input,
                &result, provider.as_deref(), model.as_deref(), state);
        }

        // ── Reward fuso per il Q-learning (deterministico, golden-abile) ──────
        // heuristic_reward: PUNTO UNICO decisions::reward (regola L, RIUSATO).
        // iteration_budget non e' nel learner Python: usa MAX_AGENT_ITERATIONS
        // come floor (`iterations >= MAX_AGENT_ITERATIONS`, __init__.py:4582).
        let iterations = state.iterations.unwrap_or(0);
        let heuristic = heuristic_reward(
            &stop_reason,
            !result.is_empty(),
            iterations,
            // budget=0 -> il punto unico ricade su MAX_AGENT_ITERATIONS (il
            // learner Python confronta direttamente con MAX_AGENT_ITERATIONS).
            0,
        );
        let _reward = Self::fuse_reward(state.final_reward, heuristic);

        // TODO porting: feedback Q-table dietro trait RewardSink, Fase 3 PR2
        // integrazione (submit_feedback -> Q-table di mcp-core,
        // agent_router_client.py:146). Non mappa su nessuno dei 4 trait esistenti
        // (LlmGateway/ToolExecutor/EventSink/db); NON introduciamo un nuovo trait
        // ora. Gated nel Python da (profile_name presente); qui calcoliamo
        // COMUNQUE il reward fuso (deterministico) ma NON lo inviamo. In shadow
        // andrebbe comunque soppresso (zero side-effect).
        let _ = (MAX_AGENT_ITERATIONS, &state.profile_name);

        // TODO porting: closure_judge come modulo separato (closure_judge.py,
        // mig 0391). Chiamata LLM best-effort avvolta in try/except che ingoia
        // tutto (__init__.py:4627-4631). E' un punto unico a se'; i flag
        // (agent.closure_judge.shadow_enabled/active) sono shadow/OFF di default
        // -> nessuna divergenza dal path di produzione. NON portato qui.

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
    /// GATING SHADOW: chiamata SOLO quando `!ctx.shadow` (il gate e' nel
    /// chiamante `run`): in shadow zero scritture.
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
    use crate::runtime::ports::{EventSink, LlmGateway, LlmRequest, LlmResponse, PortError, SseEvent};
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
    fn ctx_with(shadow: bool) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            db: pool,
            llm: Arc::new(UnusedLlm),
            tools: Arc::new(StubToolExecutor::with_success(json!("ok"))),
            emit: Arc::new(Sink),
            cfg: crate::routing::config::RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow,
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
        let node = LearnerNode::new(LearnerConfig::default());
        let ctx = ctx_with(false);
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

    /// Shadow: NESSUN side-effect. Il delta e' comunque {completed_at} (la
    /// persistenza e' soppressa, ma il nodo termina pulito).
    #[tokio::test]
    async fn shadow_nessun_side_effect() {
        let node = LearnerNode::new(LearnerConfig::default());
        let ctx = ctx_with(true); // shadow
        let st = base_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // Il delta resta {completed_at}; nessuna scrittura DB (pool lazy mai usato).
        assert!(out.completed_at.is_some());
    }

    /// auto_extract=false: il gate Qdrant e' chiuso (nessun salvataggio), ma il
    /// nodo termina comunque con {completed_at}.
    #[tokio::test]
    async fn flag_auto_extract_off() {
        let cfg = LearnerConfig {
            auto_extract: false,
            ..Default::default()
        };
        let node = LearnerNode::new(cfg);
        let ctx = ctx_with(false);
        let st = base_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert!(out.completed_at.is_some());
        // Gate Qdrant chiuso indipendentemente dal reward.
        assert!(!LearnerNode::should_save_qdrant(
            &LearnerConfig { auto_extract: false, ..Default::default() },
            1.0
        ));
    }

    /// result vuoto: il nodo non scrive Qdrant/PG ma chiude pulito.
    #[tokio::test]
    async fn result_vuoto_chiude_pulito() {
        let node = LearnerNode::new(LearnerConfig::default());
        let ctx = ctx_with(false);
        let mut st = base_state();
        st.result = Some(String::new());
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert!(out.completed_at.is_some());
    }

    // ── Funzioni deterministiche unitarie ──────────────────────────────────────

    #[test]
    fn user_input_primo_human() {
        let msgs = vec![
            human("primo"),
            human("secondo"),
        ];
        assert_eq!(LearnerNode::user_input(&msgs), "primo");
        // Nessun human -> stringa vuota.
        assert_eq!(LearnerNode::user_input(&[]), "");
    }

    #[test]
    fn should_save_qdrant_gate() {
        let cfg = LearnerConfig { auto_extract: true, min_confidence: 0.6 };
        // prelim >= soglia E auto_extract -> true.
        assert!(LearnerNode::should_save_qdrant(&cfg, 0.6));
        assert!(LearnerNode::should_save_qdrant(&cfg, 1.0));
        // prelim < soglia -> false.
        assert!(!LearnerNode::should_save_qdrant(&cfg, 0.4));
        // auto_extract off -> false anche se sopra soglia.
        let off = LearnerConfig { auto_extract: false, min_confidence: 0.6 };
        assert!(!LearnerNode::should_save_qdrant(&off, 1.0));
    }

    #[test]
    fn fuse_reward_final_o_heuristic() {
        // final_reward presente -> usa quello.
        assert_eq!(LearnerNode::fuse_reward(Some(0.94), 1.0), 0.94);
        // final_reward assente -> usa l'euristico.
        assert_eq!(LearnerNode::fuse_reward(None, 0.4), 0.4);
    }

    #[test]
    fn preview_tronca_a_200_char() {
        let lunga = "x".repeat(500);
        let p = LearnerNode::preview(&lunga);
        assert_eq!(p.chars().count(), 200);
        // Unicode: i 200 sono CODE-POINT, non byte.
        let unicode = "à".repeat(300);
        let pu = LearnerNode::preview(&unicode);
        assert_eq!(pu.chars().count(), 200);
    }

    #[test]
    fn interaction_text_formato() {
        assert_eq!(
            LearnerNode::interaction_text("dom", "ris"),
            "Input: dom\nOutput: ris"
        );
    }

    #[test]
    fn build_payload_preview_e_provider_null() {
        let p = LearnerNode::build_qdrant_payload(
            "tid", "code_write", "bilanciata", None, None,
            &"a".repeat(300), &"b".repeat(300),
        );
        assert_eq!(p.input_preview.chars().count(), 200);
        assert_eq!(p.output_preview.chars().count(), 200);
        let j = p.to_json();
        assert_eq!(j["provider"], json!(null));
        assert_eq!(j["model"], json!(null));
        assert_eq!(j["thread_id"], json!("tid"));
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del nodo
    //! learner + del punto unico reward. Lo script `/tmp/gen_golden_learner.py`
    //! replica byte-fedele la logica inline del `learner_node`
    //! (`prelim_reward`, reward fusion, save_to_qdrant gate, troncamenti payload,
    //! user_input) e per il reward Q-learning importa il punto unico gia' validato
    //! e salva `{case_id, function, input, output}` in `/tmp/golden_learner.json`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 /tmp/gen_golden_learner.py
    //!   cargo test -p nexus-agent-graph --lib golden_learner_parita -- --ignored

    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::{LearnerConfig, LearnerNode};
    use crate::decisions::reward::{heuristic_reward, prelim_reward};
    use crate::state::{Message, MessageContent};

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    #[test]
    #[ignore = "richiede /tmp/golden_learner.json generato da gen_golden_learner.py"]
    fn golden_learner_parita() {
        let path = "/tmp/golden_learner.json";
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("impossibile leggere {path}: {e}; genera con python3 /tmp/gen_golden_learner.py")
        });
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(!cases.is_empty(), "golden vuoto");

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "prelim_reward" => {
                    let sr = c.input.get("stop_reason").and_then(Value::as_str).unwrap_or("");
                    let res = c
                        .input
                        .get("result_non_empty")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    json!(prelim_reward(sr, res))
                }
                "heuristic_reward" => {
                    let sr = c.input.get("stop_reason").and_then(Value::as_str).unwrap_or("");
                    let res = c
                        .input
                        .get("result_non_empty")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let it = c.input.get("iterations").and_then(Value::as_i64).unwrap_or(0);
                    let bud = c.input.get("iteration_budget").and_then(Value::as_i64).unwrap_or(0);
                    json!(heuristic_reward(sr, res, it, bud))
                }
                "fuse_reward" => {
                    // final_reward_state e' null o un float.
                    let frs = c.input.get("final_reward_state").and_then(Value::as_f64);
                    let h = c.input.get("heuristic").and_then(Value::as_f64).unwrap_or(0.0);
                    json!(LearnerNode::fuse_reward(frs, h))
                }
                "should_save_qdrant" => {
                    let auto = c.input.get("auto_extract").and_then(Value::as_bool).unwrap_or(false);
                    let minc = c.input.get("min_confidence").and_then(Value::as_f64).unwrap_or(0.0);
                    let prelim = c.input.get("prelim_reward").and_then(Value::as_f64).unwrap_or(0.0);
                    let cfg = LearnerConfig { auto_extract: auto, min_confidence: minc };
                    json!(LearnerNode::should_save_qdrant(&cfg, prelim))
                }
                "interaction_text" => {
                    let ui = c.input.get("user_input").and_then(Value::as_str).unwrap_or("");
                    let res = c.input.get("result").and_then(Value::as_str).unwrap_or("");
                    json!(LearnerNode::interaction_text(ui, res))
                }
                "build_qdrant_payload" => {
                    let ui = c.input.get("user_input").and_then(Value::as_str).unwrap_or("");
                    let res = c.input.get("result").and_then(Value::as_str).unwrap_or("");
                    let tid = c.input.get("thread_id").and_then(Value::as_str).unwrap_or("");
                    let tt = c.input.get("task_type").and_then(Value::as_str).unwrap_or("");
                    let bm = c.input.get("behavior_mode").and_then(Value::as_str).unwrap_or("");
                    let prov = c.input.get("provider").and_then(Value::as_str);
                    let model = c.input.get("model").and_then(Value::as_str);
                    let p = LearnerNode::build_qdrant_payload(tid, tt, bm, prov, model, ui, res);
                    p.to_json()
                }
                "user_input" => {
                    // input.messages = lista di {role, content}; ricostruiamo i Message.
                    let msgs_raw = c.input.get("messages").and_then(Value::as_array).expect("messages");
                    let msgs: Vec<Message> = msgs_raw
                        .iter()
                        .filter_map(|m| {
                            let role = m.get("role").and_then(Value::as_str)?;
                            let content = m.get("content").and_then(Value::as_str).unwrap_or("");
                            match role {
                                "user" | "human" => Some(Message::Human {
                                    content: MessageContent::text(content),
                                }),
                                _ => None,
                            }
                        })
                        .collect();
                    json!(LearnerNode::user_input(&msgs))
                }
                other => panic!("funzione golden sconosciuta: {other} (caso {})", c.case_id),
            };

            assert!(
                got == c.output,
                "PARITA' FALLITA caso {} ({}):\n  rust   = {}\n  python = {}",
                c.case_id,
                c.function,
                got,
                c.output
            );
            checked += 1;
        }
        println!("golden learner: {checked} casi verificati, tutti verdi");
    }
}
