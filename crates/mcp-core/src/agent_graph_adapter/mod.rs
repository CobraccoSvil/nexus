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

// STATO — FASE 2 COMPLETA (tutte le 14 impl concrete agganciate ai servizi mcp-core):
//
// Tutti gli adapter sotto hanno l'`impl <Trait>` concreta (delega all'I/O reale,
// gate Real/Replay, fail-open). NON sono ancora COSTRUITI da nessun call site:
// `run_via_native` resta uno stub (select_engine ritorna sempre Python), quindi
// struct + `new` + impl restano "non costruiti" finche' la FASE 3 non li cabla nel
// motore nativo. Per questo ciascuno porta un `#[allow(dead_code)]` MIRATO con la
// nota "cablato in F3": l'allow e' per-file (non di modulo), cosi' resta visibile
// per-adapter e si rimuove uno alla volta quando F3 costruisce ciascuno. select_engine
// resta Python -> path Rust irraggiungibile -> regressione NULLA.

// --- 8 impl FASE 2a (cablate in F3): allow mirato per-file ---
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod agent_step_store;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod billing_cooldown_port;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod event_sink;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod meta_step_store;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod model_upscale_port;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod run_control_store;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod todo_store;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod verifier_run_store;

// --- 3 impl FASE 2b (cablate in F3): NextActions / ContextOffload / Escalation ---
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod context_offload;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod escalation_port;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod next_actions_deriver;

// --- 3 impl FASE 2c (cablate in F3): LlmGateway / ToolExecutor / CriteriaRunner ---
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod criteria_runner;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod llm_gateway;
#[allow(dead_code)] // cablato in F3 (run_via_native): impl viva, non ancora costruita
pub mod tool_executor;
