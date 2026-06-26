//! Adapter del trait [`nexus_agent_graph::runtime::ports::ToolExecutor`].
//!
//! IMPLEMENTERA' (FASE 2) `ToolExecutor::execute` delegando:
//! - in [`ExecMode::Real`] al ToolRunner concreto di mcp-core
//!   ([`crate::tool_runner_server::ToolRunnerDeps`] -> `execute_agent_tool`),
//!   che esegue davvero il tool (side-effect possibili sul progetto);
//! - in [`ExecMode::Replay`] (modalita' shadow) a un lettore dei `tool_result`
//!   del run PRIMARIO (rilettura da `agent_steps`, ZERO side-effect).
//!
//! L'`exit_code` strutturato del `ToolOutcome` DEVE fluire INVARIATO nel
//! `ContentBlock::ToolResult` (vedi doc del trait: alimenta
//! `routing::signals::tool_result_outcome_after`).

use crate::tool_runner_server::ToolRunnerDeps;

/// Adapter [`ToolExecutor`] -> ToolRunner gRPC (Real) + replay tool_result (Replay).
///
/// F2 implementera' il trait `ToolExecutor` su questa struct.
pub struct ToolRunnerExecutorAdapter {
    /// Dipendenze del ToolRunner concreto (db, neural, channels...) a cui
    /// l'esecuzione `Real` delegera' in F2; lo stesso `db` serve la rilettura
    /// `Replay` dei tool_result del primario.
    deps: ToolRunnerDeps,
}

impl ToolRunnerExecutorAdapter {
    /// Costruisce l'adapter sulle dipendenze del ToolRunner concreto.
    pub fn new(deps: ToolRunnerDeps) -> Self {
        Self { deps }
    }
}
