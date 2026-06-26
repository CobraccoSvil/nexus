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

// In FASE 1 (scaffolding) nessun call site costruisce ancora questi adapter:
// struct, costruttori `new` e handle al servizio reale sono "dead code" finche'
// la FASE 2 non implementa i trait e li caggia nel motore. L'allow vive a livello
// di modulo (un solo punto) e SARA' RIMOSSO in F2 quando gli adapter diventano vivi.
#![allow(dead_code)]

pub mod agent_step_store;
pub mod billing_cooldown_port;
pub mod context_offload;
pub mod criteria_runner;
pub mod escalation_port;
pub mod event_sink;
pub mod llm_gateway;
pub mod meta_step_store;
pub mod model_upscale_port;
pub mod next_actions_deriver;
pub mod run_control_store;
pub mod todo_store;
pub mod tool_executor;
pub mod verifier_run_store;
