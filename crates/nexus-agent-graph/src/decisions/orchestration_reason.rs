//! `orchestration_reason`: parte PURA (regola L) del meta-reasoner di
//! ORCHESTRAZIONE. GEMELLO di [`crate::decisions::meta_reason`] (stesso stile,
//! golden-abile in isolamento), su tipi DISGIUNTI: nessun tipo condiviso col
//! recovery, nessun enum wrapper (regola L, design v2).
//!
//! Il ragionamento LLM contestuale (SE/COME fare la plan-phase, decomporre,
//! delegare) sostituisce l'euristica fissa (`is_eligible`/`should_parallelize`),
//! ma la parte non-deterministica (la chiamata LLM) vive dietro il metodo
//! [`crate::runtime::ports::MetaReasonerPort::orchestrate`]. Qui sta SOLO la
//! logica deterministica (regola M: nessuna prosa, solo segnali strutturati):
//!   - [`build_orchestration_context`]: costruzione DETERMINISTICA del
//!     [`OrchestrationContext`] dai segnali gia' risolti a monte
//!     (routing/context_reduction/depth/cost).
//!   - [`orch_epoch`]: epoca di lavoro STABILE (chiave idempotenza/replay). In
//!     Fase 1 (plan-entry) e' per-run: la decisione di orchestrazione all'ingresso
//!     e' presa UNA volta per run (segue la disciplina di
//!     [`crate::decisions::meta_reason::work_epoch`]).
//!   - [`validate_orch_move`]: PUNTO UNICO di validazione dell'output JSON dell'LLM
//!     contro l'enum CHIUSO [`OrchestrationMove`]; qualunque forma malformata /
//!     `Decompose` senza blocchi / `DelegateSubagents` senza task o vietata /
//!     `ParallelIsolated` senza isolamento fisico degrada a
//!     [`OrchestrationMove::Fallback`] (rete di sicurezza: euristica esistente).

use crate::runtime::ports::{
    ContextPressure, Coordination, OrchPhase, OrchestrationContext, OrchestrationMove,
};

/// Soglia (rapporto used/limit) oltre cui la pressione del contesto e' MEDIA.
/// Soglia di CALCOLO PURO deterministico (non config di business: e' la
/// derivazione strutturata di [`ContextPressure`] dai token, come le soglie
/// aritmetiche di [`crate::decisions::meta_reason::work_epoch`]), non un magic
/// fallback su un comportamento (regola G): non accende feature ne' sceglie
/// modelli, mappa solo un rapporto in un enum.
const PRESSURE_MEDIUM_RATIO: f64 = 0.60;

/// Soglia (rapporto used/limit) oltre cui la pressione del contesto e' ALTA.
const PRESSURE_HIGH_RATIO: f64 = 0.85;

/// Deriva la [`ContextPressure`] DETERMINISTICAMENTE dal rapporto
/// `context_tokens_used / context_window_limit` (regola M: segnale strutturato,
/// mai prosa). `limit <= 0` (ignoto) o `used <= 0` -> [`ContextPressure::Low`]
/// (nessuna informazione = nessuna pressione presunta). Punto unico del calcolo:
/// [`build_orchestration_context`] e i test lo riusano.
pub fn context_pressure_from_tokens(used: i64, limit: i64) -> ContextPressure {
    if limit <= 0 || used <= 0 {
        return ContextPressure::Low;
    }
    let ratio = used as f64 / limit as f64;
    if ratio >= PRESSURE_HIGH_RATIO {
        ContextPressure::High
    } else if ratio >= PRESSURE_MEDIUM_RATIO {
        ContextPressure::Medium
    } else {
        ContextPressure::Low
    }
}

/// Epoca di lavoro STABILE per la chiave di idempotenza/replay del meta-reasoner
/// di orchestrazione (`orch_move::{phase}::{orch_epoch}`). In Fase 1 la sola fase
/// e' [`OrchPhase::PlanEntry`]: la decisione di orchestrazione e' presa
/// all'INGRESSO del run, UNA volta -> l'epoca e' banalmente per-run (0). Segue la
/// disciplina di [`crate::decisions::meta_reason::work_epoch`]: valore stabile e
/// deterministico, non la coda-segnali volatile. Le fasi successive (worktree)
/// potranno far avanzare l'epoca su cambi macroscopici senza toccare i chiamanti.
pub fn orch_epoch(phase: OrchPhase) -> i64 {
    match phase {
        OrchPhase::PlanEntry => 0,
    }
}

