//! `end_turn`: decisioni DETERMINISTICHE post-end_turn dell'`executor_node`
//! (`brain/agents/nodes/__init__.py:3360-3429`), PUNTO UNICO (regola L) dei rami
//! che riscrivono il `result` visibile a turno concluso.
//!
//! Porta la PARTE PURA (golden-abile) di tre rami ON/seedati in produzione:
//!
//! - **unfulfilled-report** (`:3404-3429`): in modalita' NON autonoma (confirm,
//!   l'`automation_mode` DEFAULT) con intento NON compiuto e turno NON
//!   action-oriented, SOSTITUISCE il `result` con [`build_unfulfilled_report`]
//!   (resoconto onesto deterministico). Gate puro: [`should_substitute_unfulfilled_report`].
//! - **next_actions** (`:3379-3402`): la sola RIMOZIONE deterministica del blocco
//!   `<suggested_actions>` dal testo visibile ([`strip_suggested_actions`], 1:1
//!   con `next_actions.extract_block`). La DERIVAZIONE delle scelte (LLM leggero
//!   purpose `choices_extractor`) e' I/O dietro la porta
//!   [`crate::runtime::ports::NextActionsDeriver`].
//! - **billing fail-fast** (`:2072-2092`): la DECISIONE pura
//!   ([`billing_fail_fast_message`]) di chiudere con `loop_abort` quando la soglia
//!   di esplorazione e' raggiunta E ci sono provider in cooldown billing. La LISTA
//!   dei provider esauriti e' I/O dietro [`crate::runtime::ports::BillingCooldownPort`].
//!
//! Confine I/O (regola L): qui SOLO la parte deterministica. Gli input I/O
//! (modalita' provider esauriti, derivazione LLM) arrivano gia' risolti.

use serde_json::Value;

use crate::state::{AutomationMode, Message};

// ──────────────────────────────────────────────────────────────────────────
//  (1) unfulfilled-report
// ──────────────────────────────────────────────────────────────────────────

/// Gate PURO del ramo unfulfilled-report (`py:3411-3418`): a turno concluso
/// (end_turn senza pending, `result` non vuoto — verificato dal chiamante) il
/// `result` viene sostituito con [`build_unfulfilled_report`] quando TUTTE:
///   - la modalita' NON e' autonoma (`automatic`/`continuous`): in autonoma il
///     re-entry G1 fa AGIRE il modello, qui non si interviene
///     (`_auto_mode not in ("automatic","continuous")`, py:3413). Modalita'
///     assente -> Python default `"confirm"` -> NON autonoma -> gate VERO.
///   - l'intento e' NON compiuto (`unfulfilled`, dal detector gia' risolto a monte);
///   - il turno NON e' action-oriented (`not turn_action_oriented(state)`, py:3418):
///     le richieste d'azione esplicite sono gestite dal G1.
///
/// `automation_mode` e' l'enum dello stato (`None`/assente == Python `"confirm"`).
pub fn should_substitute_unfulfilled_report(
    automation_mode: Option<AutomationMode>,
    unfulfilled: bool,
    action_oriented: bool,
) -> bool {
    if !unfulfilled {
        return false;
    }
    if action_oriented {
        return false;
    }
    // Autonoma = automatic | continuous: in quel caso NON si sostituisce.
    let autonomous = matches!(
        automation_mode,
        Some(AutomationMode::Automatic) | Some(AutomationMode::Continuous)
    );
    !autonomous
}

/// Numero massimo di file mostrati nel resoconto (`files_touched[:12]`, py:1670).
const REPORT_MAX_FILES: usize = 12;
/// Lunghezza della coda del result mostrata (`snippet[-180:]`, py:1675).
const REPORT_SNIPPET_TAIL: usize = 180;

