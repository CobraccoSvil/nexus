//! `predictive_cap`: guardia PURA del predictive context cap pre-tool (FIX D, ADR 0014).
//! Porting 1:1 del CALCOLO di `_predictive_cap_check` (`brain/agents/nodes/helpers.py:3539`).
//!
//! L'IO (lookup `context_window` dal catalogo, stima del contesto corrente, stima
//! dimensione tool_result) resta FUORI: arriva al chiamante gia' risolto e viene
//! passato come parametri numerici. Qui vive solo la decisione di blocco e la
//! costruzione del messaggio user-facing col SENTINEL.
//!
//! PUNTO UNICO (regola L) della sentinella [`PREDICTIVE_CAP_SENTINEL`]: il guard
//! "blocked-da-cap" del tool_dispatch la matcha TESTUALMENTE (`if SENTINEL in content`)
//! per rifiutare una dichiarazione `task_complete outcome=blocked` causata da un blocco
//! di cap su singola chiamata. Una sola costante condivisa: se la stringa diverge fra il
//! produttore (questa funzione) e il consumatore (il guard), la protezione si rompe in
//! silenzio. Per questo e' esposta qui come unica fonte e va usata da entrambi i lati.

use std::collections::HashSet;
use std::sync::LazyLock;

/// Sentinella a convenzione chiusa: prefisso del tool_result quando il predictive
/// context cap blocca una chiamata. VALORE ESATTO replicato 1:1 dal Python
/// (`PREDICTIVE_CAP_SENTINEL`, helpers.py:3518). NON modificare senza aggiornare il
/// guard testuale che la consuma (regola L).
pub const PREDICTIVE_CAP_SENTINEL: &str = "[ERROR: chiamata bloccata da predictive context cap]";

/// Tool di controllo/output-piccolo ESENTI dal cap (`_CAP_EXEMPT_TOOLS` Python, 1:1;
/// `review_verdict` e' un'aggiunta nativa Rust — Fase B ultracode, nessuna
/// controparte Python). Il loro risultato e' nullo o minuscolo: non puo' saturare
/// il contesto. `review_verdict` in particolare e' il canale di CHIUSURA del
/// revisore, chiamato per contratto come ultima azione (= al picco del contesto):
/// senza esenzione il cap lo bloccherebbe proprio quando serve e il tentativo
/// bloccato azzererebbe anche un verdetto precedente (invalidazione ADR 0034).
/// Stesso ragionamento per `advisory_verdict` (canale di CHIUSURA del parere
/// delle figure del consiglio di analisi a monte) e per `debate_position`
/// (canale di CHIUSURA dell'avvocato del dibattito).
static CAP_EXEMPT_TOOLS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "task_complete",
        "review_verdict",
        "advisory_verdict",
        "debate_position",
        "nexus_mcp_tool_call",
        "nexus_mcp_tool_search",
        "nexus_get_worklog",
        "nexus_db_tables",
        "nexus_db_describe",
    ])
});

/// Prefisso dei tool di orchestrazione (output piccolo) esenti dal cap.
const DISPATCHER_PREFIX: &str = "dispatcher_";

/// True se il tool e' esente dal predictive context cap: in `_CAP_EXEMPT_TOOLS` o con
/// prefisso `dispatcher_`. 1:1 con la guardia Python
/// (`_tn in _CAP_EXEMPT_TOOLS or _tn.startswith("dispatcher_")`).
pub fn is_cap_exempt(tool_name: &str) -> bool {
    CAP_EXEMPT_TOOLS.contains(tool_name) || tool_name.starts_with(DISPATCHER_PREFIX)
}