/// Costruisce il [`OrchestrationContext`] deterministicamente dai segnali gia'
/// risolti a monte (regola M: tutti strutturati). `delegation_forbidden` NON e'
/// dedotto qui da prosa: e' l'aggregazione DETERMINISTICA delle guard di delega
/// (depth oltre soglia / cost oltre cap / policy esplicita), calcolata dal punto
/// unico [`delegation_forbidden`]. La [`ContextPressure`] deriva da used/limit col
/// punto unico [`context_pressure_from_tokens`].
#[allow(clippy::too_many_arguments)]
pub fn build_orchestration_context(
    phase: OrchPhase,
    user_intent: Option<&str>,
    behavior_mode: &str,
    token_budget: i64,
    task_complexity: i64,
    agentic_score: i64,
    is_ambiguous: bool,
    plan_exists: bool,
    context_tokens_used: i64,
    context_window_limit: i64,
    history_len: i64,
    subagent_depth: i64,
    subagent_depth_limit: i64,
    cost_spent_usd: f64,
    cost_cap_usd: f64,
    policy_forbids_delegation: bool,
) -> OrchestrationContext {
    let context_pressure = context_pressure_from_tokens(context_tokens_used, context_window_limit);
    let delegation_forbidden = delegation_forbidden(
        subagent_depth,
        subagent_depth_limit,
        cost_spent_usd,
        cost_cap_usd,
        policy_forbids_delegation,
    );
    OrchestrationContext {
        phase,
        user_intent: user_intent.map(str::to_string),
        behavior_mode: behavior_mode.to_string(),
        token_budget,
        task_complexity,
        agentic_score,
        is_ambiguous,
        plan_exists,
        context_tokens_used,
        context_window_limit,
        history_len,
        context_pressure,
        subagent_depth,
        cost_spent_usd,
        cost_cap_usd,
        delegation_forbidden,
    }
}

/// Aggrega DETERMINISTICAMENTE le guard di delega in un solo booleano (regola M +
/// L: punto unico della guard, i chiamanti non re-implementano i confronti). La
/// delega e' VIETATA se ALMENO UNA guard scatta:
///   - `subagent_depth >= subagent_depth_limit` (limite > 0): annidamento oltre
///     soglia (evita ricorsione incontrollata di sub-agenti);
///   - `cost_cap_usd > 0 && cost_spent_usd >= cost_cap_usd`: budget di costo del
///     run esaurito;
///   - `policy_forbids_delegation`: divieto esplicito di policy (a monte).
/// Un `subagent_depth_limit <= 0` o `cost_cap_usd <= 0` DISATTIVA quella guard
/// (nessun cap configurato) — non e' un magic fallback: e' l'assenza esplicita del
/// vincolo (regola G, i valori arrivano dai settings risolti a monte).
pub fn delegation_forbidden(
    subagent_depth: i64,
    subagent_depth_limit: i64,
    cost_spent_usd: f64,
    cost_cap_usd: f64,
    policy_forbids_delegation: bool,
) -> bool {
    let depth_exceeded = subagent_depth_limit > 0 && subagent_depth >= subagent_depth_limit;
    let cost_exceeded = cost_cap_usd > 0.0 && cost_spent_usd >= cost_cap_usd;
    policy_forbids_delegation || depth_exceeded || cost_exceeded
}

