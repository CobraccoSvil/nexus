//! Adapter del trait [`nexus_agent_graph::runtime::ports::LlmGateway`].
//!
//! IMPLEMENTERA' (FASE 2) `LlmGateway::complete` delegando al gateway LLM concreto
//! di mcp-core ([`crate::nexus_gateway::NexusGatewayClient`], HTTP verso il Nexus
//! LLM Gateway sulla porta 4060 — catena Fallback DB-driven). Il provider/model
//! arrivano gia' risolti nella `LlmRequest` (regola G): l'adapter li inoltra al
//! gateway, mai li sceglie/hardcoda. RISCHIO NOTO (memoria progetto "Gateway
//! droppava tool_choice"): l'impl DEVE onorare `force_tool_choice` end-to-end.

use crate::nexus_gateway::NexusGatewayClient;

/// Adapter [`LlmGateway`] -> [`NexusGatewayClient`].
///
/// F2 implementera' il trait `LlmGateway` su questa struct (delega a
/// `NexusGatewayClient::complete`).
pub struct GatewayLlmAdapter {
    /// Client del gateway LLM concreto a cui la `complete` delegera' in F2.
    gateway: NexusGatewayClient,
}

impl GatewayLlmAdapter {
    /// Costruisce l'adapter sul client gateway concreto.
    pub fn new(gateway: NexusGatewayClient) -> Self {
        Self { gateway }
    }
}