/// Decide se la chiamata farebbe superare `ratio * window` token. PURA.
///
/// - `ratio`: frazione del context window oltre cui si blocca (`predictive_cap_ratio`).
/// - `window`: context window del modello (token), gia' risolto dal catalogo.
/// - `expected_size_bytes`: stima upper-bound del tool_result (vedi
///   [`crate::decisions::tool_dispatch::estimate_tool_result_size_bytes`]).
/// - `current_tokens`: stima del contesto attuale (vedi
///   [`crate::decisions::tool_dispatch::current_context_token_estimate`]).
///
/// Ritorna `None` se la chiamata e' ammessa (proiezione <= cap), altrimenti il
/// messaggio user-facing da iniettare come tool_result d'errore (col SENTINEL in testa),
/// costruito 1:1 col Python.
///
/// NB: l'esenzione per tool ([`is_cap_exempt`]) e' valutata dal chiamante PRIMA di
/// questa funzione (in Python e' il primo `if` di `_predictive_cap_check`); qui restiamo
/// puramente numerici, cosi' la funzione e' una sola domanda ("la proiezione sfora?").
pub fn predictive_cap_check(
    ratio: f64,
    window: i64,
    expected_size_bytes: i64,
    current_tokens: i64,
) -> Option<String> {
    // cap_tokens = int(window * ratio); expected_tokens = int(expected_bytes / 3.5).
    let cap_tokens = (window as f64 * ratio) as i64;
    let expected_tokens =
        (expected_size_bytes as f64 / crate::decisions::tool_dispatch::TOKEN_CHARS_DIVISOR) as i64;
    let projected = current_tokens + expected_tokens;
    if projected <= cap_tokens {
        return None;
    }
    // pct = int(current / max(window,1) * 100).
    let pct = (current_tokens as f64 / window.max(1) as f64 * 100.0) as i64;
    let ratio_pct = (ratio * 100.0) as i64;
    Some(format!(
        "{PREDICTIVE_CAP_SENTINEL}\n\
ATTENZIONE: e' stata bloccata SOLO questa chiamata, NON il task. \
Se questo tool non e' essenziale per la RICHIESTA CORRENTE dell'utente \
(es. l'hai chiamato per via di contenuti storici della conversazione), \
IGNORALO e prosegui col task usando i dati che hai gia' raccolto. \
NON dichiarare il task bloccato per questo motivo.\n\
Dettaglio: context a {current_tokens} token ({pct}% del budget {window}); il \
risultato atteso aggiungerebbe ~{expected_tokens} token oltre il \
{ratio_pct}% (cap={cap_tokens}).\n\
Solo se il tool e' DAVVERO necessario alla richiesta corrente:\n\
- Riduci i parametri (es. length piu' piccolo).\n\
- Usa estrazione strutturata (nexus_extract_figma_structure, \
nexus_extract_pdf_text, nexus_extract_docx_text).\n\
- Oppure dichiara con task_complete outcome=needs_input cosa serve dall'utente."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esenzione_tool() {
        assert!(is_cap_exempt("task_complete"));
        // Canale di chiusura del revisore (Fase B): dichiarato per contratto
        // come ultima azione, cioe' al picco del contesto — mai cap-ato.
        assert!(is_cap_exempt("review_verdict"));
        assert!(is_cap_exempt("nexus_db_tables"));
        assert!(is_cap_exempt("dispatcher_foo"));
        assert!(!is_cap_exempt("read_file"));
        assert!(!is_cap_exempt("nexus_read_attachment"));
    }

    #[test]
    fn sotto_soglia_passa() {
        // window 100_000, ratio 0.8 -> cap 80_000. current 1000 + expected piccolo.
        assert!(predictive_cap_check(0.8, 100_000, 3500, 1000).is_none());
    }

    #[test]
    fn sopra_soglia_blocca_col_sentinel() {
        // current 79_000 + expected (350_000/3.5=100_000) supera 80_000.
        let msg = predictive_cap_check(0.8, 100_000, 350_000, 79_000).unwrap();
        assert!(msg.starts_with(PREDICTIVE_CAP_SENTINEL));
        assert!(msg.contains("context a 79000 token"));
        assert!(msg.contains("cap=80000"));
    }

    #[test]
    fn esattamente_al_cap_passa() {
        // projected == cap_tokens -> None (Python: `if projected <= cap_tokens: return None`).
        // cap = 0.5 * 100 = 50. current 40 + expected(35/3.5=10) = 50.
        assert!(predictive_cap_check(0.5, 100, 35, 40).is_none());
    }
}

/// Golden di parita' 1:1 vs Python per il predictive cap. Carica lo STESSO file
/// `/tmp/golden_dispatch_pure.json` (vedi `gen_golden_dispatch_pure.py`) e valuta i
/// soli gruppi `predictive_cap_check` e `predictive_cap_sentinel`.
#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        input: Value,
        output: Value,
    }

    #[test]
    #[ignore = "richiede /tmp/golden_dispatch_pure.json generato da gen_golden_dispatch_pure.py"]
    fn golden_predictive_cap() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_dispatch_pure.json",
            "gen_golden_dispatch_pure.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.group.as_str() {
                "predictive_cap_check" => {
                    let inp = &c.input;
                    let ratio = inp.get("ratio").and_then(Value::as_f64).unwrap();
                    let window = inp.get("window").and_then(Value::as_i64).unwrap();
                    let exp = inp
                        .get("expected_size_bytes")
                        .and_then(Value::as_i64)
                        .unwrap();
                    let cur = inp.get("current_tokens").and_then(Value::as_i64).unwrap();
                    match predictive_cap_check(ratio, window, exp, cur) {
                        Some(s) => Value::String(s),
                        None => Value::Null,
                    }
                }
                "predictive_cap_sentinel" => Value::String(PREDICTIVE_CAP_SENTINEL.to_string()),
                // Gli altri gruppi (tool_dispatch) sono valutati altrove.
                _ => continue,
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA {} / {}:\n  rust   = {}\n  python = {}",
                c.group, c.case_id, got, c.output
            );
            checked += 1;
        }
        assert!(
            checked >= 5,
            "attesi >= 5 casi predictive_cap, verificati {checked}"
        );
        println!("golden predictive_cap: {checked} casi verificati, tutti verdi");
    }
}
