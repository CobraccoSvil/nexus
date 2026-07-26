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
    /// tool result RIPOSIZIONATI: erano nella history ma non immediatamente
    /// dopo la loro tool_call (es. un messaggio user iniettato tra assistant e
    /// tool dal ReviewGate/correzione). Il formato OpenAI-compat (Mistral 400
    /// "Unexpected role 'tool' after role 'user'") esige l'adiacenza.
    pub reordered_tool_results: usize,
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
    // Il `reasoning` su DeepSeek NON e' un campo opzionale da ripulire: l'API lo
    // ESIGE sugli assistant prodotti in thinking mode, e senza risponde HTTP 400
    // ("The reasoning_content in the thinking mode must be passed back"). Toglierlo
    // nel retry Aggressive produce con CERTEZZA il fallimento che quel retry doveva
    // riparare: il vincolo del provider vince sulla modalita'. Aggressive continua a
    // fare tutto il resto (riconciliazione tool piu' invasiva, trailing assistant).
    //
    // Asimmetria voluta con le due firme sotto: Anthropic e Google TOLLERANO
    // l'assenza della firma (la richiedono solo nei turni con tool), quindi li'
    // rimuoverla e' una semplificazione legittima dell'ultima spiaggia.
    let keep_reasoning = provider_keeps_reasoning(provider);
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

/// Riconcilia tool_use <-> tool_result RICOSTRUENDO la sequenza: ogni tool
/// result viene posizionato SUBITO DOPO l'assistant che contiene la sua call
/// (nell'ordine delle call), i result orfani sono rimossi e le call senza
/// risposta ricevono un sintetico INLINE (non in fondo). Cosi' l'invariante del
/// formato OpenAI/Anthropic/Google `assistant(tool_calls) -> tool(results)` vale
/// SEMPRE, anche se un messaggio user era stato iniettato in mezzo (ReviewGate,
/// correzione post-final_gate): era la causa del Mistral 400
/// "Unexpected role 'tool' after role 'user'".
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

    // 1. Conta le posizioni ILLEGALI nella history originale (telemetria): sono
    //    esattamente i tool result che il rebuild sposta.
    report.reordered_tool_results += misplaced_tool_count(messages, &call_ids);

    // 2. Estrai i tool result: id valido -> mappa (primo vince), orfani scartati.
    //    `kept` conserva l'ordine di tutti i NON-tool.
    let mut tool_by_id: HashMap<String, LlmMessage> = HashMap::new();
    let mut kept: Vec<LlmMessage> = Vec::with_capacity(messages.len());
    for m in messages.drain(..) {
        if m.role == "tool" {
            match m.tool_call_id.as_deref() {
                Some(id) if call_ids.contains(id) => {
                    tool_by_id.entry(id.to_string()).or_insert(m);
                }
                _ => report.removed_orphan_tool_results += 1,
            }
        } else {
            kept.push(m);
        }
    }

    // 3. Soglia injection: la Standard guarda il totale mancante (storico).
    let missing_count = kept
        .iter()
        .filter(|m| m.role == "assistant")
        .flat_map(|m| m.tool_calls.as_ref().into_iter().flatten())
        .filter(|tc| !tool_by_id.contains_key(&tc.id))
        .count();
    let inject = match mode {
        SanitizeMode::Aggressive => true,
        SanitizeMode::Standard => missing_count <= 2,
    };

    // 4. Ricostruisci: dopo ogni assistant con tool_calls, i result nell'ordine
    //    delle call; sintetico INLINE per le mancanti (se consentito).
    let mut out: Vec<LlmMessage> = Vec::with_capacity(kept.len() + tool_by_id.len());
    for m in kept {
        let calls = if m.role == "assistant" {
            m.tool_calls.clone()
        } else {
            None
        };
        out.push(m);
        let Some(calls) = calls else { continue };
        for tc in &calls {
            if let Some(t) = tool_by_id.remove(&tc.id) {
                out.push(t);
            } else if inject {
                out.push(synthetic_tool_message(&tc.id, &tc.function.name));
                report.injected_synthetic_tool_results += 1;
            }
        }
    }

    *messages = out;
}

/// True se `prev` puo' legittimamente precedere un messaggio `tool` nel formato
/// OpenAI-compat: un altro tool result o l'assistant che ha emesso le call.
fn valid_tool_predecessor(prev: &LlmMessage) -> bool {
    prev.role == "tool" || (prev.role == "assistant" && prev.tool_calls.is_some())
}

