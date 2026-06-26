//! `g1_accounting`: CONTEGGIO deterministico del gate G1 (re-entry/cap) dell'executor.
//! Porting 1:1 del solo CONTEGGIO da `brain/agents/nodes/__init__.py` (executor_node,
//! blocco ~1882-2042).
//!
//! Punto unico (regola L) della domanda "questa entry e' una re-entry G1 da contare,
//! e il cap e' raggiunto?". La funzione [`g1_accounting`] e' PURA: i segnali arrivano
//! gia' risolti dal chiamante (nessun IO, nessuna lettura DB, nessuna chiamata LLM).
//!
//! ATTENZIONE — separazione delle responsabilita':
//!   - QUI vive SOLO il conteggio: incremento di `g1_reroute_count` e rilevazione del
//!     cap (`>= g1_max_nudges`). Il contatore reale nello state Python e' `g1_reroute_count`.
//!   - La DECISIONE conseguente (escalation / abort / nudge: cosa fare al re-entry o al
//!     cap) NON e' qui: DELEGA a [`crate::decisions::progress_controller::decide`], gia'
//!     portato. Questo modulo non re-decide nulla.
//!
//! Logica di re-entry G1 (1:1 col Python):
//!   conta SE  `iterations >= 1`
//!         AND `prev_stop_reason in {end_turn, stop}`
//!         AND nessun pending_tool_use
//!         AND (`turn_action_oriented` OR `unfulfilled_signal`)
//!         AND NOT `detect_recent_tool_error`  (il modello reagisce a un errore: non e'
//!             "descrittivo" ma "in difficolta'", non si conta — vedi FIX G1 intelligente).
//! Quando si conta, `g1_reroute_count` viene incrementato di 1.
//!
//! Cap: `g1_reroute_count (aggiornato) >= g1_max_nudges` -> cap raggiunto.

use serde::{Deserialize, Serialize};

/// Segnali grezzi del gate G1, gia' risolti dal chiamante (executor).
///
/// I quattro segnali derivati (`action_oriented`, `unfulfilled`, `recent_error`) sono
/// gli stessi punti unici PURI gia' presenti nel crate (regola L): rispettivamente
/// [`crate::decisions::helpers::turn_action_oriented`],
/// [`crate::routing::signals::unfulfilled_signal`] e
/// [`crate::routing::signals::detect_recent_tool_error`]. NON ricalcolarli qui.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct G1Signals {
    /// `stop_reason` del turno PRECEDENTE (None = nessuno / entry anomala).
    pub prev_stop_reason: Option<String>,
    /// Iterazioni gia' alle spalle del run (`state.iterations`).
    pub iterations: i64,
    /// `true` se ci sono `pending_tool_uses` (il turno ha gia' tool call pendenti).
    pub has_pending: bool,
    /// La richiesta del turno corrente e' action-oriented (`turn_action_oriented`).
    pub action_oriented: bool,
    /// L'output e' "non compiuto" (`unfulfilled_signal`: closure_judge/pending/lessicale).
    pub unfulfilled: bool,
    /// Gli ultimi tool_result recenti indicano errore (`detect_recent_tool_error`).
    pub recent_error: bool,
    /// Contatore corrente `g1_reroute_count` (dallo state).
    pub current_count: i64,
    /// Cap massimo di re-entry G1 (`agent.g1_max_nudges`, default 3). DB-driven (regola G).
    pub max_nudges: i64,
}

impl Default for G1Signals {
    fn default() -> Self {
        Self {
            prev_stop_reason: None,
            iterations: 0,
            has_pending: false,
            action_oriented: false,
            unfulfilled: false,
            recent_error: false,
            current_count: 0,
            // Default sicuro identico al Python (_G1_NUDGE_DEFAULT_MAX = 3).
            max_nudges: 3,
        }
    }
}

/// Esito del conteggio G1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct G1Accounting {
    /// `g1_reroute_count` aggiornato (incrementato di 1 sse questa e' una re-entry da contare).
    pub updated_count: i64,
    /// `true` se questa entry e' stata contata come re-entry G1 autentica.
    pub is_reentry: bool,
    /// `true` se il contatore aggiornato raggiunge/supera il cap (`>= max_nudges`).
    pub cap_reached: bool,
}

/// `stop_reason` del turno precedente che marcano una chiusura descrittiva reale.
/// Solo `end_turn`/`stop` (NON `None`): contare anche `None` a iter>=1 eroderebbe il
/// budget anti-loop (vedi FIX mis-conteggio re-entry G1).
fn prev_stop_counts(prev: Option<&str>) -> bool {
    matches!(prev, Some("end_turn") | Some("stop"))
}

