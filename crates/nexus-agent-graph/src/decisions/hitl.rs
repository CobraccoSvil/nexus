//! Gate HITL (Human-in-the-Loop) per la modalita' Conferma.
//!
//! Punto unico (regola L): decide se sospendere il run prima dell'esecuzione di
//! tool mutativi quando `automation_mode` e' Confirm/None e l'utente non ha
//! ancora approvato (`approved != true`). La lista dei tool mutativi riusa
//! `fs_mutator_tools` (setting `agent.tools.result_cache_mutators`, mig 0394),
//! allineata al prompt `automation.mode_confirm_instruction` (mig 0420).

use serde_json::{json, Value};

use crate::state::AutomationMode;

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
            json!({
                "index": index,
                "toolName": name,
                "toolInput": input,
                "description": format_pending_action_description(name, &input),
            })
        })
        .collect()
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