/// Numero di tool result (con id valido) in posizione ILLEGALE: il predecessore
/// non e' un predecessore valido per un tool. Sono quelli che il rebuild sposta.
fn misplaced_tool_count(messages: &[LlmMessage], call_ids: &HashSet<String>) -> usize {
    messages
        .iter()
        .enumerate()
        .filter(|(i, m)| {
            m.role == "tool"
                && m.tool_call_id.as_deref().is_some_and(|id| call_ids.contains(id))
                && !i
                    .checked_sub(1)
                    .map(|p| valid_tool_predecessor(&messages[p]))
                    .unwrap_or(false)
        })
        .count()
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
    fn deepseek_mantiene_reasoning_anche_in_aggressive() {
        // Aggressive e' il retry che scatta DOPO un client_error di formato. Se
        // togliesse il reasoning, DeepSeek risponderebbe 400 "must be passed
        // back": il tentativo di riparare garantirebbe un secondo fallimento,
        // diverso dal primo. Il campo resta; il resto della sanificazione no.
        let mut msgs = vec![assistant_with_tools("c1", "get_time")];
        let r = sanitize_history(&mut msgs, "deepseek", SanitizeMode::Aggressive);
        assert_eq!(
            r.stripped_reasoning, 0,
            "il reasoning e' obbligatorio su deepseek: Aggressive non deve toglierlo"
        );
        assert!(msgs[0].reasoning.is_some());
        // La firma Anthropic invece cade: li' l'assenza e' tollerata dal provider.
        assert_eq!(r.stripped_thinking_signature, 1);
    }

    #[test]
    fn fuori_deepseek_aggressive_toglie_comunque_il_reasoning() {
        // Il campo resta provider-specifico: su un provider che non lo conosce
        // va tolto in entrambe le modalita' (era gia' vero, non deve regredire).
        let mut msgs = vec![assistant_with_tools("c1", "get_time")];
        let r = sanitize_history(&mut msgs, "mistral", SanitizeMode::Aggressive);
        assert_eq!(r.stripped_reasoning, 1);
        assert!(msgs.iter().all(|m| m.reasoning.is_none()));
    }

    #[test]
    fn deepseek_mantiene_reasoning_in_standard() {
        let mut msgs = vec![assistant_with_tools("c1", "get_time")];
        let r = sanitize_history(&mut msgs, "deepseek", SanitizeMode::Standard);
        assert_eq!(r.stripped_reasoning, 0);
        assert!(msgs[0].reasoning.is_some());
    }

    // NOTA: qui viveva `aggressive_strip_tutti_i_campi_provider_specifici`, che
    // asseriva `stripped_reasoning == 1` su deepseek in Aggressive. Codificava la
    // premessa "l'ultima spiaggia toglie TUTTI i campi provider-specifici", che
    // per il reasoning di DeepSeek e' controproducente: quel campo e' obbligatorio,
    // e toglierlo trasforma un 400 di formato in un 400 "must be passed back".
    // Il caso e' ora coperto da `deepseek_mantiene_reasoning_anche_in_aggressive`
    // (reasoning conservato, firma Anthropic comunque rimossa) e da
    // `fuori_deepseek_aggressive_toglie_comunque_il_reasoning`.
    //
    // TENSIONE NOTA, non risolta da questo cambio: il doc del modulo segnala anche
    // il caso opposto (reasoning "fuori contesto" -> 400). Conservare il campo sugli
    // assistant SUPERSTITI e' coerente per costruzione (quelli rimossi si portano via
    // il proprio reasoning), ma se emergesse un 400 da reasoning fuori posto la
    // risposta non sara' rimetterlo via in blocco: sara' allineare il campo al
    // troncamento. Il log a `info` del report serve a distinguere i due casi.

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

    fn user(text: &str) -> LlmMessage {
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(text.to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }
    }

    #[test]
    fn tool_result_dopo_user_viene_riposizionato_dopo_la_call() {
        // Scenario incidente run 1db02ed3 (Mistral 400 "Unexpected role 'tool'
        // after role 'user'"): un messaggio user (nota del ReviewGate/correzione)
        // era finito TRA l'assistant con la call e il suo tool result.
        let mut msgs = vec![
            user("fai qualcosa"),
            assistant_with_tools("c1", "read_file"),
            user("NOTA: correggi anche X"),
            tool_result("c1"),
        ];
        let r = sanitize_history(&mut msgs, "mistral", SanitizeMode::Standard);
        assert_eq!(r.reordered_tool_results, 1, "il tool era in posizione illegale");
        // Invariante OpenAI ristabilita: ogni tool segue l'assistant o un tool.
        for i in 0..msgs.len() {
            if msgs[i].role == "tool" {
                assert!(
                    i > 0 && valid_tool_predecessor(&msgs[i - 1]),
                    "tool a {i} preceduto da {}",
                    msgs[i - 1].role
                );
            }
        }
        // Il tool result e' subito dopo il suo assistant; la nota user resta in coda.
        let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "user"]);
    }

    #[test]
    fn tool_result_gia_adiacente_non_conta_come_riposizionato() {
        let mut msgs = vec![
            user("vai"),
            assistant_with_tools("c1", "read_file"),
            tool_result("c1"),
        ];
        let r = sanitize_history(&mut msgs, "mistral", SanitizeMode::Standard);
        assert_eq!(r.reordered_tool_results, 0, "gia' in ordine: nessuno spostamento");
        assert_eq!(r.removed_orphan_tool_results, 0);
        let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool"]);
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
