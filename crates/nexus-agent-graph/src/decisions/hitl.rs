//! Gate HITL (Human-in-the-Loop) per la modalita' Conferma.
//!
//! Punto unico (regola L): decide se sospendere il run prima dell'esecuzione di
//! tool mutativi quando `automation_mode` e' Confirm/None e l'utente non ha
//! ancora approvato (`approved != true`). La lista dei tool mutativi riusa
//! `fs_mutator_tools` (setting `agent.tools.result_cache_mutators`, mig 0394),
//! allineata al prompt `automation.mode_confirm_instruction` (mig 0420).

use serde_json::{json, Value};

use crate::state::{AutomationMode, TaskComplexity};

/// Chiave in `AgentState.extra` per le azioni pendenti serializzate (JSON array
/// con shape camelCase compatibile con `AgentPendingAction` lato mcp-core).
pub const HITL_PENDING_ACTIONS_EXTRA_KEY: &str = "hitl_pending_actions";

/// `true` se la modalita' automazione richiede conferma strutturale HITL.
pub fn automation_requires_hitl(mode: Option<AutomationMode>) -> bool {
    matches!(
        mode,
        Some(AutomationMode::Confirm) | Some(AutomationMode::None) | None
    )
}

/// `true` se il tool e' classificato come mutatore (write/run/service/...).
pub fn is_mutator_tool_name(name: &str, fs_mutator_tools: &[String]) -> bool {
    fs_mutator_tools.iter().any(|m| m == name)
}

/// `true` se almeno un pending tool_use e' mutativo.
pub fn pending_contains_mutator(pending: &[Value], fs_mutator_tools: &[String]) -> bool {
    pending.iter().any(|p| {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        is_mutator_tool_name(name, fs_mutator_tools)
    })
}

/// Decide se il tool_dispatch deve sospendere PRIMA di eseguire i pending.
pub fn should_suspend_for_hitl(
    automation_mode: Option<AutomationMode>,
    approved: Option<bool>,
    pending: &[Value],
    fs_mutator_tools: &[String],
) -> bool {
    if approved.unwrap_or(false) {
        return false;
    }
    if !automation_requires_hitl(automation_mode) {
        return false;
    }
    pending_contains_mutator(pending, fs_mutator_tools)
}

/// Costruisce l'array JSON delle azioni in attesa (solo tool mutativi).
pub fn build_pending_actions_json(pending: &[Value], fs_mutator_tools: &[String]) -> Vec<Value> {
    pending
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            let name = p.get("name").and_then(Value::as_str).unwrap_or("");
            is_mutator_tool_name(name, fs_mutator_tools)
        })
        .map(|(index, p)| {
            let name = p.get("name").and_then(Value::as_str).unwrap_or("");
            let input = p.get("input").cloned().unwrap_or_else(|| json!({}));
            azione(index, name, input)
        })
        .collect()
}

/// L'UNICO costruttore della shape wire `AgentPendingAction` (le chiavi
/// `AZIONE_*` hanno qui il loro solo punto di scrittura).
fn azione(index: usize, tool_name: &str, tool_input: Value) -> Value {
    let mut m = serde_json::Map::new();
    m.insert(AZIONE_INDEX.to_string(), json!(index));
    m.insert(AZIONE_TOOL_NAME.to_string(), json!(tool_name));
    m.insert(
        AZIONE_DESCRIPTION.to_string(),
        json!(format_pending_action_description(tool_name, &tool_input)),
    );
    m.insert(AZIONE_TOOL_INPUT.to_string(), tool_input);
    Value::Object(m)
}

fn format_pending_action_description(name: &str, input: &Value) -> String {
    if let Some(path) = input
        .get("path")
        .or_else(|| input.get("file_path"))
        .and_then(Value::as_str)
    {
        return format!("{name}({path})");
    }
    if let Some(cmd) = input
        .get("command")
        .or_else(|| input.get("cmd"))
        .and_then(Value::as_str)
    {
        let short = if cmd.chars().count() > 80 {
            format!("{}...", cmd.chars().take(77).collect::<String>())
        } else {
            cmd.to_string()
        };
        return format!("{name}({short})");
    }
    name.to_string()
}

/// Nome sintetico della pending action di approvazione del PIANO: non e' un
/// tool del catalogo, e' il contratto fra il planner che sospende, il confirm
/// handler che riconosce (e scrive `approved_at/approved_by`) e la card UI.
pub const PLAN_APPROVAL_ACTION: &str = "plan_approval";

