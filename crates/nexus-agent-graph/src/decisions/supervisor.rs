//! Supervisore worker: scheduling puro + parsing della risposta LLM.
//!
//! PUNTO UNICO (regola L) per:
//! - quando invocare il supervisore (`should_invoke`) in base a `SupervisorMode`;
//! - costruzione del riassunto step / blocco anomalie;
//! - validazione del JSON `{action: continue|redirect|abandon, ...}`.
//!
//! Il TASK del turno non si decide qui: la domanda ha un punto unico proprio in
//! [`crate::decisions::turn_task`], condiviso col focus del turno.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::decisions::loop_signatures::{
    build_signature, detect_signature_loop_with, LoopThresholds,
};
use crate::state::{AgentState, Message, MessageContent, SupervisorMode};

/// Soglia default step count per modalita' anomaly (override DB a monte).
pub const DEFAULT_ANOMALY_STEP_THRESHOLD: i64 = 20;

/// Intervallo default modalita' interleaved (override DB a monte).
pub const DEFAULT_INTERLEAVED_INTERVAL: i64 = 5;

/// Decisione strutturata del supervisore (enum chiuso, regola M).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorDecision {
    Continue,
    Redirect { message: String },
    Abandon { reason: String },
}

/// Segnali di anomalia rilevati deterministicamente dallo stato.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SupervisorAnomalies {
    pub high_step_count: bool,
    pub repeated_errors: bool,
    pub loop_detected: bool,
}

impl SupervisorAnomalies {
    pub fn any(&self) -> bool {
        self.high_step_count || self.repeated_errors || self.loop_detected
    }
}

/// Config DB-driven del supervisore (regola G: nessun hardcode nei nodi).
#[derive(Debug, Clone, Copy)]
pub struct SupervisorConfig {
    pub interleaved_interval: i64,
    pub anomaly_step_threshold: i64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            interleaved_interval: DEFAULT_INTERLEAVED_INTERVAL,
            anomaly_step_threshold: DEFAULT_ANOMALY_STEP_THRESHOLD,
        }
    }
}

/// `true` se il supervisore deve girare DOPO il tool_dispatch corrente.
pub fn should_invoke(
    mode: SupervisorMode,
    iterations: i64,
    cfg: SupervisorConfig,
    anomalies: &SupervisorAnomalies,
) -> bool {
    if iterations <= 0 {
        return false;
    }
    match mode {
        SupervisorMode::None => false,
        SupervisorMode::Continuous => true,
        SupervisorMode::Interleaved => {
            let n = cfg.interleaved_interval.max(1);
            iterations % n == 0
        }
        SupervisorMode::Anomaly => anomalies.any(),
    }
}

/// Rileva anomalie strutturate dallo stato (regola M: niente parsing prosa).
pub fn detect_anomalies(state: &AgentState, cfg: SupervisorConfig) -> SupervisorAnomalies {
    let iterations = state.iterations.unwrap_or(0);
    let high_step_count = iterations >= cfg.anomaly_step_threshold.max(1);

    let recent_errors = count_recent_tool_errors(&state.messages, 8);
    let repeated_errors = recent_errors >= 3;

    let loop_detected = detect_tool_loop(&state.messages);

    SupervisorAnomalies {
        high_step_count,
        repeated_errors,
        loop_detected,
    }
}

/// Riassunto compatto degli ultimi step per il prompt supervisore.
pub fn build_steps_summary(state: &AgentState, max_steps: usize) -> String {
    let mut lines: Vec<String> = state
        .meta_steps
        .iter()
        .rev()
        .take(max_steps)
        .rev()
        .map(|ms| format!("- [{}] {}", ms.kind, ms.title))
        .collect();

    if lines.is_empty() {
        lines = recent_tool_lines_from_messages(&state.messages, max_steps);
    }

    if lines.is_empty() {
        "(nessuno step registrato)".to_string()
    } else {
        lines.join("\n")
    }
}

/// Blocco anomalie per il placeholder `{{anomaly_block}}` del template.
pub fn build_anomaly_block(anomalies: &SupervisorAnomalies) -> String {
    if !anomalies.any() {
        return String::new();
    }
    let mut parts = Vec::new();
    if anomalies.high_step_count {
        parts.push("- Step count elevato (> soglia anomaly)");
    }
    if anomalies.repeated_errors {
        parts.push("- Errori ripetuti negli ultimi tool");
    }
    if anomalies.loop_detected {
        parts.push("- Loop di azioni ripetute rilevato");
    }
    format!("\nAnomalie rilevate:\n{}\n", parts.join("\n"))
}