/// Punto unico: data la fotografia dei segnali G1, ritorna il contatore aggiornato e
/// se il cap e' raggiunto. PURO. La DECISIONE (escalation/abort/nudge) la prende
/// [`crate::decisions::progress_controller::decide`], non questa funzione.
pub fn g1_accounting(signals: &G1Signals) -> G1Accounting {
    // Re-entry G1 autentica: stesse condizioni del blocco Python (in AND).
    let is_reentry = signals.iterations >= 1
        && prev_stop_counts(signals.prev_stop_reason.as_deref())
        && !signals.has_pending
        && (signals.action_oriented || signals.unfulfilled)
        && !signals.recent_error;

    let updated_count = if is_reentry {
        signals.current_count + 1
    } else {
        signals.current_count
    };

    let cap_reached = updated_count >= signals.max_nudges;

    G1Accounting {
        updated_count,
        is_reentry,
        cap_reached,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reentry_signals() -> G1Signals {
        // Configurazione che soddisfa TUTTE le condizioni di re-entry.
        G1Signals {
            prev_stop_reason: Some("end_turn".to_string()),
            iterations: 3,
            has_pending: false,
            action_oriented: true,
            unfulfilled: false,
            recent_error: false,
            current_count: 0,
            max_nudges: 3,
        }
    }

    #[test]
    fn reentry_incrementa() {
        let r = g1_accounting(&reentry_signals());
        assert!(r.is_reentry);
        assert_eq!(r.updated_count, 1);
        assert!(!r.cap_reached);
    }

    #[test]
    fn stop_reason_none_non_conta() {
        let mut s = reentry_signals();
        s.prev_stop_reason = None;
        let r = g1_accounting(&s);
        assert!(!r.is_reentry);
        assert_eq!(r.updated_count, 0);
    }

    #[test]
    fn errore_recente_non_conta() {
        // Il modello reagisce a un tool_result d'errore: non e' "descrittivo".
        let mut s = reentry_signals();
        s.recent_error = true;
        let r = g1_accounting(&s);
        assert!(!r.is_reentry);
        assert_eq!(r.updated_count, 0);
    }

    #[test]
    fn pending_tool_non_conta() {
        let mut s = reentry_signals();
        s.has_pending = true;
        assert!(!g1_accounting(&s).is_reentry);
    }

    #[test]
    fn iter_zero_non_conta() {
        let mut s = reentry_signals();
        s.iterations = 0;
        assert!(!g1_accounting(&s).is_reentry);
    }

    #[test]
    fn unfulfilled_conta_anche_senza_action_oriented() {
        let mut s = reentry_signals();
        s.action_oriented = false;
        s.unfulfilled = true;
        let r = g1_accounting(&s);
        assert!(r.is_reentry);
        assert_eq!(r.updated_count, 1);
    }

    #[test]
    fn ne_action_ne_unfulfilled_non_conta() {
        let mut s = reentry_signals();
        s.action_oriented = false;
        s.unfulfilled = false;
        assert!(!g1_accounting(&s).is_reentry);
    }

    #[test]
    fn cap_raggiunto_al_terzo() {
        let mut s = reentry_signals();
        s.current_count = 2;
        let r = g1_accounting(&s);
        assert_eq!(r.updated_count, 3);
        assert!(r.cap_reached);
    }

    #[test]
    fn cap_gia_superato_senza_reentry() {
        // Anche senza una re-entry questo turno, se il contatore e' gia' al cap
        // (stato persistito), il cap resta raggiunto (Python: `if count >= max`).
        let mut s = reentry_signals();
        s.prev_stop_reason = None; // niente re-entry
        s.current_count = 3;
        let r = g1_accounting(&s);
        assert!(!r.is_reentry);
        assert_eq!(r.updated_count, 3);
        assert!(r.cap_reached);
    }
}

/// Golden di parita' 1:1 vs Python per il conteggio G1. Carica
/// `/tmp/golden_executor_g1.json` (vedi `gen_golden_executor_g1.py`).
#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;
    use serde_json::{json, Value};

    #[derive(Debug, Deserialize)]
    struct In {
        prev_stop_reason: Option<String>,
        iterations: i64,
        has_pending: bool,
        action_oriented: bool,
        unfulfilled: bool,
        recent_error: bool,
        current_count: i64,
        max_nudges: i64,
    }

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        input: In,
        output: Value,
    }

    #[test]
    #[ignore = "richiede /tmp/golden_executor_g1.json generato da gen_golden_executor_g1.py"]
    fn golden_executor_g1() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_executor_g1.json", "gen_golden_executor_g1.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 12, "attesi >= 12 casi, trovati {}", cases.len());
        for c in &cases {
            assert_eq!(c.group, "g1_accounting");
            let signals = G1Signals {
                prev_stop_reason: c.input.prev_stop_reason.clone(),
                iterations: c.input.iterations,
                has_pending: c.input.has_pending,
                action_oriented: c.input.action_oriented,
                unfulfilled: c.input.unfulfilled,
                recent_error: c.input.recent_error,
                current_count: c.input.current_count,
                max_nudges: c.input.max_nudges,
            };
            let r = g1_accounting(&signals);
            let got = json!({
                "updated_count": r.updated_count,
                "is_reentry": r.is_reentry,
                "cap_reached": r.cap_reached,
            });
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA g1_accounting / {}:\n  rust   = {}\n  python = {}",
                c.case_id, got, c.output
            );
        }
        println!("golden executor_g1: {} casi verificati, tutti verdi", cases.len());
    }
}
