//! `reward`: PUNTO UNICO (regola L) del reward euristico + fusione del reward
//! finale del grafo agentico. Portato 1:1 da `brain/agents/nodes/__init__.py`.
//!
//! Il reward euristico e' usato in DUE punti del brain con la STESSA logica:
//! `reflection_node` (`__init__.py:4355-4362`) e `learner_node` (stesso schema).
//! Per la regola L NON si duplica: la decisione vive QUI, parametrica e pura, e
//! i nodi delegano. Cosi' un nuovo requisito sul reward si aggiunge UNA volta.
//!
//! Tutte le funzioni sono PURE (nessun IO, nessuna lettura DB): gli input
//! (stop_reason, presenza result, iterations, budget, pesi) arrivano dal
//! chiamante (regola G: nessun hardcode di emergenza). Deterministiche e
//! golden-abili 1:1 vs Python.

/// Tetto di iterazioni dell'agente (`brain/agents/nodes/helpers.py:38`:
/// `MAX_AGENT_ITERATIONS = 60`). Usato come fallback del reward euristico quando
/// lo stato non porta un `iteration_budget` valorizzato. NON e' un nome
/// modello/provider (regola G): e' un parametro di loop, costante nel Python.
pub const MAX_AGENT_ITERATIONS: i64 = 60;

/// Reward euristico (`brain/agents/nodes/__init__.py:4355-4362`). Punto unico
/// (regola L): replica ESATTAMENTE la cascata di if del brain, nell'ordine.
///
/// Semantica Python:
/// ```text
/// if stop_reason == "error":            heuristic = 0.0
/// elif iterations >= (iteration_budget or 0) or MAX_AGENT_ITERATIONS:
///                                        heuristic = 0.3
/// elif result:                          heuristic = 1.0
/// else:                                 heuristic = 0.4
/// ```
/// dove `(int(state.get("iteration_budget") or 0) or MAX_AGENT_ITERATIONS)`:
/// se `iteration_budget` e' assente/0/falsy il floor diventa `MAX_AGENT_ITERATIONS`.
///
/// Parametri (tutti risolti a monte, regola G):
/// - `stop_reason`: la stringa snake_case dello stop reason (il brain confronta
///   `stop_reason == "error"` su stringa; qui idem per parita' 1:1). Solo il
///   valore esatto `"error"` produce 0.0.
/// - `result_non_empty`: `true` se `state.result` e' una stringa non vuota
///   (`bool(result)` Python su stringa).
/// - `iterations`: iterazioni eseguite (`int(state.get("iterations") or 0)`).
/// - `iteration_budget`: budget del run (`int(state.get("iteration_budget") or 0)`,
///   gia' normalizzato dal chiamante; 0/assente -> usa `MAX_AGENT_ITERATIONS`).
pub fn heuristic_reward(
    stop_reason: &str,
    result_non_empty: bool,
    iterations: i64,
    iteration_budget: i64,
) -> f64 {
    if stop_reason == "error" {
        return 0.0;
    }
    // `(iteration_budget or 0) or MAX_AGENT_ITERATIONS`: un budget falsy (0)
    // ricade sul tetto massimo. iteration_budget e' gia' `int(... or 0)`.
    let floor = if iteration_budget != 0 {
        iteration_budget
    } else {
        MAX_AGENT_ITERATIONS
    };
    if iterations >= floor {
        return 0.3;
    }
    if result_non_empty {
        return 1.0;
    }
    0.4
}

/// Fusione del reward finale (`brain/agents/nodes/__init__.py:4367-4374`):
/// `heuristic_weight = round(1.0 - reward_weight, 4)` e
/// `final_reward = round(heuristic_weight * heuristic + reward_weight * reflection_score, 4)`.
///
/// `reward_weight` e' `reflection_reward_weight` (config DB, regola G). Replica
/// il rounding di Python (round-half-to-even, vedi [`round_half_even`]).
pub fn final_reward(heuristic: f64, reflection_score: f64, reward_weight: f64) -> f64 {
    let heuristic_weight = round_half_even(1.0 - reward_weight, 4);
    round_half_even(
        heuristic_weight * heuristic + reward_weight * reflection_score,
        4,
    )
}

