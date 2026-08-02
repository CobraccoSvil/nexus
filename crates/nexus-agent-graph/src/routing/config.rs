//! Config DB-driven del routing, PASSATA come parametro (regola G: nessuna
//! lettura DB qui dentro, nessun fallback hardcoded di emergenza nella logica).
//!
//! Raccoglie in un solo struct i settings che in Python vengono letti da
//! `orchestrator_config.get()` / `_load_g1_max_nudges()` /
//! `_load_tool_choice_forcing_config()`.
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
///   - `fs_mutator_tools`         -> `agent.tools.result_cache_mutators`
///   - `iteration_cap`            -> `agent.executor.iteration_cap` (default 60)
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
    /// Tool che MUTANO il filesystem/progetto (per `has_filesystem_mutation_in_history`).
    /// Punto unico dei DATI: setting `agent.tools.result_cache_mutators` (mig 0394).
    pub fs_mutator_tools: Vec<String>,
    /// Cap di ITERAZIONI agentiche del run: oltre la soglia `route_after_*` non
    /// rilancia piu' l'executor e instrada alla chiusura (passando prima dalla
    /// verifica oggettiva, vedi `route_after_executor`). NON e' il cap di
    /// superstep del grafo, che e' [`RoutingConfig::recursion_limit`].
    ///
    /// Fonte: setting `agent.executor.iteration_cap`, lo STESSO che governa la
    /// chiusura d'autorita' DENTRO l'executor (`ExecutorConfig::iteration_cap`):
    /// una sola domanda, un solo valore. Prima il routing lo teneva in una
    /// costante di compile-time e il setting non lo governava, quindi i due lati
    /// della stessa soglia potevano divergere (in produzione: 60 contro 100).
    ///
    /// `serde(default)` perche' i casi golden serializzati non portano il campo:
    /// il valore di ripiego e' quello DICHIARATO da [`Default`], non un valore
    /// diverso deciso qui.
    ///
    /// INNESTO del wiring DB: `mcp-core::native_engine::load_routing_config`
    /// legge GIA' `agent.executor.iteration_cap` nella variabile locale
    /// `iteration_cap` (le serve per `effective_recursion_limit`) e costruisce
    /// la struct chiudendo con `..RoutingConfig::default()`. Finche' quel
    /// letterale non elenca anche `iteration_cap,`, il routing resta al default
    /// dichiarato qui e il valore DB non lo governa.
    #[serde(default = "default_iteration_cap")]
    pub iteration_cap: i64,
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
    /// Quante volte il budget di iterazioni puo' essere RICONCESSO
    /// (`agent.executor.max_escalations`): un'escalation azzera `iterations`
    /// (executor, `reset_iterations=true`) e il run riparte con un budget
    /// pieno. Il tetto dei superstep deve budgetarle tutte, o la promessa del
    /// reset e' impagabile per costruzione.
    pub max_escalations: i64,
}

