//! `helpers`: funzioni pure decisionali dei nodi agentici. Porting 1:1 di
//! `brain/agents/nodes/helpers.py` (solo le funzioni PURE richieste dalla Fase
//! 2a; le `route_after_*` sono nel PR 2b).
//!
//! Le funzioni che in Python leggono i settings dal DB qui ricevono la config
//! come PARAMETRO esplicito ([`AdaptiveBudgetConfig`]) per restare pure e
//! testabili (regola G: nessun hardcode di emergenza, nessuna lettura DB).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Funzione PURA: decide se forzare `tool_choice` per il turno corrente
/// (ADR 0018 leva 2). Ritorna True solo quando TUTTE le condizioni sono vere.
/// Vedi `should_force_tool_choice` Python.
#[allow(clippy::too_many_arguments)]
pub fn should_force_tool_choice(
    tools_available: bool,
    action_oriented: bool,
    iteration: i64,
    in_discovery_phase: bool,
    provider_supports_forcing: bool,
    enabled: bool,
    max_iteration: i64,
) -> bool {
    if !enabled {
        return false;
    }
    if !tools_available {
        return false;
    }
    if !action_oriented {
        return false;
    }
    if in_discovery_phase {
        return false;
    }
    if !provider_supports_forcing {
        return false;
    }
    if iteration > max_iteration {
        return false;
    }
    true
}

/// Funzione PURA: segnale STRUTTURALE di stop prematuro (ADR 0018 leva 1/c).
/// Vedi `structural_unfulfilled_signal` Python.
pub fn structural_unfulfilled_signal(
    had_tools_available: bool,
    no_tool_call_this_turn: bool,
    action_oriented: bool,
    iteration: i64,
    max_iteration: i64,
) -> bool {
    if !had_tools_available {
        return false;
    }
    if !no_tool_call_this_turn {
        return false;
    }
    if !action_oriented {
        return false;
    }
    if iteration > max_iteration {
        return false;
    }
    true
}

/// Punto unico (regola L): il TURNO CORRENTE richiede azione con tool?
///
/// Fonte autoritativa: il campo `action_oriented` calcolato da router_node.
/// Default conservativo `true` quando il campo manca (None). Vedi
/// `turn_action_oriented` Python (`state.get("action_oriented")`).
pub fn turn_action_oriented(action_oriented: Option<bool>) -> bool {
    action_oriented.unwrap_or(true)
}

/// Config dell'adaptive budget (PARAMETRO esplicito, no lettura DB: regola G).
/// Mappa i settings `agent.iteration_budget.*` / `agent.complexity.*`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdaptiveBudgetConfig {
    pub iteration_budget_base: i64,
    pub iteration_budget_per_complexity_point: i64,
    pub iteration_budget_max: i64,
    pub complexity_step_marker_points: i64,
    pub complexity_file_path_points: i64,
    /// Pesi keyword (substring -> peso). BTreeMap per determinismo dell'ordine.
    pub complexity_keyword_weights: BTreeMap<String, i64>,
    pub weak_model_multiplier: f64,
}

impl Default for AdaptiveBudgetConfig {
    fn default() -> Self {
        // Default documentati identici a `_ADAPTIVE_BUDGET_DEFAULTS` Python.
        let mut weights = BTreeMap::new();
        for (k, v) in [
            ("create", 3),
            ("write_file", 2),
            ("install", 2),
            ("build", 2),
            ("systemctl", 2),
            ("docker", 2),
            ("pnpm", 2),
            ("npm", 1),
            ("deploy", 3),
            ("migrate", 3),
            ("refactor", 4),
            ("fullstack", 10),
            ("end-to-end", 8),
            ("backend", 2),
            ("frontend", 2),
            ("database", 2),
            ("crea", 3),
            ("installa", 2),
            ("esegui", 2),
            ("avvia", 2),
            ("configura", 2),
        ] {
            weights.insert(k.to_string(), v);
        }
        Self {
            iteration_budget_base: 60,
            iteration_budget_per_complexity_point: 4,
            iteration_budget_max: 300,
            complexity_step_marker_points: 5,
            complexity_file_path_points: 2,
            complexity_keyword_weights: weights,
            weak_model_multiplier: 1.5,
        }
    }
}

// Regex compilati una volta (identici a `_STEP_MARKER_RE` / `_FILE_PATH_RE`).
static STEP_MARKER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:\d+\.|step\s+\d+|task\s+\d+|phase\s+\d+|fase\s+\d+|passo\s+\d+)\b")
        .expect("regex step marker valido")
});

