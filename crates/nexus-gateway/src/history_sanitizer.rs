//! Sanitizer cross-provider della conversation history (punto unico, regola L).
//!
//! Normalizza [`crate::types::LlmMessage`] per il dialetto del provider TARGET
//! prima di ogni chiamata LLM e, in caso di `client_error` legato al formato
//! history, abilita un retry con modalita' [`SanitizeMode::Aggressive`].
//!
//! Copre i quirk emersi in produzione (run f0ad0337):
//!   - DeepSeek: `reasoning` obbligatorio solo su DeepSeek; fuori contesto -> 400;
//!   - Anthropic: `thinking_signature` solo su Anthropic;
//!   - Google: `thought_signature` per-call solo su Google;
//!   - Mistral: ultimo messaggio deve essere user/tool (no trailing assistant);
//!   - Cross-provider failover: pairing tool_use/tool_result incoerente dopo
//!     rolling summary o cambio provider.

use std::collections::{HashMap, HashSet};

use crate::types::{LlmMessage, MessageContent};

/// Modalita' di sanificazione: `Standard` applica le regole per-dialetto;
/// `Aggressive` (retry post client_error history) rimuove anche campi
/// provider-specifici residui e ripara pairing tool piu' invasivamente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SanitizeMode {
    Standard,
    Aggressive,
}

/// Statistiche della sanificazione (telemetria/debug, niente contenuti sensibili).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SanitizeReport {
    pub stripped_reasoning: usize,
    pub stripped_thinking_signature: usize,
    pub stripped_thought_signature: usize,
    pub stripped_trailing_assistant: usize,
    pub removed_orphan_tool_results: usize,
    pub injected_synthetic_tool_results: usize,
}

/// Placeholder per tool-result sintetico quando manca la risposta (history troncata).
const SYNTHETIC_TOOL_RESULT: &str =
    "[tool result unavailable: history truncated or provider switch]";

/// True se il codice strutturato indica un errore client causato dalla history /
/// ordine messaggi / argomento invalido (retry con sanificazione aggressiva).
pub fn is_history_related_client_error(code: Option<&str>) -> bool {
    let Some(c) = code.map(str::to_ascii_lowercase) else {
        return false;
    };
    c.starts_with("invalid_request")
        || c == "invalid_argument"
        || c == "invalid_request_error"
        || c == "invalid_request_message_order"
        || c == "malformed_function_call"
}

/// True se l'errore indica un model_id inesistente/deprecato (auto-disable immediato).
pub fn is_invalid_model_error(code: Option<&str>, status: u16) -> bool {
    if status == 404 {
        return true;
    }
    matches!(
        code.map(str::to_ascii_lowercase).as_deref(),
        Some(c) if c == "invalid_model"
            || c == "model_not_found"
            || c == "not_found"
            || c.contains("invalid_model")
    )
}

/// Sanifica `messages` in-place per il provider `target_provider` (nome canonico
/// lowercase, es. `"mistral"`, `"deepseek"`, `"google"`).
pub fn sanitize_history(
    messages: &mut Vec<LlmMessage>,
    target_provider: &str,
    mode: SanitizeMode,
) -> SanitizeReport {
    let provider = normalize_provider(target_provider);
    let mut report = SanitizeReport::default();

    strip_provider_specific_fields(messages, &provider, mode, &mut report);
    reconcile_tool_pairing(messages, mode, &mut report);
    if provider_requires_user_or_tool_last(&provider) {
        strip_trailing_assistant(messages, &mut report);
    }

    report
}

fn normalize_provider(p: &str) -> String {
    p.split('/').next().unwrap_or(p).trim().to_ascii_lowercase()
}

fn provider_keeps_reasoning(provider: &str) -> bool {
    provider == "deepseek"
}

fn provider_keeps_thinking_signature(provider: &str) -> bool {
    provider == "anthropic"
}

fn provider_keeps_thought_signature(provider: &str) -> bool {
    provider == "google"
}

/// True per provider OpenAI-compat stretti (Mistral) che rifiutano assistant trailing.
pub fn provider_requires_user_or_tool_last(provider: &str) -> bool {
    normalize_provider(provider) == "mistral"
}