/// Pavimento topologico: quanti superstep un run puo' consumare al massimo prima
/// che `iteration_cap` o i nodi di chiusura lo fermino in modo ordinato.
///
/// Formula (punto unico, regola L):
/// - `iteration_cap × (max_escalations + 1) × 3` — executor↔tool_dispatch +
///   deviazioni occasionali (verifier, scale, G1 inline) per iterazione. Il
///   moltiplicatore delle escalation e' il pezzo che mancava: un'escalation
///   AZZERA `iterations` e riconcede il budget pieno, quindi le iterazioni
///   concedibili in totale sono `cap × (escalations + 1)`, non `cap`. Senza
///   quel fattore il tetto budgetava UNA sola concessione, e ogni run che
///   resettava oltre meta' corsa moriva a meta' del budget promesso — misurato
///   sui run 8ec6f5bf e 99fab373 (bacheca-attivita): reset a superstep ~214,
///   morte per `recursion_limit` a 350 con un RIMANDO IN CORREZIONE pendente,
///   cioe' il run ucciso mentre stava rilavorando, e chiuso come errore del
///   motore. Al reset restavano ~65 iterazioni delle 100 appena riconcesse:
///   la promessa era impagabile per costruzione, non per sfortuna;
/// - `stall_max_moves × 2` — stall_recovery→executor (solo se enabled);
/// - `g1_max_nudges × 2` — g1_continue→executor;
/// - `final_gate_max_cycles × 4` — final_gate↔executor;
/// - `CONTOUR_MARGIN` — router/clarify/understanding/planner/reflection/learner.
///
/// Ritorna `max(db_floor, topology_floor)`.
///
/// NOTA DI SCOPO: questo tetto non e' un controllo di spesa (uccide senza
/// chiusura). Il freno alla spesa e' l'hard-cap token/costo del run; se piu'
/// budget consecutivi sono ritenuti troppo costosi, la leva e'
/// `agent.executor.max_escalations`, non questo pavimento.
pub fn effective_recursion_limit(limits: &GraphTopologyLimits) -> u32 {
    const SUPERSTEPS_PER_ITERATION: i64 = 3;
    const SUPERSTEPS_PER_STALL_RECOVERY: i64 = 2;
    const SUPERSTEPS_PER_G1_NUDGE: i64 = 2;
    const SUPERSTEPS_PER_FINAL_GATE_CYCLE: i64 = 4;
    const CONTOUR_MARGIN: i64 = 25;

    // Budget di iterazioni CONCEDIBILI nel run intero: quello iniziale piu'
    // una riconcessione per ogni escalation ammessa.
    let budget_concedibili = limits
        .iteration_cap
        .max(0)
        .saturating_mul(limits.max_escalations.max(0).saturating_add(1));

    let mut topology = budget_concedibili
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

/// Cap iterazioni quando il wiring non ha passato il valore DB (safe-default
/// DB-down, come gli altri campi di [`RoutingConfig::default`]). Identico a
/// `ExecutorConfig::default().iteration_cap`: le due config rispondono alla
/// STESSA domanda e non devono divergere nemmeno a DB spento — invariante
/// coperta dal test `iteration_cap_default_allineato_all_executor`.
const ITERATION_CAP_DEFAULT: i64 = 60;

/// Ripiego di serde per [`RoutingConfig::iteration_cap`]: rimanda al default
/// dichiarato, mai a un valore proprio.
fn default_iteration_cap() -> i64 {
    ITERATION_CAP_DEFAULT
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
            fs_mutator_tools: _FS_MUTATORS_DEFAULT
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            // Pavimento safe-DB-down: a runtime `effective_recursion_limit` lo
            // scala sulla topologia (iteration_cap + stall/G1/final_gate).
            recursion_limit: 150,
            iteration_cap: ITERATION_CAP_DEFAULT,
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
            // Allineato a `ExecutorConfig::default().max_escalations`: e' lo
            // stesso setting (`agent.executor.max_escalations`) e l'invariante
            // e' coperta da `max_escalations_default_allineato_all_executor`.
            max_escalations: 3,
        }
    }

    /// Stesso patto di `iteration_cap_default_allineato_all_executor`: il
    /// numero di riconcessioni che il TETTO budgeta e quello che l'EXECUTOR
    /// concede leggono lo stesso setting, e i safe-default non possono
    /// divergere o a DB spento il tetto tornerebbe a budgetare meno budget di
    /// quanti l'executor ne conceda — cioe' il difetto che questo campo chiude.
    #[test]
    fn max_escalations_default_allineato_all_executor() {
        assert_eq!(
            prod_defaults().max_escalations,
            crate::nodes::ExecutorConfig::default().max_escalations
        );
    }

    /// IL test del difetto (run 8ec6f5bf/99fab373): il tetto deve pagare TUTTI
    /// i budget concedibili, non il primo. Coi valori vivi del DB al momento
    /// della misura (cap=100, escalations=3, tutto il resto ai default) un run
    /// che esaurisce il primo budget e viene riconcesso deve avere ancora
    /// spazio per gli altri tre cicli pieni.
    ///
    /// MUTAZIONE: togliere il moltiplicatore (tornare a `iteration_cap` secco)
    /// fa scendere il tetto a ~351 e la prima asserzione rosseggia con il
    /// valore del difetto reale (morte a 350 col rimando pendente).
    #[test]
    fn il_tetto_budgeta_ogni_riconcessione_del_budget() {
        let limits = GraphTopologyLimits {
            db_floor: 200,
            iteration_cap: 100,
            stall_recovery_enabled: true,
            stall_max_moves: 6,
            g1_max_nudges: 3,
            final_gate_max_cycles: 2,
            max_escalations: 3,
        };
        let eff = effective_recursion_limit(&limits);
        // 4 budget da 100 iterazioni × ~3 superstep l'una: il tetto deve
        // coprirli tutti (il margine di contorno fa il resto).
        assert!(
            eff as i64 >= 100 * 4 * 3,
            "il tetto ({eff}) non copre i 4 budget concedibili: un run \
             riconcesso muore a meta' del budget promesso"
        );
        // E con zero escalation ammesse torna il tetto di una concessione sola.
        let mut una_sola = limits;
        una_sola.max_escalations = 0;
        assert!(effective_recursion_limit(&una_sola) < eff);
    }

    /// Le due config che rispondono a "quante iterazioni puo' fare questo run"
    /// (il ROUTING che smette di rilanciare l'executor, e l'EXECUTOR che chiude
    /// d'autorita') leggono lo stesso setting: i loro safe-default non possono
    /// divergere, o a DB spento il grafo e il nodo si fermerebbero a soglie
    /// diverse. Il confronto attraversa i due PRODUTTORI veri (regola O), non
    /// due letterali ricopiati.
    #[test]
    fn iteration_cap_default_allineato_all_executor() {
        assert_eq!(
            RoutingConfig::default().iteration_cap,
            crate::nodes::ExecutorConfig::default().iteration_cap
        );
    }

    #[test]
    fn topology_prod_default_supera_db_floor() {
        let eff = effective_recursion_limit(&prod_defaults());
        assert!(
            eff > 150,
            "con iteration_cap=60 e stall_recovery ON il cap deve superare il floor 150, got {eff}"
        );
        // 60 × (3+1) budget × 3 superstep + 25 margine + 6 g1 + 8 gate + 12 stall.
        assert_eq!(eff, 771);
    }

    #[test]
    fn db_floor_alto_vince_su_topology() {
        // Il pavimento deve stare SOPRA la topologia corrente (771 coi default
        // di produzione) perche' il test misuri davvero "il floor piu' alto
        // vince": con un floor sotto la topologia vincerebbe questa, e il test
        // asserirebbe un'altra cosa.
        let mut limits = prod_defaults();
        limits.db_floor = 5000;
        assert_eq!(effective_recursion_limit(&limits), 5000);
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
        assert_eq!(effective_recursion_limit(&limits), 759);
    }
}
