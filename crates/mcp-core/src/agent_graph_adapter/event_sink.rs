//! Adapter del trait [`nexus_agent_graph::runtime::ports::EventSink`].
//!
//! IMPLEMENTERA' (FASE 2) `EventSink::emit` (sincrono, infallibile, best-effort)
//! pubblicando l'evento sul canale SSE concreto verso il frontend chat
//! ([`nexus_events::ProjectChannels`]). Nessun gate `mode`: lo shadow usa un sink
//! no-op iniettato nel ctx (l'unica fonte di verita' verso l'utente resta il run
//! primario). Il run a cui gli eventi appartengono e' fissato alla costruzione.

use nexus_events::ProjectChannels;
use uuid::Uuid;

/// Adapter [`EventSink`] -> canale SSE [`ProjectChannels`].
///
/// F2 implementera' il trait `EventSink` su questa struct (mappa `SseEvent`
/// sull'evento del canale e fa `emit` best-effort).
pub struct SseEventSinkAdapter {
    /// Canale SSE concreto su cui `emit` pubblichera' in F2.
    channels: ProjectChannels,
    /// Run a cui gli eventi emessi appartengono (correlazione SSE).
    run_id: Uuid,
}

impl SseEventSinkAdapter {
    /// Costruisce l'adapter sul canale SSE concreto per un dato run.
    pub fn new(channels: ProjectChannels, run_id: Uuid) -> Self {
        Self { channels, run_id }
    }
}
