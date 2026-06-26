//! Adapter del trait [`nexus_agent_graph::runtime::ports::CriteriaRunner`].
//!
//! IMPLEMENTERA' (FASE 2) `CriteriaRunner::run` eseguendo i criteri generali del
//! final gate (`no_orphan_imported` / `outputs_exist` / `service_logs_clean` /
//! `run_command`-build) delegando ai `_check_*` concreti (parita' con
//! `brain/agents/criteria_runner.py`), che a loro volta usano il ToolRunner gRPC
//! ([`crate::tool_runner_server::ToolRunnerDeps`]). In modalita' shadow i criteri
//! girano in [`ExecMode::Replay`] (rileggono i tool_result del primario = zero
//! side-effect). Un criterio fallito NON propaga un errore: diventa un
//! `CriterionResult { passed: false, evidence.error }` (parita' col try/except Python).

use crate::tool_runner_server::ToolRunnerDeps;

/// Adapter [`CriteriaRunner`] -> `_check_*` su ToolRunner gRPC.
///
/// F2 implementera' il trait `CriteriaRunner` su questa struct.
pub struct FinalGateCriteriaRunnerAdapter {
    /// Dipendenze del ToolRunner concreto su cui i `_check_*` gireranno in F2.
    deps: ToolRunnerDeps,
}

impl FinalGateCriteriaRunnerAdapter {
    /// Costruisce il runner sui criteri delegando al ToolRunner concreto.
    pub fn new(deps: ToolRunnerDeps) -> Self {
        Self { deps }
    }
}
