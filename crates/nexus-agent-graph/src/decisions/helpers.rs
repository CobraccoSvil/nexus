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

/// Funzione PURA: loop G1 CONCLAMATO per CONTEGGIO strutturale di turni AI
/// nella finestra recente. Stesso schema di [`structural_unfulfilled_signal`]
/// (segnali gia' risolti dal chiamante, nessun IO, nessun pattern testuale —
/// regola M): vero SOLO con evidenza di ripetizione, cioe' almeno
/// `min_ai_turns` turni AI DISTINTI osservati nella finestra e NESSUNO di essi
/// produttivo. Con un solo turno AI nella finestra (nessuna ripetizione
/// osservabile) e' strutturalmente `false`, qualunque sia l'iterazione
/// corrente: la soglia sulle iterazioni resta responsabilita' del chiamante.
pub fn structural_loop_stall_signal(
    ai_turns_in_lookback: usize,
    productive_turns_in_lookback: usize,
    min_ai_turns: usize,
) -> bool {
    ai_turns_in_lookback >= min_ai_turns && productive_turns_in_lookback == 0
}

/// Punto unico (regola L): il TURNO CORRENTE richiede azione con tool?
///
/// Fonte autoritativa: il campo `action_oriented` calcolato da router_node.
/// Default conservativo `true` quando il campo manca (None). Vedi
/// `turn_action_oriented` Python (`state.get("action_oriented")`).
pub fn turn_action_oriented(action_oriented: Option<bool>) -> bool {
    action_oriented.unwrap_or(true)
}

/// Intent semanticamente CONVERSAZIONALI (risposta testuale, nessuna azione con
/// tool). Fonte autoritativa, allineata 1:1 con la semantica del classifier
/// Python (`brain/router/agentic_classifier.py`: per `intent=chat` ->
/// `requires_tools=false, agentic_score<=0.2`) e con `_INTENT_TOOL_SUBSET`
/// (`brain/agents/profile_loader.py`: `chat`/`general_chat` -> solo
/// apri-file, nessun tool di esecuzione/scrittura). Tutti gli ALTRI intent
/// (debug/fix/refactor/test/docs/architecture/file_ops/system_admin/code_read/
/// agentic_default) sono operativi (`requires_tools=true`).
const CONVERSATIONAL_INTENTS: &[&str] = &["chat", "general_chat"];

/// Punto unico (regola L): deriva `action_oriented` da un intent gia' RISOLTO.
///
/// E' la replica DETERMINISTICA della mappa intent->azione che il brain Python
/// ottiene dal classifier LLM (`__init__.py:686-707`): un intent conversazionale
/// (`chat`/`general_chat`) NON e' d'azione; ogni altro intent operativo lo e'.
/// Usata quando un intent e' gia' noto a monte (es. `intent_hint` di una
/// disambiguazione risolta, o l'intent del primario nello shadow LLM-Replay) e
/// NON e' disponibile il giudizio LLM per-turno (`requires_tools`/`agentic_score`).
///
/// Niente nome modello qui (regola G): mappa di intent semantici, non di modelli.
/// Il porting completo del classifier LLM nel `RouterNode` (TODO `router.rs`)
/// resta separato; questa funzione copre il caso "intent gia' deciso".
pub fn action_oriented_for_intent(intent: &str) -> bool {
    let intent = intent.trim();
    if intent.is_empty() {
        // Nessun intent noto: conservativo true (parita' col ramo "classifier non
        // disponibile" del Python, `__init__.py:703-707`).
        return true;
    }
    !CONVERSATIONAL_INTENTS.contains(&intent)
}

/// Stili di `tool_choice` (cap.tool_choice_style) che permettono di OBBLIGARE
/// una tool call. `_TC_FORCING_SUPPORTED_STYLES` Python (1:1). Gli stili "none"
/// e "openai_auto" NON permettono il forcing -> non sono in lista.
const TC_FORCING_SUPPORTED_STYLES: &[&str] = &[
    "anthropic_any",
    "openai_required",
    "google_function_calling_any",
];

