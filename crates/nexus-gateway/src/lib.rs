//! Nexus LLM Gateway (Rust).
//!
//! Migrazione del gateway proxy multi-provider da Node/TypeScript a Rust.
//! Il contratto LLM e' fedele a `packages/shared/src/llm-types.ts` (lingua
//! franca: OpenAI Chat Completions). Vedi ADR 0032 ("il compilato e'
//! autoritativo"): a regime questo crate sostituisce `apps/nexus-gateway`
//! (Node) e il cooldown billing vive nello stesso runtime di mcp-core,
//! eliminando lo split che oggi impedisce il re-probe reattivo dei provider.
//!
//! Migrazione INCREMENTALE: il gateway Node resta autoritativo a runtime
//! finche' la parita' non e' validata (Fase 6). I moduli vengono aggiunti
//! una fase alla volta, sempre mantenendo `cargo check` verde.

pub mod batch;
pub mod cooldown;
pub mod model_alias_resolver;
pub mod policy_engine;
pub mod provider;
pub mod provider_error;
pub mod providers;
pub mod rate_limiter;
pub mod redaction;
pub mod server;
pub mod types;