/// Chiavi wire della shape `AgentPendingAction` (camelCase, lette verbatim da
/// mcp-core e dalla UI): il contratto ha UN punto di scrittura — i due builder
/// qui sotto — e chi lo LEGGE (es. il confirm handler) riusa queste costanti,
/// mai un letterale proprio.
pub const AZIONE_INDEX: &str = "index";
pub const AZIONE_TOOL_NAME: &str = "toolName";
pub const AZIONE_TOOL_INPUT: &str = "toolInput";
pub const AZIONE_DESCRIPTION: &str = "description";

/// Rango di severita' della complessita' per il confronto con la soglia.
fn rango(c: TaskComplexity) -> u8 {
    match c {
        TaskComplexity::Low => 1,
        TaskComplexity::Medium => 2,
        TaskComplexity::High => 3,
    }
}

/// Decide se il PLANNER deve sospendere per l'approvazione umana del piano
/// (gemello di [`should_suspend_for_hitl`], che presidia i TOOL: due domande
/// diverse, due predicati — approvare il piano NON approva i mutatori).
///
/// Scatta SOLO in Confirm: in Automatic/Continuous l'utente ha scelto
/// l'autonomia (regola D), in Study non si scrive nulla e None/assente resta
/// al gate sui tool. `task_complexity` assente = NON scatta (fail-open: la
/// rete e' il gate HITL sui mutatori, che in Confirm sospende comunque);
/// `subagent_depth > 0` = il padre ha gia' l'approvazione.
pub fn should_suspend_for_plan_approval(
    gate_enabled: bool,
    automation_mode: Option<AutomationMode>,
    plan_approved: Option<bool>,
    task_complexity: Option<TaskComplexity>,
    min_complexity: TaskComplexity,
    subagent_depth: Option<i64>,
) -> bool {
    if !gate_enabled || plan_approved.unwrap_or(false) {
        return false;
    }
    if !matches!(automation_mode, Some(AutomationMode::Confirm)) {
        return false;
    }
    if subagent_depth.unwrap_or(0) > 0 {
        return false;
    }
    match task_complexity {
        Some(c) => rango(c) >= rango(min_complexity),
        None => false,
    }
}

/// La pending action del piano, nella STESSA shape delle azioni tool
/// (`AgentPendingAction` camelCase): il canale di sospensione/resume/UI e'
/// quello esistente, cambia solo il contenuto. `coverage` = (voci con almeno
/// un criterio eseguibile, voci totali): l'utente approva VEDENDO quanto del
/// piano ha una verifica automatica.
pub fn build_plan_approval_action(
    run_id: &str,
    todos: &[Value],
    coverage: (usize, usize),
) -> Value {
    let (con_criteri, totali) = coverage;
    let mut input = serde_json::Map::new();
    input.insert("run_id".to_string(), json!(run_id));
    input.insert("todos".to_string(), json!(todos));
    input.insert("executableCriteria".to_string(), json!(con_criteri));
    input.insert("totalItems".to_string(), json!(totali));
    let mut m = azione(0, PLAN_APPROVAL_ACTION, Value::Object(input));
    // La description del piano e' piu' parlante di quella derivata dal tool.
    if let Value::Object(map) = &mut m {
        map.insert(
            AZIONE_DESCRIPTION.to_string(),
            json!(format!(
                "Approva il piano ({totali} passi, {con_criteri} con verifica automatica)"
            )),
        );
    }
    m
}