/// True se lo style di tool_choice del provider permette di obbligare una tool
/// call (`anthropic_any` / `openai_required` / `google_function_calling_any`).
///
/// Pura, niente DB: lo style arriva dalla `ProviderCapability` gia' caricata.
/// Calcola il bool `provider_supports_forcing` consumato da
/// [`should_force_tool_choice`]. Vedi `provider_style_supports_forcing` Python.
pub fn provider_style_supports_forcing(tool_choice_style: Option<&str>) -> bool {
    match tool_choice_style {
        Some(style) if !style.is_empty() => TC_FORCING_SUPPORTED_STYLES.contains(&style),
        _ => false,
    }
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

/// Mappa label complexity del classifier LLM -> score 0-100. Il RICONOSCIMENTO
/// della label delega al punto unico
/// [`super::orchestration_sizing::TaskComplexity::try_parse`] (regola N: un solo
/// parse nel codebase); qui resta solo la conversione in score numerico.
fn complexity_label_score(label: &str) -> Option<i64> {
    use super::orchestration_sizing::TaskComplexity;
    match TaskComplexity::try_parse(label)? {
        TaskComplexity::Low => Some(10),
        TaskComplexity::Medium => Some(40),
        TaskComplexity::High => Some(70),
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
///
/// `performance_tier` viene dal catalog (`ai_price_catalog.performance_tier`,
/// risolto a monte, regola G): sostituisce la vecchia blacklist di substring
/// sul nome modello (`WEAK_MODELS_HINT`, violazione regola G — un nuovo modello
/// "mini" richiedeva una patch al codice). `Some("light")` = modello leggero ->
/// moltiplicatore di budget; qualunque altro valore o `None` = nessun boost.
pub fn compute_iteration_budget(
    prompt: &str,
    performance_tier: Option<&str>,
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
    // Modelli LIGHT (tier dal catalog): piu' budget per arrivare al risultato.
    if performance_tier.map(str::trim) == Some("light") {
        budget = (budget as f64 * config.weak_model_multiplier) as i64;
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
    fn loop_stall_richiede_evidenza_di_ripetizione() {
        // Sotto soglia (2 turni AI su min=3): nessuna ripetizione osservabile,
        // anche con zero produttivi -> non e' stallo.
        assert!(!structural_loop_stall_signal(2, 0, 3));
        // Soglia raggiunta ma con almeno un turno produttivo -> non e' stallo.
        assert!(!structural_loop_stall_signal(5, 1, 3));
        // Soglia raggiunta e zero produttivi -> stallo conclamato.
        assert!(structural_loop_stall_signal(3, 0, 3));
        assert!(structural_loop_stall_signal(8, 0, 3));
    }

    #[test]
    fn action_oriented_default_none() {
        assert!(turn_action_oriented(None));
        assert!(!turn_action_oriented(Some(false)));
    }

    #[test]
    fn action_oriented_da_intent_conversazionale() {
        // chat/general_chat -> NON azione (parita' classifier Python).
        assert!(!action_oriented_for_intent("chat"));
        assert!(!action_oriented_for_intent("general_chat"));
        assert!(!action_oriented_for_intent("  chat  "), "trim applicato");
    }

    #[test]
    fn action_oriented_da_intent_operativo() {
        // Intent operativi -> azione.
        for intent in [
            "debug",
            "fix",
            "refactor",
            "test",
            "docs",
            "architecture",
            "file_ops",
            "system_admin",
            "code_read",
            "agentic_default",
        ] {
            assert!(
                action_oriented_for_intent(intent),
                "{intent} deve essere d'azione"
            );
        }
    }

    #[test]
    fn action_oriented_da_intent_vuoto_conservativo() {
        // Nessun intent noto -> conservativo true (ramo classifier non disponibile).
        assert!(action_oriented_for_intent(""));
        assert!(action_oriented_for_intent("   "));
    }

    #[test]
    fn provider_style_forcing_supportati() {
        assert!(provider_style_supports_forcing(Some("anthropic_any")));
        assert!(provider_style_supports_forcing(Some("openai_required")));
        assert!(provider_style_supports_forcing(Some(
            "google_function_calling_any"
        )));
        // Non supportati / assenti.
        assert!(!provider_style_supports_forcing(Some("openai_auto")));
        assert!(!provider_style_supports_forcing(Some("none")));
        assert!(!provider_style_supports_forcing(Some("")));
        assert!(!provider_style_supports_forcing(None));
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
        // Tier "light" dal catalog (non piu' substring sul nome modello).
        let (budget, _) = compute_iteration_budget("x", Some("light"), Some("high"), None, &cfg);
        // (60 + 280) * 1.5 = 510 -> cap 300.
        assert_eq!(budget, 300);
        // Tier medium/heavy o assente: nessun moltiplicatore.
        let (b_med, _) = compute_iteration_budget("x", Some("medium"), Some("low"), None, &cfg);
        let (b_none, _) = compute_iteration_budget("x", None, Some("low"), None, &cfg);
        assert_eq!(b_med, b_none);
    }

    #[test]
    fn complexity_fullstack() {
        let cfg = AdaptiveBudgetConfig::default();
        let score = estimate_prompt_complexity("crea un sito fullstack", &cfg);
        // crea(3) + fullstack(10) = 13.
        assert_eq!(score, 13);
    }
}