static FILE_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:/[a-zA-Z0-9_.-]+){2,}|[a-zA-Z0-9_-]+\.(?:js|ts|tsx|jsx|py|rs|json|yml|yaml|sql|md|env|html|css|toml|sh)",
    )
    .expect("regex file path valido")
});

/// Hint di modelli "weak" (substring sul model_id). Vedi `_WEAK_MODELS_HINT`.
const WEAK_MODELS_HINT: &[&str] = &["mini", "nano", "haiku", "lite", "small", "flash-lite"];

/// Mappa label complexity del classifier LLM -> score 0-100. Vedi
/// `_COMPLEXITY_LABEL_SCORE` Python.
fn complexity_label_score(label: &str) -> Option<i64> {
    match label {
        "low" => Some(10),
        "medium" => Some(40),
        "high" => Some(70),
        _ => None,
    }
}

/// Stima la complessita' del task come score 0-100. Deterministico. Vedi
/// `estimate_prompt_complexity` Python (config PASSATA, non letta da DB).
pub fn estimate_prompt_complexity(prompt: &str, config: &AdaptiveBudgetConfig) -> i64 {
    if prompt.is_empty() {
        return 0;
    }
    let text = prompt.to_lowercase();
    let mut score: i64 = 0;
    // Keyword weighted match (substring).
    for (keyword, weight) in &config.complexity_keyword_weights {
        if text.contains(keyword.as_str()) {
            score += *weight;
        }
    }
    // Step markers (1., 2., step N, fase N, ...): conteggio dei match.
    let n_steps = STEP_MARKER_RE.find_iter(prompt).count() as i64;
    score += n_steps * config.complexity_step_marker_points;
    // File path / extension markers.
    let n_paths = FILE_PATH_RE.find_iter(prompt).count() as i64;
    score += n_paths * config.complexity_file_path_points;
    score.min(100)
}

/// Calcola il budget di iterazioni per un run agente. Ritorna
/// `(iter_budget, complexity_score)`. Vedi `compute_iteration_budget` Python
/// (config PASSATA come parametro).
pub fn compute_iteration_budget(
    prompt: &str,
    model: Option<&str>,
    classifier_complexity: Option<&str>,
    agentic_score: Option<f64>,
    config: &AdaptiveBudgetConfig,
) -> (i64, i64) {
    let label = classifier_complexity.unwrap_or("").trim().to_lowercase();
    let score = if let Some(mut s) = complexity_label_score(&label) {
        // Boost dall'agentic_score: un task molto multi-step merita piu' budget.
        if let Some(a) = agentic_score {
            let clamped = a.clamp(0.0, 1.0);
            s = (s + (clamped * 30.0) as i64).min(100);
        }
        s
    } else {
        // Fallback lessicale (keyword it/en).
        estimate_prompt_complexity(prompt, config)
    };
    let base = config.iteration_budget_base;
    let per_pt = config.iteration_budget_per_complexity_point;
    let mut budget = base + per_pt * score;
    // Modelli weak (mini/nano/haiku/lite): piu' budget per arrivare al risultato.
    if let Some(m) = model {
        let ml = m.to_lowercase();
        if WEAK_MODELS_HINT.iter().any(|h| ml.contains(h)) {
            budget = (budget as f64 * config.weak_model_multiplier) as i64;
        }
    }
    (budget.min(config.iteration_budget_max), score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_tool_choice_tutte_vere() {
        assert!(should_force_tool_choice(
            true, true, 1, false, true, true, 2
        ));
    }

    #[test]
    fn force_tool_choice_oltre_soglia() {
        assert!(!should_force_tool_choice(
            true, true, 3, false, true, true, 2
        ));
    }

    #[test]
    fn action_oriented_default_none() {
        assert!(turn_action_oriented(None));
        assert!(!turn_action_oriented(Some(false)));
    }

    #[test]
    fn budget_label_high() {
        let cfg = AdaptiveBudgetConfig::default();
        let (budget, score) = compute_iteration_budget("x", None, Some("high"), None, &cfg);
        assert_eq!(score, 70);
        // 60 + 4*70 = 340 -> cap 300.
        assert_eq!(budget, 300);
    }

    #[test]
    fn budget_weak_model_cap() {
        let cfg = AdaptiveBudgetConfig::default();
        let (budget, _) =
            compute_iteration_budget("x", Some("gpt-4o-mini"), Some("high"), None, &cfg);
        // (60 + 280) * 1.5 = 510 -> cap 300.
        assert_eq!(budget, 300);
    }

    #[test]
    fn complexity_fullstack() {
        let cfg = AdaptiveBudgetConfig::default();
        let score = estimate_prompt_complexity("crea un sito fullstack", &cfg);
        // crea(3) + fullstack(10) = 13.
        assert_eq!(score, 13);
    }
}