/// Punteggio aggregato ponderato dalle dimensioni della rubrica
/// (`brain/agents/reflection_rubric.py:192-200`, `aggregate_score`). Media
/// ponderata coi pesi della rubrica, round a 3 decimali (half-to-even).
///
/// `dimensions` e' una sequenza `(valore, peso)`: il chiamante passa i pesi
/// della rubrica (correctness=0.40, completeness=0.30, efficiency=0.15,
/// safety=0.15). Portata per PARITA' completa anche se il nodo non la chiama
/// direttamente (la rubrica e' il punto unico delle dimensioni).
pub fn aggregate_score(dimensions: &[(f64, f64)]) -> f64 {
    let total: f64 = dimensions.iter().map(|(v, w)| v * w).sum();
    round_half_even(total, 3)
}

/// Arrotondamento a `ndigits` decimali con la semantica di Python `round()`:
/// round-half-to-even (banker's rounding) sul VALORE DECIMALE REALE di `f64`.
///
/// NON si puo' usare l'approccio "scala-arrotonda-dividi" (`value * 10^n`):
/// moltiplicare introduce un errore di rappresentazione che crea tie artificiali
/// (es. `0.1235` e' in f64 `0.12349999...` -> Python arrotonda a `0.123`, ma
/// `0.1235 * 1000` da' ESATTAMENTE `123.5` -> `round_ties_even` -> `124`,
/// divergente). Ne' `f64::round` (half-away-from-zero, diverge sui `.5`).
///
/// La formattazione decimale di Rust (`{:.*}`, algoritmo Ryu/Grisu) arrotonda
/// half-to-even sul valore f64 reale, ESATTAMENTE come l'algoritmo `_Py_dg_dtoa`
/// di CPython: formatto a `ndigits` cifre e ri-parsifico. Verificato 1:1 col
/// golden Python (casi `pr_round`, `vr_round3`, `fr_round4`, tie `.5`).
pub fn round_half_even(value: f64, ndigits: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    // `{:.*}` = `ndigits` cifre decimali, arrotondamento half-to-even del runtime.
    let formatted = format!("{value:.ndigits$}");
    formatted.parse::<f64>().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_error_e_zero() {
        // stop_reason == "error" -> 0.0, indipendente dal resto.
        assert_eq!(heuristic_reward("error", true, 1, 60), 0.0);
        assert_eq!(heuristic_reward("error", false, 100, 0), 0.0);
    }

    #[test]
    fn heuristic_budget_superato_e_03() {
        // iterations >= budget esplicito.
        assert_eq!(heuristic_reward("end_turn", true, 10, 10), 0.3);
        // budget 0 -> floor MAX_AGENT_ITERATIONS (60).
        assert_eq!(heuristic_reward("end_turn", true, 60, 0), 0.3);
        assert_eq!(heuristic_reward("end_turn", true, 59, 0), 1.0, "59 < 60 -> non superato");
    }

    #[test]
    fn heuristic_result_presente_e_1() {
        assert_eq!(heuristic_reward("end_turn", true, 3, 60), 1.0);
    }

    #[test]
    fn heuristic_nessun_result_e_04() {
        assert_eq!(heuristic_reward("end_turn", false, 3, 60), 0.4);
    }

    #[test]
    fn final_reward_fusione() {
        // weight=0.3, heuristic=1.0, score=0.8 ->
        // hw = round(0.7,4)=0.7; final = round(0.7*1.0 + 0.3*0.8,4)=round(0.94,4)=0.94
        let fr = final_reward(1.0, 0.8, 0.3);
        assert!((fr - 0.94).abs() < 1e-9, "atteso 0.94, ottenuto {fr}");
    }

    #[test]
    fn round_half_even_replica_python() {
        // Tie esatti rappresentabili: half-to-even.
        assert_eq!(round_half_even(0.5, 0), 0.0, "0.5 -> 0 (pari)");
        assert_eq!(round_half_even(1.5, 0), 2.0, "1.5 -> 2 (pari)");
        assert_eq!(round_half_even(2.5, 0), 2.0, "2.5 -> 2 (pari)");
        // round-away (f64::round) darebbe 1.0/2.0/3.0: qui NO.
    }

    #[test]
    fn aggregate_score_ponderato() {
        // tutte 1.0 coi pesi rubrica -> 1.0.
        let dims = [(1.0, 0.40), (1.0, 0.30), (1.0, 0.15), (1.0, 0.15)];
        assert_eq!(aggregate_score(&dims), 1.0);
        // tutte 0.5 -> 0.5.
        let dims2 = [(0.5, 0.40), (0.5, 0.30), (0.5, 0.15), (0.5, 0.15)];
        assert_eq!(aggregate_score(&dims2), 0.5);
    }
}