fn strip_provider_specific_fields(
    messages: &mut [LlmMessage],
    provider: &str,
    mode: SanitizeMode,
    report: &mut SanitizeReport,
) {
    let keep_reasoning = provider_keeps_reasoning(provider) && mode != SanitizeMode::Aggressive;
    let keep_thinking = provider_keeps_thinking_signature(provider) && mode != SanitizeMode::Aggressive;
    let keep_thought = provider_keeps_thought_signature(provider) && mode != SanitizeMode::Aggressive;

    for msg in messages.iter_mut() {
        if !keep_reasoning && msg.reasoning.take().is_some() {
            report.stripped_reasoning += 1;
        }
        if !keep_thinking && msg.thinking_signature.take().is_some() {
            report.stripped_thinking_signature += 1;
        }
        if let Some(calls) = msg.tool_calls.as_mut() {
            if !keep_thought {
                for tc in calls.iter_mut() {
                    if tc.thought_signature.take().is_some() {
                        report.stripped_thought_signature += 1;
                    }
                }
            }
        }
    }
}

/// Riconcilia tool_use <-> tool_result: rimuove result orfani, inietta sintetici
/// per call senza risposta (parita' concettuale con `reconcile_function_call_response_pairs`
/// Google, ma sul contratto canonico LlmMessage).
fn reconcile_tool_pairing(
    messages: &mut Vec<LlmMessage>,
    mode: SanitizeMode,
    report: &mut SanitizeReport,
) {
    let call_ids = collect_tool_call_ids(messages);
    if call_ids.is_empty() {
        // Nessuna tool-call: elimina messaggi tool orfani.
        let before = messages.len();
        messages.retain(|m| m.role != "tool");
        report.removed_orphan_tool_results += before.saturating_sub(messages.len());
        return;
    }

    // Rimuovi tool-result il cui id non corrisponde a nessuna call.
    messages.retain(|m| {
        if m.role != "tool" {
            return true;
        }
        let Some(id) = m.tool_call_id.as_deref() else {
            report.removed_orphan_tool_results += 1;
            return false;
        };
        if call_ids.contains(id) {
            true
        } else {
            report.removed_orphan_tool_results += 1;
            false
        }
    });

    let answered = collect_tool_result_ids(messages);
    let missing: Vec<(String, String)> = messages
        .iter()
        .filter(|m| m.role == "assistant")
        .flat_map(|m| {
            m.tool_calls
                .as_ref()
                .into_iter()
                .flat_map(|calls| calls.iter())
        })
        .filter(|tc| !answered.contains(&tc.id))
        .map(|tc| (tc.id.clone(), tc.function.name.clone()))
        .collect();

    if missing.is_empty() {
        return;
    }

    // In Standard mode: inietta sintetici solo se <= 2 mancanti (history parziale).
    // In Aggressive: inietta sempre (post client_error).
    let inject = match mode {
        SanitizeMode::Aggressive => true,
        SanitizeMode::Standard => missing.len() <= 2,
    };
    if !inject {
        return;
    }

    for (id, name) in missing {
        messages.push(synthetic_tool_message(&id, &name));
        report.injected_synthetic_tool_results += 1;
    }
}

fn collect_tool_call_ids(messages: &[LlmMessage]) -> HashSet<String> {
    messages
        .iter()
        .filter(|m| m.role == "assistant")
        .flat_map(|m| m.tool_calls.as_ref())
        .flat_map(|calls| calls.iter())
        .map(|tc| tc.id.clone())
        .collect()
}

fn collect_tool_result_ids(messages: &[LlmMessage]) -> HashSet<String> {
    messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.clone())
        .collect()
}

fn synthetic_tool_message(tool_call_id: &str, name: &str) -> LlmMessage {
    LlmMessage {
        role: "tool".to_string(),
        content: MessageContent::Text(SYNTHETIC_TOOL_RESULT.to_string()),
        tool_call_id: Some(tool_call_id.to_string()),
        tool_calls: None,
        name: Some(name.to_string()),
        thinking_signature: None,
        reasoning: None,
    }
}

/// Rimuove assistant finali senza tool_calls pendenti (Mistral 422/400).
fn strip_trailing_assistant(messages: &mut Vec<LlmMessage>, report: &mut SanitizeReport) {
    while messages.len() > 1 {
        let drop_last = matches!(
            messages.last(),
            Some(m) if m.role == "assistant" && m.tool_calls.is_none()
        );
        if drop_last {
            messages.pop();
            report.stripped_trailing_assistant += 1;
        } else {
            break;
        }
    }
}

