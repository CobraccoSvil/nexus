//! Config DB-driven del routing, PASSATA come parametro (regola G: nessuna
//! lettura DB qui dentro, nessun fallback hardcoded di emergenza nella logica).
//!
//! Raccoglie in un solo struct i settings che in Python vengono letti da
//! `orchestrator_config.get()` / `_load_g1_max_nudges()` /
//! `_load_tool_choice_forcing_config()` / `_load_pending_steps_config()`.
//! I [`Default`] replicano i `_SAFE_DEFAULTS` documentati del brain: valgono
//! SOLO quando il DB non e' raggiungibile (stessa semantica del Python), mai
//! come "magic fallback" dentro la logica decisionale.

use serde::{Deserialize, Serialize};

/// Tutta la config DB-driven necessaria alle `route_after_*`.
///
/// Mappa i settings letti dal brain Python:
///   - `g1_max_nudges`            -> `agent.g1_max_nudges` (default 3)
///   - `tool_choice_forcing_*`    -> `agent.tool_choice_forcing_{enabled,max_iteration}`
///   - `verifier_enabled`         -> `agent.verifier.enabled` (default false)
///   - `dag_parallel_enabled`     -> `agent.dag.parallel_enabled` (default false)
///   - `final_gate_enabled`       -> `agent.final_gate.enabled` (default true)
///   - `final_gate_max_cycles`    -> `agent.final_gate.max_cycles` (default 2)
///   - `final_gate_software_intents` -> `agent.final_gate.software_intents`
///   - `todo_isolation_enabled`   -> `agent.continuous.todo_isolation_enabled` (default false)
///   - `pending_steps_*`          -> `agent.closure.pending_steps_*`
///   - `fs_mutator_tools`         -> `agent.tools.result_cache_mutators`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Numero massimo di re-routing G1 verso executor per run (`g1_max_nudges`).
    pub g1_max_nudges: i64,
    /// Flag globale del tool_choice forcing (`tool_choice_forcing_enabled`).
    pub tool_choice_forcing_enabled: bool,
    /// Soglia iterazione oltre cui NON si forza piu' (`tool_choice_forcing_max_iteration`).
    pub tool_choice_forcing_max_iteration: i64,
    /// Verifier attivo (plan_phase + verifier -> verifier node).
    pub verifier_enabled: bool,
    /// DAG parallelo attivo (prevale su todo_isolation in route_after_planner).
    pub dag_parallel_enabled: bool,
    /// Final gate generale abilitato.
    pub final_gate_enabled: bool,
    /// Cap di cicli del final gate.
    pub final_gate_max_cycles: i64,
    /// Whitelist intent "software" (lower-case) per `_is_software_task`.
    pub final_gate_software_intents: Vec<String>,
    /// Esecuzione todo come sub-run isolate abilitata (`todo_isolation_active`).
    pub todo_isolation_enabled: bool,
    /// Rilevamento report con passi pendenti abilitato.
    pub pending_steps_detection_enabled: bool,
    /// Numero minimo di item per considerare un testo "report con TODO".
    pub pending_steps_min_items: i64,
    /// Tool che MUTANO il filesystem/progetto (per `has_filesystem_mutation_in_history`).
    /// Punto unico dei DATI: setting `agent.tools.result_cache_mutators` (mig 0394).
    pub fs_mutator_tools: Vec<String>,
    /// Cap effettivo di superstep del motore di grafo (`GraphEngine`): anti-loop
    /// infinito del grafo (NON delle iterazioni agentiche, che hanno il loro
    /// `iter_cap`). Valorizzato a runtime da
    /// [`effective_recursion_limit`] (punto unico): `max(pavimento DB
    /// `agent.graph.recursion_limit`, stima topologica da `iteration_cap` +
    /// nodi stall/G1/final_gate + margine contorno). Un grafo che non converge
    /// si ferma qui invece di girare per sempre.
    pub recursion_limit: u32,
}

/// Input per il calcolo del cap effettivo di superstep (tutti i valori dal
/// wiring/DB, regola G: nessuna lettura DB in questa funzione).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphTopologyLimits {
    /// Pavimento DB (`agent.graph.recursion_limit`): il cap non scende mai sotto.
    pub db_floor: u32,
    /// Cap iterazioni executor (`agent.executor.iteration_cap`).
    pub iteration_cap: i64,
    /// Meta-reasoner stall recovery attivo (`agent.stall_recovery.enabled`).
    pub stall_recovery_enabled: bool,
    /// Max mosse stall per sessione (`agent.stall_recovery.max_moves_per_session`).
    pub stall_max_moves: i64,
    /// Max re-routing G1 (`agent.g1_max_nudges`).
    pub g1_max_nudges: i64,
    /// Max cicli final gate (`agent.final_gate.max_cycles`).
    pub final_gate_max_cycles: i64,
}