/// Resoconto onesto deterministico quando il turno chiude con un'intenzione non
/// eseguita e NON si fa auto-restart (`build_unfulfilled_report`,
/// `helpers.py:1630-1685`). 1:1 col Python: sintetizza le azioni gia' svolte
/// (tool usati + file toccati dai blocchi `tool_use` della history), dichiara lo
/// stato (interrotto, non completato) e propone il prossimo passo. NESSUNA
/// chiamata LLM (deterministico, nessuna nuova allucinazione).
pub fn build_unfulfilled_report(result_text: Option<&str>, messages: &[Message]) -> String {
    // tool_counts: nome -> conteggio; files_touched: path in ordine di prima apparizione.
    let mut tool_counts: Vec<(String, i64)> = Vec::new();
    let mut files_touched: Vec<String> = Vec::new();
    for m in messages {
        // Solo i blocchi `tool_use` dei messaggi a BLOCCHI contano (py:1644-1656:
        // `content` deve essere una lista di dict con type=="tool_use").
        let blocks = match m {
            Message::Human {
                content: crate::state::MessageContent::Blocks(b),
            } => b,
            Message::Ai {
                content: crate::state::MessageContent::Blocks(b),
                ..
            } => b,
            Message::Tool {
                content: crate::state::MessageContent::Blocks(b),
                ..
            } => b,
            _ => continue,
        };
        for block in blocks {
            if let crate::state::ContentBlock::ToolUse { name, input, .. } = block {
                let name = if name.is_empty() {
                    "tool".to_string()
                } else {
                    name.clone()
                };
                bump_count(&mut tool_counts, &name);
                // path | file_path | filename dal blocco input (py:1654).
                if let Some(obj) = input.as_object() {
                    let path = obj
                        .get("path")
                        .or_else(|| obj.get("file_path"))
                        .or_else(|| obj.get("filename"))
                        .and_then(Value::as_str);
                    if let Some(p) = path {
                        if !p.is_empty() && !files_touched.iter().any(|x| x == p) {
                            files_touched.push(p.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut lines: Vec<String> = vec![
        "Mi sono fermato annunciando un'attesa o un passo successivo senza \
eseguirlo, quindi il compito NON e' completato. Ecco il resoconto onesto:"
            .to_string(),
        String::new(),
    ];
    if !tool_counts.is_empty() {
        // sorted(tool_counts.items(), key=lambda kv: -kv[1]) (py:1664): conteggio
        // DESC, tie-break stabile (sort_by_key e' stabile -> ordine d'inserzione).
        let mut sorted = tool_counts.clone();
        sorted.sort_by_key(|(_, n)| -*n);
        let azioni: Vec<String> = sorted
            .iter()
            .map(|(name, n)| format!("{n}x {name}"))
            .collect();
        lines.push(format!("- Cosa ho fatto: {}.", azioni.join(", ")));
    } else {
        lines.push("- Cosa ho fatto: nessuna azione concreta in questo turno.".to_string());
    }
    if !files_touched.is_empty() {
        let shown = files_touched
            .iter()
            .take(REPORT_MAX_FILES)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = if files_touched.len() <= REPORT_MAX_FILES {
            String::new()
        } else {
            format!(" (+{} altri)", files_touched.len() - REPORT_MAX_FILES)
        };
        lines.push(format!("- File toccati: {shown}{more}."));
    }
    // snippet = (result_text or "").strip().replace("\n"," ") -> coda 180 codepoint.
    let snippet = result_text.unwrap_or("").trim().replace('\n', " ");
    if !snippet.is_empty() {
        let tail = tail_chars(&snippet, REPORT_SNIPPET_TAIL);
        lines.push(format!("- Dove mi sono interrotto: \"{tail}\""));
    }
    lines.push(
        "- Cosa manca: portare a termine il compito; l'ultimo passo annunciato \
non e' stato eseguito."
            .to_string(),
    );
    lines.push(
        "- Prossimo passo proposto: invece di attendere passivamente, diagnosticare \
lo stato reale (es. leggere i log del servizio/container che non parte) e \
agire sulla causa. Confermi se procedo?"
            .to_string(),
    );
    lines.join("\n")
}

/// Incrementa il conteggio di `name` in una lista (preserva l'ordine di prima
/// apparizione, come l'inserimento progressivo in un dict Python 3.7+).
fn bump_count(counts: &mut Vec<(String, i64)>, name: &str) {
    if let Some(entry) = counts.iter_mut().find(|(n, _)| n == name) {
        entry.1 += 1;
    } else {
        counts.push((name.to_string(), 1));
    }
}

/// Ultimi `n` CODEPOINT di `s` (parita' con lo slicing Python `s[-n:]`, che opera
/// su code point, non byte). Riusa la stessa semantica di `tail_chars` dei signals.
fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}

// ──────────────────────────────────────────────────────────────────────────
//  (2) next_actions: rimozione deterministica del blocco <suggested_actions>
// ──────────────────────────────────────────────────────────────────────────

/// Marker di apertura del blocco machine-readable (`<suggested_actions>`).
const SUGGESTED_OPEN: &str = "<suggested_actions>";
/// Marker di chiusura del blocco.
const SUGGESTED_CLOSE: &str = "</suggested_actions>";

/// Rimuove OGNI occorrenza del blocco `<suggested_actions>...</suggested_actions>`
/// dal testo visibile (l'utente non deve mai vedere il blocco grezzo), poi fa il
/// `rstrip` del risultato. 1:1 con la parte deterministica di
/// `next_actions.extract_block` (`next_actions.py:262-295`): la regex Python
/// `<suggested_actions>\s*(.*?)\s*</suggested_actions>` con flag IGNORECASE|DOTALL.
///
/// Tollerante: case-insensitive sui tag, `.` attraversa i newline (DOTALL),
/// match non-greedy (la PRIMA chiusura dopo ogni apertura). Un blocco senza
/// chiusura NON viene rimosso (parita' col match della regex). Se il testo non
/// contiene il marker (case-insensitive) ritorna il testo invariato (early-out
/// py:273).
pub fn strip_suggested_actions(text: &str) -> String {
    let lower = text.to_lowercase();
    if !lower.contains(SUGGESTED_OPEN) {
        return text.to_string();
    }
    // Lavoriamo sui byte-offset trovati nel lower-case (i tag sono ASCII: gli
    // offset coincidono col testo originale anche con contenuto multibyte interno).
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize; // offset nel testo ORIGINALE gia' emesso
    let mut search_from = 0usize; // offset nel lower da cui cercare
    while let Some(rel_open) = lower[search_from..].find(SUGGESTED_OPEN) {
        let open = search_from + rel_open;
        // Cerca la chiusura DOPO l'apertura (non-greedy: la prima).
        let after_open = open + SUGGESTED_OPEN.len();
        let Some(rel_close) = lower[after_open..].find(SUGGESTED_CLOSE) else {
            // Nessuna chiusura: il blocco non e' un match valido -> non rimuovere.
            break;
        };
        let close_end = after_open + rel_close + SUGGESTED_CLOSE.len();
        // Emetti il testo PRIMA dell'apertura INVARIATO (parita' 1:1 con `re.sub`:
        // la regex rimuove SOLO il match `<...>\s*...\s*</...>`; gli `\s*` interni
        // catturano lo whitespace ADIACENTE ai tag DENTRO il blocco, ma lo
        // whitespace ESTERNO al blocco NON e' toccato, es. "a <blocco> b" -> "a  b").
        out.push_str(&text[cursor..open]);
        cursor = close_end;
        search_from = close_end;
    }
    out.push_str(&text[cursor..]);
    // rstrip finale (py:294: cleaned = _BLOCK_RE.sub("", text).rstrip()).
    out.trim_end().to_string()
}

// ──────────────────────────────────────────────────────────────────────────
//  (3) billing fail-fast
// ──────────────────────────────────────────────────────────────────────────

/// Decisione PURA del fail-fast billing (`py:2072-2079`): se l'esplorazione ha
/// raggiunto la soglia (`exploration_count >= exploration_threshold`) E ci sono
/// provider in cooldown billing (`exhausted` non vuota), ritorna `Some(messaggio)`
/// onesto (il run si chiude con `loop_abort`); altrimenti `None` (prosegue).
///
/// La LISTA `exhausted` arriva gia' risolta dalla porta I/O
/// [`crate::runtime::ports::BillingCooldownPort`] (gia' ordinata, come
/// `sorted(snap.keys())` py-side). Il messaggio e' 1:1 col Python.
pub fn billing_fail_fast_message(
    exploration_count: i64,
    exploration_threshold: i64,
    exhausted: &[String],
    current_provider: &str,
) -> Option<String> {
    if exploration_count < exploration_threshold {
        return None;
    }
    if exhausted.is_empty() {
        return None;
    }
    // Fail-fast SOLO se il provider IN USO e' esso stesso esausto. Se il run usa
    // un provider VALIDO (es. deepseek mentre anthropic/openai sono in cooldown),
    // l'esplorazione lunga NON e' un problema di billing ma di loop/modello ->
    // deve seguire l'anti-loop (guide/escalate), NON chiudere con "ricarica
    // crediti" (fuorviante). Senza questo check il messaggio incolpava i crediti
    // anche con un provider perfettamente funzionante.
    if current_provider.is_empty()
        || !exhausted
            .iter()
            .any(|p| p.eq_ignore_ascii_case(current_provider))
    {
        return None;
    }
    Some(format!(
        "L'elaborazione si e' interrotta: i provider AI principali sono in \
cooldown per quota/credito esaurito ({}). Ricarica i crediti (o attendi il \
reset) e riprova.",
        exhausted.join(", ")
    ))
}

// ──────────────────────────────────────────────────────────────────────────
//  (4) smart upscale: decisione pura
// ──────────────────────────────────────────────────────────────────────────

/// Decisione PURA dello smart-upscale (`_smart_upscale_model` gate, `helpers.py:2755-2760`):
/// `true` se il contesto stimato e' >= 90% del context window del modello attivo
/// (`est_tokens >= current_window * 0.9`, py:2757). Solo allora vale la pena
/// promuovere a un modello con window piu' grande PRIMA della chiamata LLM.
///
/// Pre-condizioni Python (`enabled`, `est_tokens > 0`, `current_window > 0`,
/// py:2755): `enabled` e' DB-driven (la config arriva al chiamante), `est_tokens`/
/// `current_window <= 0` -> `false` (qui replicate). La SELEZIONE del modello
/// target (query catalog tier-based) e' I/O dietro
/// [`crate::runtime::ports::ModelUpscalePort`]: questa funzione decide SOLO SE
/// tentare l'upscale + il numero `required` di token (`est_tokens * overhead`).
pub fn should_upscale(enabled: bool, est_tokens: i64, current_window: i64) -> bool {
    if !enabled {
        return false;
    }
    if est_tokens <= 0 || current_window <= 0 {
        return false;
    }
    // est_tokens >= current_window * 0.9 (py:2757 in forma negata `< 0.9 -> None`).
    (est_tokens as f64) >= (current_window as f64) * 0.9
}

/// Token "richiesti" per l'upscale (`required = int(est_tokens * overhead)`,
/// py:2760): il modello target deve avere `context_window >= required`. PURA;
/// la query catalog e' I/O dietro [`crate::runtime::ports::ModelUpscalePort`].
pub fn upscale_required_tokens(est_tokens: i64, overhead_ratio: f64) -> i64 {
    ((est_tokens as f64) * overhead_ratio) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ContentBlock, MessageContent};
    use serde_json::json;

    fn ai_tool(name: &str, input: Value) -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: name.into(),
                input,
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        }
    }

    #[test]
    fn gate_unfulfilled_confirm_non_action() {
        // Confirm (assente == confirm), unfulfilled, non action -> sostituisce.
        assert!(should_substitute_unfulfilled_report(None, true, false));
        assert!(should_substitute_unfulfilled_report(
            Some(AutomationMode::Confirm),
            true,
            false
        ));
        // Autonoma -> NON sostituisce (G1 fa agire).
        assert!(!should_substitute_unfulfilled_report(
            Some(AutomationMode::Automatic),
            true,
            false
        ));
        assert!(!should_substitute_unfulfilled_report(
            Some(AutomationMode::Continuous),
            true,
            false
        ));
        // Action-oriented -> NON sostituisce (gestito dal G1).
        assert!(!should_substitute_unfulfilled_report(None, true, true));
        // Compiuto -> NON sostituisce.
        assert!(!should_substitute_unfulfilled_report(None, false, false));
    }

    #[test]
    fn report_conta_tool_e_file() {
        let messages = vec![
            ai_tool("write_file", json!({"path": "a.rs"})),
            ai_tool("write_file", json!({"path": "b.rs"})),
            ai_tool("read_file", json!({"path": "a.rs"})),
        ];
        let report = build_unfulfilled_report(Some("Ora attendo il build."), &messages);
        assert!(report.contains("2x write_file"));
        assert!(report.contains("1x read_file"));
        assert!(report.contains("a.rs, b.rs"));
        assert!(report.contains("Ora attendo il build."));
        assert!(report.contains("NON e' completato"));
    }

    #[test]
    fn report_nessuna_azione() {
        let report = build_unfulfilled_report(Some("Procedo."), &[]);
        assert!(report.contains("nessuna azione concreta"));
        assert!(!report.contains("File toccati"));
    }

    #[test]
    fn strip_blocco_suggested() {
        let text =
            "Ecco la risposta.\n<suggested_actions>\n[{\"label\":\"x\"}]\n</suggested_actions>";
        let cleaned = strip_suggested_actions(text);
        assert_eq!(cleaned, "Ecco la risposta.");
        assert!(!cleaned.contains("suggested_actions"));
    }

    #[test]
    fn strip_blocco_case_insensitive_e_multiplo() {
        let text = "A <SUGGESTED_ACTIONS>uno</SUGGESTED_ACTIONS> B <suggested_actions>due</suggested_actions> C";
        let cleaned = strip_suggested_actions(text);
        assert!(!cleaned.to_lowercase().contains("suggested_actions"));
        assert!(cleaned.contains('A') && cleaned.contains('B') && cleaned.contains('C'));
    }

    #[test]
    fn strip_no_blocco_invariato() {
        let text = "Nessun blocco qui.";
        assert_eq!(strip_suggested_actions(text), "Nessun blocco qui.");
    }

    #[test]
    fn strip_blocco_senza_chiusura_non_rimosso() {
        let text = "Testo <suggested_actions> senza chiusura";
        // Niente chiusura -> nessun match -> invariato (eccetto rstrip finale).
        assert_eq!(strip_suggested_actions(text), text);
    }

    #[test]
    fn billing_fail_fast_scatta() {
        let exhausted = vec!["anthropic".to_string(), "openai".to_string()];
        let msg = billing_fail_fast_message(6, 6, &exhausted, "anthropic");
        assert!(msg.is_some());
        let m = msg.unwrap();
        assert!(m.contains("anthropic, openai"));
        assert!(m.contains("cooldown"));
    }

    #[test]
    fn billing_fail_fast_sotto_soglia() {
        let exhausted = vec!["anthropic".to_string()];
        assert!(billing_fail_fast_message(5, 6, &exhausted, "anthropic").is_none());
    }

    #[test]
    fn billing_fail_fast_nessun_esaurito() {
        assert!(billing_fail_fast_message(6, 6, &[], "deepseek").is_none());
    }

    #[test]
    fn billing_fail_fast_provider_corrente_valido() {
        // FIX: provider IN USO valido (deepseek) mentre anthropic/openai sono
        // esausti -> NON e' billing -> None (prosegue all'anti-loop), niente
        // messaggio "ricarica crediti" fuorviante.
        let exhausted = vec!["anthropic".to_string(), "openai".to_string()];
        assert!(billing_fail_fast_message(7, 6, &exhausted, "deepseek").is_none());
        // Provider corrente esausto -> Some (caso legittimo).
        assert!(billing_fail_fast_message(7, 6, &exhausted, "anthropic").is_some());
    }

    #[test]
    fn upscale_gate() {
        // 90% di 100_000 = 90_000.
        assert!(should_upscale(true, 90_000, 100_000));
        assert!(should_upscale(true, 95_000, 100_000));
        assert!(!should_upscale(true, 89_999, 100_000));
        // Disabilitato / window ignoto / est<=0 -> false.
        assert!(!should_upscale(false, 95_000, 100_000));
        assert!(!should_upscale(true, 95_000, 0));
        assert!(!should_upscale(true, 0, 100_000));
    }

    #[test]
    fn upscale_required() {
        assert_eq!(upscale_required_tokens(100_000, 1.2), 120_000);
    }
}
