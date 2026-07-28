//! Adapter mcp-core -> trait del grafo Rust (`nexus-agent-graph`).
//!
//! CONFINE D'INVERSIONE (vedi `nexus_agent_graph::runtime::ports`): il crate del
//! grafo NON dipende da mcp-core; espone le sue dipendenze I/O come TRAIT astratti
//! (`LlmGateway`, `ToolExecutor`, `EventSink`, `TodoStore`, ...). mcp-core
//! (questo modulo) le IMPLEMENTERA' delegando alle proprie infrastrutture concrete
//! (gateway LLM `nexus_gateway`, ToolRunner gRPC, canale SSE `nexus_events`,
//! `sqlx::PgPool` per le scritture DB).
//!
//! STATO — FASE 2 COMPLETA (tutte le 14 impl concrete):
//! ogni modulo qui sotto contiene la struct adapter + il suo costruttore + la
//! `impl <Trait>` concreta che delega all'I/O reale (gate Real/Replay, fail-open).
//! L'aggancio nel motore (`run_via_native`) e la costruzione effettiva degli
//! adapter sono lavoro della FASE 3. Finche' `select_engine` resta `python` il
//! path Rust e' irraggiungibile, quindi la regressione e' NULLA: queste struct non
//! sono ancora cablate da nessun call site (da cui l'`#[allow(dead_code)]` mirato).
//!
//! Regola L (punto unico): ogni adapter delega a UN servizio concreto gia'
//! esistente in mcp-core; non re-implementa logica (gateway/cooldown/routing
//! restano nei loro moduli autoritativi). Regola G: nessun nome modello /
//! provider hardcoded qui (provider e model arrivano gia' risolti nelle
//! `LlmRequest`).

// STATO — FASE 3 (cablaggio reale): tutte le 14 impl concrete sono ora COSTRUITE
// dal motore nativo ([`crate::native_engine::build_native_engine`]), che le inietta
// nei nodi del grafo Rust. Gli `#[allow(dead_code)] // cablato in F3` per-file sono
// stati rimossi: ciascun adapter ha ora un call site reale. Il path nativo e' il
// PRIMARIO instradato globalmente (select_engine ritorna 'rust' sulla riga jolly
// '*'=rust, regola G): e' il flusso effettivamente eseguito per i nuovi run.

// --- 8 impl FASE 2a (cablate da native_engine) ---
pub mod agent_step_store;
pub mod billing_cooldown_port;
pub mod event_sink;
pub mod meta_step_store;
pub mod model_upscale_port;
pub mod run_control_store;
pub mod todo_store;
pub mod verifier_run_store;

// --- 3 impl FASE 2b: NextActions / ContextOffload / Escalation ---
pub mod context_offload;
pub mod escalation_port;
pub mod next_actions_deriver;

// --- detector clarification CROSS-RUN (loop email): ClarifyHistoryPort ---
pub mod clarify_history_store;

// --- meta-reasoner LLM (ADR 0036-style): MetaReasonerPort (PgMetaReasonerPort) ---
// recover (recovery-da-stallo) implementato; orchestrate STUB (#11c).
pub mod stall_reasoner_port;

// --- budget CROSS-RUN del meta-reasoner (per sessione): StallBudgetPort ---
// contatore append+count su nexus_agent_meta_steps (kind='stall_budget'), zero DDL.
pub mod stall_budget_store;

// --- rolling-summary (intervento 3): SummaryStore (LLM economico) ---
pub mod summary_store;

// --- continuity-trim (EmbeddingStore): embedder ONNX in-process per la
// compressione SEMANTICA del contesto (coseno vs focus del turno) ---
pub mod embedding_store;

// --- 3 impl FASE 2c: LlmGateway / ToolExecutor / CriteriaRunner ---
pub mod criteria_runner;
pub mod review_panel;

// --- misura del progresso fra un rimando in correzione e il successivo:
//     MutationProgressPort (sopra `file_mutations`, hash del contenuto) ---
pub mod mutation_progress;
pub mod llm_gateway;
pub mod tool_executor;