/// Costruisce una mappa id->nome tool da TUTTA la history (per round-trip Google).
pub fn tool_call_id_to_name(messages: &[LlmMessage]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for msg in messages {
        if let Some(calls) = msg.tool_calls.as_ref() {
            for tc in calls {
                map.insert(tc.id.clone(), tc.function.name.clone());
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmToolCall, ToolFunctionCall};

    fn assistant_with_tools(id: &str, name: &str) -> LlmMessage {
        LlmMessage {
            role: "assistant".to_string(),
            content: MessageContent::Text(String::new()),
            tool_calls: Some(vec![LlmToolCall {
                id: id.to_string(),
                kind: "function".to_string(),
                function: ToolFunctionCall {
                    name: name.to_string(),
                    arguments: "{}".to_string(),
                },
                thought_signature: Some("sig-g".to_string()),
            }]),
            tool_call_id: None,
            name: None,
            thinking_signature: Some("think-a".to_string()),
            reasoning: Some("reason-d".to_string()),
        }
    }

    fn tool_result(id: &str) -> LlmMessage {
        LlmMessage {
            role: "tool".to_string(),
            content: MessageContent::Text("ok".to_string()),
            tool_call_id: Some(id.to_string()),
            tool_calls: None,
            name: Some("get_time".to_string()),
            thinking_signature: None,
            reasoning: None,
        }
    }

    #[test]
    fn strip_reasoning_fuori_deepseek() {
        let mut msgs = vec![assistant_with_tools("c1", "get_time")];
        let r = sanitize_history(&mut msgs, "mistral", SanitizeMode::Standard);
        assert_eq!(r.stripped_reasoning, 1);
        assert_eq!(r.stripped_thinking_signature, 1);
        assert_eq!(r.stripped_thought_signature, 1);
        assert!(msgs[0].reasoning.is_none());
    }

    #[test]
    fn deepseek_mantiene_reasoning_in_standard() {
        let mut msgs = vec![assistant_with_tools("c1", "get_time")];
        let r = sanitize_history(&mut msgs, "deepseek", SanitizeMode::Standard);
        assert_eq!(r.stripped_reasoning, 0);
        assert!(msgs[0].reasoning.is_some());
    }

    #[test]
    fn aggressive_strip_tutti_i_campi_provider_specifici() {
        let mut msgs = vec![assistant_with_tools("c1", "get_time")];
        let r = sanitize_history(&mut msgs, "deepseek", SanitizeMode::Aggressive);
        assert_eq!(r.stripped_reasoning, 1);
        assert!(msgs[0].reasoning.is_none());
    }

    #[test]
    fn mistral_strip_trailing_assistant() {
        let mut msgs = vec![
            LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("ciao".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
            },
            LlmMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("risposta".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
            },
        ];
        let r = sanitize_history(&mut msgs, "mistral", SanitizeMode::Standard);
        assert_eq!(r.stripped_trailing_assistant, 1);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn rimuove_tool_result_orfano() {
        let mut msgs = vec![
            LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("x".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
            },
            tool_result("orphan"),
        ];
        let r = sanitize_history(&mut msgs, "google", SanitizeMode::Standard);
        assert_eq!(r.removed_orphan_tool_results, 1);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn inietta_tool_result_sintetico_per_call_mancante() {
        let mut msgs = vec![
            LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("usa tool".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
            },
            assistant_with_tools("call_x", "read_file"),
        ];
        let r = sanitize_history(&mut msgs, "anthropic", SanitizeMode::Aggressive);
        assert_eq!(r.injected_synthetic_tool_results, 1);
        assert!(msgs.iter().any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call_x")));
    }

    #[test]
    fn history_client_error_codes() {
        assert!(is_history_related_client_error(Some("invalid_request_error")));
        assert!(is_history_related_client_error(Some("invalid_request_message_order")));
        assert!(is_history_related_client_error(Some("invalid_argument")));
        assert!(!is_history_related_client_error(Some("invalid_model")));
        assert!(!is_history_related_client_error(None));
    }

    #[test]
    fn invalid_model_detection() {
        assert!(is_invalid_model_error(Some("invalid_model"), 400));
        assert!(is_invalid_model_error(None, 404));
        assert!(!is_invalid_model_error(Some("invalid_request_error"), 400));
    }
}