/// Copertura dei criteri eseguibili: quante voci del piano hanno almeno un
/// criterio col tipo nel vocabolario (`PLAN_CRITERION_TYPES`). E' una misura
/// per l'approvazione informata, MAI un enforcement (quello e' del verifier).
pub fn plan_criteria_coverage(todos: &[Value]) -> (usize, usize) {
    let eseguibile = |c: &Value| {
        c.get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| crate::runtime::ports::PLAN_CRITERION_TYPES.contains(&t))
    };
    let con_criteri = todos
        .iter()
        .filter(|t| {
            t.get("acceptance_criteria")
                .and_then(Value::as_array)
                .is_some_and(|arr| arr.iter().any(eseguibile))
        })
        .count();
    (con_criteri, todos.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutators() -> Vec<String> {
        vec!["write_file".into(), "run_command".into()]
    }

    #[test]
    fn confirm_richiede_hitl_su_mutator_automatic_no() {
        assert!(automation_requires_hitl(Some(AutomationMode::Confirm)));
        assert!(!automation_requires_hitl(Some(AutomationMode::Automatic)));
        assert!(!automation_requires_hitl(Some(AutomationMode::Continuous)));
    }

    #[test]
    fn should_suspend_solo_con_mutator_e_confirm() {
        let pending = vec![json!({"name": "write_file", "input": {"path": "a.rs"}})];
        assert!(should_suspend_for_hitl(
            Some(AutomationMode::Confirm),
            None,
            &pending,
            &mutators(),
        ));
        assert!(!should_suspend_for_hitl(
            Some(AutomationMode::Automatic),
            None,
            &pending,
            &mutators(),
        ));
        assert!(!should_suspend_for_hitl(
            Some(AutomationMode::Confirm),
            Some(true),
            &pending,
            &mutators(),
        ));
    }

    #[test]
    fn read_only_pending_non_sospende() {
        let pending = vec![json!({"name": "read_file", "input": {"path": "a.rs"}})];
        assert!(!should_suspend_for_hitl(
            Some(AutomationMode::Confirm),
            None,
            &pending,
            &mutators(),
        ));
    }

    /// Il gate del piano scatta solo in Confirm, sopra soglia, alla profondita'
    /// zero e mai due volte (plan_approved=true al resume lo spegne).
    ///
    /// MUTAZIONE: allargare il matches! ad Automatic (o togliere il controllo
    /// su plan_approved) fa cadere le asserzioni negative.
    #[test]
    fn plan_approval_scatta_solo_in_confirm_sopra_soglia() {
        let base = |mode, approved, complexity, depth| {
            should_suspend_for_plan_approval(
                true,
                mode,
                approved,
                complexity,
                TaskComplexity::Medium,
                depth,
            )
        };
        assert!(base(Some(AutomationMode::Confirm), None, Some(TaskComplexity::Medium), None));
        assert!(base(Some(AutomationMode::Confirm), None, Some(TaskComplexity::High), Some(0)));
        // Low sotto soglia; Automatic mai; gia' approvato mai; sub-run mai;
        // complessita' ignota = fail-open (la rete e' il gate sui tool).
        assert!(!base(Some(AutomationMode::Confirm), None, Some(TaskComplexity::Low), None));
        assert!(!base(Some(AutomationMode::Automatic), None, Some(TaskComplexity::High), None));
        assert!(!base(Some(AutomationMode::Confirm), Some(true), Some(TaskComplexity::High), None));
        assert!(!base(Some(AutomationMode::Confirm), None, Some(TaskComplexity::High), Some(1)));
        assert!(!base(Some(AutomationMode::Confirm), None, None, None));
        // Kill-switch.
        assert!(!should_suspend_for_plan_approval(
            false,
            Some(AutomationMode::Confirm),
            None,
            Some(TaskComplexity::High),
            TaskComplexity::Medium,
            None,
        ));
    }

    /// La coverage conta le voci con almeno un criterio ESEGUIBILE (vocabolario
    /// del contratto), mai i criteri fuori vocabolario.
    #[test]
    fn coverage_conta_solo_criteri_del_vocabolario() {
        let todos = vec![
            json!({"content": "a", "acceptance_criteria": [{"type": "run_command", "command": "x"}]}),
            json!({"content": "b", "acceptance_criteria": [{"type": "db_query"}]}),
            json!({"content": "c"}),
        ];
        assert_eq!(plan_criteria_coverage(&todos), (1, 3));
        let azione = build_plan_approval_action("run-1", &todos, plan_criteria_coverage(&todos));
        assert_eq!(azione["toolName"], PLAN_APPROVAL_ACTION);
        assert_eq!(azione["toolInput"]["totalItems"], 3);
        assert!(azione["description"].as_str().unwrap_or_default().contains("3 passi"));
    }

    #[test]
    fn build_pending_actions_solo_mutators() {
        let pending = vec![
            json!({"name": "read_file", "input": {"path": "x.rs"}}),
            json!({"name": "write_file", "input": {"path": "y.rs"}}),
        ];
        let actions = build_pending_actions_json(&pending, &mutators());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["toolName"], "write_file");
        assert_eq!(actions[0]["description"], "write_file(y.rs)");
    }
}