/// Pavimento topologico: quanti superstep un run puo' consumare al massimo prima
/// che `iteration_cap` o i nodi di chiusura lo fermino in modo ordinato.
///
/// Formula (punto unico, regola L):
/// - `iteration_cap × 3` — executor↔tool_dispatch + deviazioni occasionali
///   (verifier, scale, G1 inline) per iterazione;
/// - `stall_max_moves × 2` — stall_recovery→executor (solo se enabled);
/// - `g1_max_nudges × 2` — g1_continue→executor;
/// - `final_gate_max_cycles × 4` — final_gate↔executor;
/// - `CONTOUR_MARGIN` — router/clarify/understanding/planner/reflection/learner.
///
/// Ritorna `max(db_floor, topology_floor)`.
pub fn effective_recursion_limit(limits: &GraphTopologyLimits) -> u32 {
    const SUPERSTEPS_PER_ITERATION: i64 = 3;
    const SUPERSTEPS_PER_STALL_RECOVERY: i64 = 2;
    const SUPERSTEPS_PER_G1_NUDGE: i64 = 2;
    const SUPERSTEPS_PER_FINAL_GATE_CYCLE: i64 = 4;
    const CONTOUR_MARGIN: i64 = 25;

    let mut topology = limits
        .iteration_cap
        .max(0)
        .saturating_mul(SUPERSTEPS_PER_ITERATION)
        .saturating_add(CONTOUR_MARGIN)
        .saturating_add(
            limits
                .g1_max_nudges
                .max(0)
                .saturating_mul(SUPERSTEPS_PER_G1_NUDGE),
        )
        .saturating_add(
            limits
                .final_gate_max_cycles
                .max(0)
                .saturating_mul(SUPERSTEPS_PER_FINAL_GATE_CYCLE),
        );

    if limits.stall_recovery_enabled {
        topology = topology.saturating_add(
            limits
                .stall_max_moves
                .max(0)
                .saturating_mul(SUPERSTEPS_PER_STALL_RECOVERY),
        );
    }

    limits.db_floor.max(topology.max(0) as u32)
}

impl Default for RoutingConfig {
    fn default() -> Self {
        // Default IDENTICI ai `_SAFE_DEFAULTS` del brain (orchestrator_config.py),
        // ai default di `_load_g1_max_nudges` / `_load_tool_choice_forcing_config`
        // / `_load_pending_steps_config` e a `_FS_MUTATORS_DEFAULT`.
        Self {
            g1_max_nudges: 3,
            tool_choice_forcing_enabled: true,
            tool_choice_forcing_max_iteration: 2,
            verifier_enabled: false,
            dag_parallel_enabled: false,
            final_gate_enabled: true,
            final_gate_max_cycles: 2,
            final_gate_software_intents: [
                "code",
                "debug",
                "scaffold",
                "implement",
                "build",
                "frontend",
                "fix",
                "refactor",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            todo_isolation_enabled: false,
            pending_steps_detection_enabled: true,
            pending_steps_min_items: 2,
            fs_mutator_tools: _FS_MUTATORS_DEFAULT
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            // Pavimento safe-DB-down: a runtime `effective_recursion_limit` lo
            // scala sulla topologia (iteration_cap + stall/G1/final_gate).
            recursion_limit: 150,
        }
    }
}

/// CSV identico a `_FS_MUTATORS_DEFAULT` Python (e a `MUTATORS_DEFAULT` in
/// `crates/mcp-core/src/agent_tool_result_cache.rs`, mig 0394). Il punto unico
/// dei DATI e' il setting DB condiviso; questo default serve solo se la chiave
/// manca o il DB e' irraggiungibile.
const _FS_MUTATORS_DEFAULT: &str = "write_file,edit_file,delete_file,rename_file,file_write,fs_copy,fs_mkdir,fs_move,format_file,run_lint_fix,run_command,command,run_in_terminal,git_command,git_pull,git_commit,git_stage,git_push,nexus_extract_figma_code,nexus_install_shadcn_components,nexus_mcp_tool_call,cargo_install,run_service,service_restart,stop_service";

#[cfg(test)]
mod tests {
    use super::*;

    fn prod_defaults() -> GraphTopologyLimits {
        GraphTopologyLimits {
            db_floor: 150,
            iteration_cap: 60,
            stall_recovery_enabled: true,
            stall_max_moves: 6,
            g1_max_nudges: 3,
            final_gate_max_cycles: 2,
        }
    }

    #[test]
    fn topology_prod_default_supera_db_floor() {
        let eff = effective_recursion_limit(&prod_defaults());
        assert!(
            eff > 150,
            "con iteration_cap=60 e stall_recovery ON il cap deve superare il floor 150, got {eff}"
        );
        assert_eq!(eff, 231);
    }

    #[test]
    fn db_floor_alto_vince_su_topology() {
        let mut limits = prod_defaults();
        limits.db_floor = 500;
        assert_eq!(effective_recursion_limit(&limits), 500);
    }

    #[test]
    fn iteration_cap_alto_scala_linearemente() {
        let mut limits = prod_defaults();
        limits.iteration_cap = 300;
        let eff = effective_recursion_limit(&limits);
        assert!(eff >= 950, "eff={eff}");
    }

    #[test]
    fn stall_recovery_off_riduce_margine() {
        let mut limits = prod_defaults();
        limits.stall_recovery_enabled = false;
        assert_eq!(effective_recursion_limit(&limits), 219);
    }
}
