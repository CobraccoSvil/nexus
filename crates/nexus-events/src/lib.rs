//! Dispatcher centrale di eventi cross-pannello per Nexus IDE.
//!
//! Vedi `docs/architecture/dispatcher.md` per la motivazione e l'integrazione.
//!
//! Espone:
//! - [`event::ProjectEvent`] enum tipizzata degli eventi
//! - [`event::EnvelopedEvent`] busta con seq, event_id, ts, ui_hint
//! - [`dispatcher::ProjectChannels`] mappa per-progetto di broadcast channel
//! - [`dispatcher::emit`] helper per emettere eventi
//! - [`classifier`] regole hardcoded che decorano gli eventi con UiHint
//!
//! Pattern di riferimento: `crates/mcp-core/src/playwright_live.rs` (per-job),
//! generalizzato a per-project con ring buffer per replay e classifier opzionale.

pub mod classifier;
pub mod dispatcher;
pub mod enricher;
pub mod event;
pub mod llm_classifier;
pub mod ring_buffer;

pub use dispatcher::{ProjectChannel, ProjectChannels, RegistryHandle};
pub use enricher::{emit_and_tap, enricher_loop};
pub use event::{EnvelopedEvent, ProjectEvent, UiHint};
pub use llm_classifier::{EnrichmentDelta, LlmEnricher, NoOpEnricher};