/// Valida la risposta JSON del supervisore (punto unico, regola L).
pub fn validate_supervisor_response(v: &Value) -> SupervisorDecision {
    let action = v
        .get("action")
        .or_else(|| v.get("decision"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");

    match action.to_ascii_lowercase().as_str() {
        "continue" => SupervisorDecision::Continue,
        "redirect" => {
            let message = v
                .get("message")
                .or_else(|| v.get("instructions"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("Correggi l'approccio e prosegui con un metodo piu' efficiente.")
                .to_string();
            SupervisorDecision::Redirect { message }
        }
        "abandon" | "abort" => {
            let reason = v
                .get("reason")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("Task bloccato secondo valutazione supervisore.")
                .to_string();
            SupervisorDecision::Abandon { reason }
        }
        _ => SupervisorDecision::Continue,
    }
}

/// Chiave cache idempotente per iterazione (replay/resume).
pub fn supervisor_cache_key(iterations: i64) -> String {
    format!("supervisor_decision::{iterations}")
}

fn count_recent_tool_errors(messages: &[Message], window: usize) -> i64 {
    let mut errors = 0i64;
    for msg in messages.iter().rev().take(window) {
        if let Message::Tool { content, .. } = msg {
            let text = content.flatten_text();
            if text.contains("\"is_error\": true")
                || text.contains("\"is_error\":true")
                || text.starts_with("[Errore")
                || text.contains("\"error\"")
            {
                errors += 1;
            }
        }
    }
    errors
}

fn recent_tool_lines_from_messages(messages: &[Message], max: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for msg in messages.iter().rev() {
        if lines.len() >= max {
            break;
        }
        match msg {
            Message::Ai {
                content: MessageContent::Blocks(blocks),
                ..
            } => {
                for b in blocks {
                    if let crate::state::ContentBlock::ToolUse { name, .. } = b {
                        lines.push(format!("- tool {name}"));
                    }
                }
            }
            Message::Tool { content, .. } => {
                let preview: String = content.flatten_text().chars().take(120).collect();
                lines.push(format!("- result: {preview}"));
            }
            _ => {}
        }
    }
    lines.reverse();
    lines
}

fn detect_tool_loop(messages: &[Message]) -> bool {
    let mut signatures: Vec<String> = Vec::new();
    for msg in messages.iter().rev().take(12) {
        if let Message::Ai {
            content: MessageContent::Blocks(blocks),
            ..
        } = msg
        {
            for b in blocks {
                if let crate::state::ContentBlock::ToolUse { name, input, .. } = b {
                    signatures.push(build_signature(name, input));
                }
            }
        }
    }
    if signatures.len() < 3 {
        return false;
    }
    let recent = signatures[..signatures.len().saturating_sub(1)].to_vec();
    let new = signatures[signatures.len().saturating_sub(1)..].to_vec();
    let thresholds = LoopThresholds {
        signature: 3,
        cap: 12,
    };
    detect_signature_loop_with(&recent, &new, thresholds)
        .loop_signature
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MetaStep;

    #[test]
    fn continuous_invoke_ogni_iterazione() {
        let cfg = SupervisorConfig::default();
        assert!(should_invoke(
            SupervisorMode::Continuous,
            1,
            cfg,
            &SupervisorAnomalies::default()
        ));
        assert!(!should_invoke(
            SupervisorMode::None,
            5,
            cfg,
            &SupervisorAnomalies::default()
        ));
    }

    #[test]
    fn interleaved_ogni_n() {
        let cfg = SupervisorConfig {
            interleaved_interval: 5,
            ..Default::default()
        };
        assert!(!should_invoke(
            SupervisorMode::Interleaved,
            4,
            cfg,
            &SupervisorAnomalies::default()
        ));
        assert!(should_invoke(
            SupervisorMode::Interleaved,
            5,
            cfg,
            &SupervisorAnomalies::default()
        ));
    }

    #[test]
    fn anomaly_solo_con_segnali() {
        let cfg = SupervisorConfig::default();
        let quiet = SupervisorAnomalies::default();
        assert!(!should_invoke(SupervisorMode::Anomaly, 10, cfg, &quiet));

        let hot = SupervisorAnomalies {
            high_step_count: true,
            ..Default::default()
        };
        assert!(should_invoke(SupervisorMode::Anomaly, 10, cfg, &hot));
    }

    #[test]
    fn validate_json_redirect_e_abandon() {
        let redirect = validate_supervisor_response(&serde_json::json!({
            "action": "redirect",
            "message": "Usa replace_all"
        }));
        assert!(matches!(redirect, SupervisorDecision::Redirect { .. }));

        let abandon = validate_supervisor_response(&serde_json::json!({
            "action": "abandon",
            "reason": "Impossibile"
        }));
        assert!(matches!(abandon, SupervisorDecision::Abandon { .. }));
    }

    #[test]
    fn steps_summary_da_meta_steps() {
        let state = AgentState {
            meta_steps: vec![MetaStep {
                kind: "tool".into(),
                title: "edit_file — src/a.rs".into(),
                payload: Value::Null,
                correlation_id: None,
                created_at: None,
            }],
            ..Default::default()
        };
        let s = build_steps_summary(&state, 5);
        assert!(s.contains("edit_file"));
    }

}