/// Valida l'output JSON dell'LLM contro l'enum CHIUSO [`OrchestrationMove`]. Punto
/// unico (regola L): il nodo/gate di orchestrazione e l'impl della porta chiamano
/// SOLO questa funzione. Qualunque forma malformata / con collezione vuota dove
/// serve / mossa non applicabile per una guard deterministica degrada a
/// [`OrchestrationMove::Fallback`] (rete di sicurezza: l'euristica esistente
/// `is_eligible`/`should_parallelize`).
///
/// `isolation_available` e' la guard fisica anti-race (regola verificata sul
/// codice: `dag_scheduler` non ha campi file, i sub-run condividono la root
/// per-sessione -> due paralleli che scrivono si pesterebbero). In Fase 1
/// `isolation_available` e' SEMPRE `false` -> [`Coordination::ParallelIsolated`]
/// e' SEMPRE rifiutata (l'isolamento fisico via worktree e' una fase infra
/// successiva). L'anti-race NON si basa su file predetti dall'LLM ma su questa
/// guard fisica.
///
/// `delegation_forbidden` (aggregato deterministico di depth/cost/policy dal
/// [`OrchestrationContext`]) e' passato ESPLICITAMENTE: se `true`, qualunque
/// [`OrchestrationMove::DelegateSubagents`] degrada a `Fallback` (la decisione LLM
/// non puo' scavalcare la guard deterministica).
pub fn validate_orch_move(
    raw: &serde_json::Value,
    isolation_available: bool,
    delegation_forbidden: bool,
) -> OrchestrationMove {
    let mv: OrchestrationMove = match serde_json::from_value(raw.clone()) {
        Ok(m) => m,
        Err(_) => return OrchestrationMove::Fallback,
    };
    match &mv {
        // Decompose senza blocchi: mossa vuota, inutile -> euristica.
        OrchestrationMove::Decompose { blocks } if blocks.is_empty() => OrchestrationMove::Fallback,
        // Delega senza task: mossa vuota -> euristica.
        OrchestrationMove::DelegateSubagents { tasks, .. } if tasks.is_empty() => {
            OrchestrationMove::Fallback
        }
        // Delega vietata da guard deterministica (depth/cost/policy): la decisione
        // LLM non scavalca la guard -> euristica.
        OrchestrationMove::DelegateSubagents { .. } if delegation_forbidden => {
            OrchestrationMove::Fallback
        }
        // Delega parallela senza isolamento fisico (Fase 1: isolation_available
        // sempre false): anti-race, rifiutata -> euristica.
        OrchestrationMove::DelegateSubagents {
            coordination: Coordination::ParallelIsolated,
            ..
        } if !isolation_available => OrchestrationMove::Fallback,
        _ => mv,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ports::{PlanBlock, SubTask};
    use serde_json::json;

    #[test]
    fn context_pressure_deriva_da_used_su_limit() {
        // Limite/used ignoti -> Low (nessuna pressione presunta).
        assert_eq!(context_pressure_from_tokens(0, 0), ContextPressure::Low);
        assert_eq!(context_pressure_from_tokens(100, 0), ContextPressure::Low);
        assert_eq!(context_pressure_from_tokens(0, 1000), ContextPressure::Low);
        // Sotto la soglia media -> Low.
        assert_eq!(context_pressure_from_tokens(500, 1000), ContextPressure::Low);
        // Tra media e alta -> Medium.
        assert_eq!(context_pressure_from_tokens(700, 1000), ContextPressure::Medium);
        // Oltre la soglia alta -> High.
        assert_eq!(context_pressure_from_tokens(900, 1000), ContextPressure::High);
        // Esattamente sulla soglia alta -> High (>=).
        assert_eq!(context_pressure_from_tokens(850, 1000), ContextPressure::High);
    }

    #[test]
    fn orch_epoch_plan_entry_e_stabile() {
        assert_eq!(orch_epoch(OrchPhase::PlanEntry), 0);
        // Deterministica a chiamate ripetute (idempotenza/replay).
        assert_eq!(orch_epoch(OrchPhase::PlanEntry), orch_epoch(OrchPhase::PlanEntry));
    }

    #[test]
    fn delegation_forbidden_scatta_su_ogni_guard() {
        // Nessuna guard attiva -> permessa.
        assert!(!delegation_forbidden(0, 3, 0.0, 10.0, false));
        // Policy esplicita -> vietata.
        assert!(delegation_forbidden(0, 3, 0.0, 10.0, true));
        // Depth oltre soglia -> vietata.
        assert!(delegation_forbidden(3, 3, 0.0, 10.0, false));
        assert!(delegation_forbidden(4, 3, 0.0, 10.0, false));
        // Depth sotto soglia -> permessa.
        assert!(!delegation_forbidden(2, 3, 0.0, 10.0, false));
        // Cost oltre cap -> vietata.
        assert!(delegation_forbidden(0, 3, 10.0, 10.0, false));
        assert!(delegation_forbidden(0, 3, 12.5, 10.0, false));
        // Cost sotto cap -> permessa.
        assert!(!delegation_forbidden(0, 3, 9.9, 10.0, false));
        // Cap/limite disattivati (<= 0): guard inattiva.
        assert!(!delegation_forbidden(99, 0, 999.0, 0.0, false));
    }

    #[test]
    fn build_orchestration_context_da_segnali_strutturati() {
        let ctx = build_orchestration_context(
            OrchPhase::PlanEntry,
            Some("aggiungi endpoint REST"),
            "automatic",
            50_000,
            7,
            8,
            false,
            false,
            700,
            1000,
            12,
            1,
            3,
            0.5,
            10.0,
            false,
        );
        assert_eq!(ctx.phase, OrchPhase::PlanEntry);
        assert_eq!(ctx.user_intent.as_deref(), Some("aggiungi endpoint REST"));
        assert_eq!(ctx.behavior_mode, "automatic");
        assert_eq!(ctx.token_budget, 50_000);
        assert_eq!(ctx.task_complexity, 7);
        assert_eq!(ctx.agentic_score, 8);
        assert!(!ctx.is_ambiguous);
        assert!(!ctx.plan_exists);
        assert_eq!(ctx.context_tokens_used, 700);
        assert_eq!(ctx.context_window_limit, 1000);
        assert_eq!(ctx.history_len, 12);
        // 700/1000 = 0.70 -> Medium (punto unico context_pressure).
        assert_eq!(ctx.context_pressure, ContextPressure::Medium);
        assert_eq!(ctx.subagent_depth, 1);
        assert_eq!(ctx.cost_spent_usd, 0.5);
        assert_eq!(ctx.cost_cap_usd, 10.0);
        // depth 1 < 3, cost 0.5 < 10.0, policy false -> delega permessa.
        assert!(!ctx.delegation_forbidden);
    }

    #[test]
    fn build_orchestration_context_aggrega_guard_delega() {
        // depth 3 >= limite 3 -> delegation_forbidden nel contesto costruito.
        let ctx = build_orchestration_context(
            OrchPhase::PlanEntry,
            None,
            "confirm",
            0,
            0,
            0,
            true,
            true,
            0,
            0,
            0,
            3,
            3,
            0.0,
            0.0,
            false,
        );
        assert!(ctx.delegation_forbidden);
        assert!(ctx.is_ambiguous);
        assert!(ctx.plan_exists);
        // limite/used contesto ignoti -> Low.
        assert_eq!(ctx.context_pressure, ContextPressure::Low);
    }

    #[test]
    fn validate_orch_move_forme_valide() {
        let m = validate_orch_move(&json!({"move": "run_inline"}), false, false);
        assert_eq!(m, OrchestrationMove::RunInline);

        let m = validate_orch_move(&json!({"move": "plan_phase", "decompose": true}), false, false);
        assert_eq!(m, OrchestrationMove::PlanPhase { decompose: true });

        let m = validate_orch_move(
            &json!({
                "move": "decompose",
                "blocks": [{"title": "setup", "description": "prepara lo scaffold"}]
            }),
            false,
            false,
        );
        assert_eq!(
            m,
            OrchestrationMove::Decompose {
                blocks: vec![PlanBlock {
                    title: "setup".into(),
                    description: "prepara lo scaffold".into()
                }]
            }
        );

        // Delega sequenziale con task e senza guard -> valida.
        let m = validate_orch_move(
            &json!({
                "move": "delegate_subagents",
                "tasks": [{"task_description": "scrivi i test", "kind": "coder"}],
                "coordination": "sequential"
            }),
            false,
            false,
        );
        assert_eq!(
            m,
            OrchestrationMove::DelegateSubagents {
                tasks: vec![SubTask {
                    task_description: "scrivi i test".into(),
                    kind: "coder".into()
                }],
                coordination: Coordination::Sequential
            }
        );
    }

    #[test]
    fn validate_orch_move_malformato_degrada_a_fallback() {
        // Enum sconosciuto.
        assert_eq!(
            validate_orch_move(&json!({"move": "boh"}), false, false),
            OrchestrationMove::Fallback
        );
        // Nessun tag "move".
        assert_eq!(
            validate_orch_move(&json!({"foo": "bar"}), false, false),
            OrchestrationMove::Fallback
        );
        // Decompose senza blocchi.
        assert_eq!(
            validate_orch_move(&json!({"move": "decompose", "blocks": []}), false, false),
            OrchestrationMove::Fallback
        );
        // Delega senza task.
        assert_eq!(
            validate_orch_move(
                &json!({"move": "delegate_subagents", "tasks": [], "coordination": "sequential"}),
                false,
                false
            ),
            OrchestrationMove::Fallback
        );
    }

    #[test]
    fn validate_orch_move_parallel_isolated_rifiutata_senza_isolamento() {
        let raw = json!({
            "move": "delegate_subagents",
            "tasks": [{"task_description": "compila", "kind": "coder"}],
            "coordination": "parallel_isolated"
        });
        // Fase 1: isolation_available = false -> Fallback (anti-race fisico).
        assert_eq!(
            validate_orch_move(&raw, false, false),
            OrchestrationMove::Fallback
        );
        // Con isolamento fisico disponibile (fase infra successiva) -> ammessa.
        assert_eq!(
            validate_orch_move(&raw, true, false),
            OrchestrationMove::DelegateSubagents {
                tasks: vec![SubTask {
                    task_description: "compila".into(),
                    kind: "coder".into()
                }],
                coordination: Coordination::ParallelIsolated
            }
        );
    }

    #[test]
    fn validate_orch_move_delega_vietata_da_guard_deterministica() {
        let raw = json!({
            "move": "delegate_subagents",
            "tasks": [{"task_description": "x", "kind": "coder"}],
            "coordination": "sequential"
        });
        // delegation_forbidden = true (depth/cost/policy a monte) -> Fallback:
        // la decisione LLM non scavalca la guard deterministica (regola M).
        assert_eq!(
            validate_orch_move(&raw, false, true),
            OrchestrationMove::Fallback
        );
        // Senza guard -> la stessa mossa e' valida.
        assert!(matches!(
            validate_orch_move(&raw, false, false),
            OrchestrationMove::DelegateSubagents { .. }
        ));
    }
}
