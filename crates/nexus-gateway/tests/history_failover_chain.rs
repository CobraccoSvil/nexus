//! Regressione: history tool-heavy attraverso sanificazione cross-provider.
//!
//! Simula la catena dell'incidente f0ad0337 (DeepSeek 400 -> failover Mistral):
//! history con reasoning DeepSeek + tool-call incompleta viene sanificata per
//! il dialetto Mistral senza message_order error.

use nexus_gateway::history_sanitizer::{self, SanitizeMode};
use nexus_gateway::types::{
    LlmMessage, LlmToolCall, MessageContent, ToolFunctionCall,
};

fn assistant_tool(id: &str, reasoning: &str) -> LlmMessage {
    LlmMessage {
        role: "assistant".to_string(),
        content: MessageContent::Text(String::new()),
        tool_call_id: None,
        tool_calls: Some(vec![LlmToolCall {
            id: id.to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "read_file".to_string(),
                arguments: r#"{"path":"a.rs"}"#.to_string(),
            },
            thought_signature: Some("gemini-sig".to_string()),
        }]),
        name: None,
        thinking_signature: Some("anthropic-sig".to_string()),
        reasoning: Some(reasoning.to_string()),
    }
}

fn trailing_assistant_text() -> LlmMessage {
    LlmMessage {
        role: "assistant".to_string(),
        content: MessageContent::Text("solo testo post-summary".to_string()),
        tool_call_id: None,
        tool_calls: None,
        name: None,
        thinking_signature: None,
        reasoning: Some("deepseek-reasoning-residue".to_string()),
    }
}

#[test]
fn tool_heavy_history_failover_deepseek_to_mistral() {
    let mut messages = vec![
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text("leggi a.rs".to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        },
        assistant_tool("call_1", "chain-of-thought deepseek"),
        trailing_assistant_text(),
    ];

    let report =
        history_sanitizer::sanitize_history(&mut messages, "mistral", SanitizeMode::Aggressive);

    assert!(report.stripped_reasoning >= 1);
    assert!(
        report.stripped_trailing_assistant >= 1 || messages.last().unwrap().role != "assistant"
    );
    assert!(messages.iter().all(|m| m.reasoning.is_none()));
    assert!(messages.last().unwrap().role == "user" || messages.last().unwrap().role == "tool");
    assert!(messages
        .iter()
        .any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call_1")));
}

#[test]
fn failover_google_mantiene_thought_signature() {
    let mut messages = vec![assistant_tool("call_g", "reason")];
    history_sanitizer::sanitize_history(&mut messages, "google", SanitizeMode::Standard);
    let tc = messages[0].tool_calls.as_ref().unwrap().first().unwrap();
    assert_eq!(tc.thought_signature.as_deref(), Some("gemini-sig"));
    assert!(messages[0].reasoning.is_none());
}
