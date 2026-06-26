//! Adapter mcp-core -> trait del grafo Rust (`nexus-agent-graph`).
//!
//! CONFINE D'INVERSIONE (vedi `nexus_agent_graph::runtime::ports`): il crate del
//! grafo NON dipende da mcp-core; espone le sue dipendenze I/O come TRAIT astratti
//! (`LlmGateway`, `ToolExecutor`, `EventSink`, `TodoStore`, ...). mcp-core
//! (questo modulo) le IMPLEMENTERA' delegando alle proprie infrastrutture concrete
//! (gateway LLM `nexus_gateway`, ToolRunner gRPC, canale SSE `nexus_events`,
//! `sqlx::PgPool` per le scritture DB).
//!
//! STATO — FASE 1 (SCAFFOLDING, ZERO comportamento):
//! questo modulo contiene SOLO le struct adapter (una per trait di
//! `runtime::ports`) con il loro costruttore e l'handle al servizio reale a cui
//! delegheranno. NESSUNA `impl <Trait> for ...` e' ancora presente: l'aggancio
//! effettivo dei trait (con la delega all'I/O concreto) e' lavoro della FASE 2.
//! Finche' `select_engine` resta `python` il path Rust e' irraggiungibile, quindi
//! la regressione e' NULLA: queste struct non sono ancora cablate da nessun
//! call site.
//!
//! Regola L (punto unico): ogni adapter delega a UN servizio concreto gia'
//! esistente in mcp-core; non re-implementa logica (gateway/cooldown/routing
//! restano nei loro moduli autoritativi). Regola G: nessun nome modello /
//! provider hardcoded qui (provider e model arrivano gia' risolti nelle
//! `LlmRequest`).

// STATO — FASE 2a (8 impl concrete agganciate ai servizi mcp-core):
//
// Gli 8 adapter sotto hanno l'`impl <Trait>` concreta (delega all'I/O reale,
// gate Real/Replay, fail-open). NON sono ancora COSTRUITI da nessun call site:
// `run_via_native` resta uno stub (select_engine ritorna sempre Python), quindi
// struct + `new` + impl restano "non costruiti" finche' la FASE 3 non li caggia nel
// motore nativo. Per questo ciascuno porta un `#[allow(dead_code)]` MIRATO con la
// nota "cablato in F3" (non un allow di modulo che maschererebbe anche i 6 file
// ancora-stub). I 6 file rimanenti (F2b/F2c: NextActions/ContextOffload/Escalation/
// LlmGateway/ToolExecutor/CriteriaRunner) sono ancora scaffold senza impl: l'allow
// di modulo copre solo loro.

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

// --- 6 scaffold senza impl (F2b/F2c): ancora dead code totale ---
#[allow(dead_code)] // scaffold F1, impl in F2b/F2c
pub mod context_offload;
#[allow(dead_code)] // scaffold F1, impl in F2b/F2c
pub mod criteria_runner;
#[allow(dead_code)] // scaffold F1, impl in F2b/F2c
pub mod escalation_port;
#[allow(dead_code)] // scaffold F1, impl in F2b/F2c
pub mod llm_gateway;
#[allow(dead_code)] // scaffold F1, impl in F2b/F2c
pub mod next_actions_deriver;
#[allow(dead_code)] // scaffold F1, impl in F2b/F2c
pub mod tool_executor;
