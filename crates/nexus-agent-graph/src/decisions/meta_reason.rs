//! `meta_reason`: parte PURA (regola L) del meta-reasoner di recovery-da-stallo.
//!
//! Il ragionamento LLM contestuale sostituisce la RISPOSTA fissa allo stallo
//! (`progress_controller::decide`), ma la parte non-deterministica (la chiamata
//! LLM) vive dietro la porta [`crate::runtime::ports::MetaReasonerPort`]. Qui
//! sta SOLO la logica deterministica e golden-abile in isolamento (come
//! [`crate::decisions::escalation`]):
//!   - [`build_stall_context`]: costruzione DETERMINISTICA del
//!     [`StallContext`] dai segnali gia' esistenti dell'executor (regola M:
//!     nessuna prosa, solo segnali strutturati).
//!   - [`validate_move`]: validazione dell'output JSON dell'LLM contro l'enum
//!     CHIUSO [`RecoveryMove`] e il vocabolario `blocker` ADR 0034; qualunque
//!     forma malformata / enum sconosciuto / campo vuoto / blocker fuori
//!     vocabolario degrada a [`RecoveryMove::Fallback`] (rete di sicurezza).
//!   - [`translate`]: traduzione [`RecoveryMove`] -> [`ProgressDecision`] (STESSA
//!     struct del ramo fisso: NON un vocabolario di intenti parallelo, regola L).
//!   - [`work_epoch`]: epoca di lavoro STABILE (chiave idempotenza/replay): avanza
//!     solo sui cambi macroscopici, non sulla coda-segnali volatile.

use crate::decisions::progress_controller::{Action, Axis, ProgressDecision, ProgressSignals};
use crate::runtime::ports::{RecoveryMove, StallContext};

/// Vocabolario CHIUSO dei `blocker` strutturati (ADR 0034 `task_complete`). Un
/// `DeclareBlocked` con un `blocker` fuori da questa lista e' malformato e
/// degrada a [`RecoveryMove::Fallback`] (regola M: enum stabile, non prosa).
pub const VALID_BLOCKERS: &[&str] = &[
    "dependency",
    "credential",
    "permission",
    "service",
    "request_ambiguity",
    "safety",
];

/// Epoca di lavoro STABILE per la chiave di idempotenza/replay del meta-reasoner
/// (`stall_move::{axis}::{work_epoch}`). Avanza SOLO sui cambi macroscopici del
/// run — nuovo todo (`todo_seq`), escalation (`escalations`), bump del floor
/// anti-loop (`repeat_scan_floor`) — e NON include la coda-segnali volatile: lo
/// stesso stallo non riconsulta l'LLM anche se i `tool_result` variano (anti
/// meta-loop + determinismo replay). Somma monotona: cambia se e solo se uno dei
/// contatori avanza.
pub fn work_epoch(todo_seq: i64, escalations: i64, repeat_scan_floor: i64) -> i64 {
    todo_seq.max(0) + escalations.max(0) + repeat_scan_floor.max(0)
}

/// Costruisce il [`StallContext`] deterministicamente dai segnali gia' risolti
/// dall'executor (regola M: tutti strutturati). `axis` e' l'asse in stallo gia'
/// individuato dai detector; `signals` sono i [`ProgressSignals`] del turno;
/// gli altri parametri sono i segnali cross-cutting (esito tool strutturato,
/// redazione, clarify cross-run, file modificati, epoca).
#[allow(clippy::too_many_arguments)]
pub fn build_stall_context(
    axis: Axis,
    signals: &ProgressSignals,
    recent_tool_signatures: &[String],
    last_tool_outcome: Option<&str>,
    redaction_rejected: bool,
    repeated_clarify_count: i64,
    user_intent: Option<&str>,
    modified_files: &[String],
    already_asked_user: bool,
    work_epoch: i64,
) -> StallContext {
    let key = axis.as_str();
    let (label, count) = match axis {
        Axis::RepeatedAction => signals
            .repeated_action
            .as_ref()
            .map(|(l, c)| (Some(l.clone()), *c))
            .unwrap_or((None, 0)),
        Axis::Exploration => (None, signals.exploration_count),
        Axis::ResourceReallocation => (None, signals.reallocation_count),
        Axis::RepeatedUserQuestion => (None, repeated_clarify_count),
        Axis::Signature => (signals.signature_loop_tool.clone(), 0),
        Axis::G1Descriptive => (None, 0),
    };
    StallContext {
        axis: key.to_string(),
        label,
        count,
        repeated_action_edit_failed: signals.repeated_action_edit_failed,
        repeated_action_service_failed: signals.repeated_action_service_failed,
        repeated_action_read_only: signals.repeated_action_read_only,
        action_oriented: signals.action_oriented,
        escalations: signals.escalations,
        max_escalations: signals.max_escalations,
        already_guided: signals.already_guided.contains(key),
        already_diagnosed: signals.already_diagnosed.contains(key),
        already_strategy_shifted: signals.already_strategy_shifted.contains(key),
        already_asked_user,
        user_intent: user_intent.map(str::to_string),
        recent_tool_signatures: recent_tool_signatures.to_vec(),
        last_tool_outcome: last_tool_outcome.map(str::to_string),
        redaction_rejected,
        repeated_clarify_count,
        modified_files: modified_files.to_vec(),
        work_epoch,
    }
}

/// Valida l'output JSON dell'LLM contro l'enum CHIUSO [`RecoveryMove`]. Qualunque
/// forma non deserializzabile, con testo vuoto dove serve, o con `blocker` fuori
/// dal vocabolario ADR 0034 ([`VALID_BLOCKERS`]) degrada a
/// [`RecoveryMove::Fallback`] (rete di sicurezza: gerarchia fissa). Punto unico
/// (regola L): il nodo e l'impl della porta chiamano SOLO questa funzione.
pub fn validate_move(raw: &serde_json::Value) -> RecoveryMove {
    let mv: RecoveryMove = match serde_json::from_value(raw.clone()) {
        Ok(m) => m,
        Err(_) => return RecoveryMove::Fallback,
    };
    match &mv {
        RecoveryMove::ContinueGuided { nudge }
        | RecoveryMove::ShiftStrategy { nudge }
        | RecoveryMove::ForceDiagnose { nudge }
            if nudge.trim().is_empty() =>
        {
            RecoveryMove::Fallback
        }
        RecoveryMove::AskUser { question } if question.trim().is_empty() => RecoveryMove::Fallback,
        RecoveryMove::DeclareBlocked { blocker }
            if !VALID_BLOCKERS.contains(&blocker.trim()) =>
        {
            RecoveryMove::Fallback
        }
        _ => mv,
    }
}

/// Traduce una [`RecoveryMove`] nella [`ProgressDecision`] equivalente (STESSA
/// struct del ramo fisso: i match `dec.action` esistenti restano l'unico punto
/// di traduzione intento->effetto, regola L). Ritorna `None` per
/// [`RecoveryMove::Fallback`]: il chiamante ricade su `progress_controller::decide`
/// (rete di sicurezza). Il testo (`nudge`/`question`/`blocker`) fluisce in
/// `nudge_text` — il consumo (iniezione nudge / `forced_declaration_delta`) e'
/// del chiamante.
pub fn translate(mv: &RecoveryMove) -> Option<ProgressDecision> {
    let d = match mv {
        RecoveryMove::Fallback => return None,
        RecoveryMove::ContinueGuided { nudge } => ProgressDecision {
            action: Action::Guide,
            axis: None,
            force_action: true,
            nudge_text: Some(nudge.clone()),
            stop_reason: None,
            reason: "meta-reasoner: prosegui guidato".to_string(),
        },
        RecoveryMove::ShiftStrategy { nudge } => ProgressDecision {
            action: Action::ChangeStrategy,
            axis: None,
            force_action: true,
            nudge_text: Some(nudge.clone()),
            stop_reason: None,
            reason: "meta-reasoner: cambia strategia".to_string(),
        },
        RecoveryMove::ForceDiagnose { nudge } => ProgressDecision {
            action: Action::ForceDiagnose,
            axis: None,
            // I log/errori devono restare leggibili: niente forza-azione
            // (coerente col ramo FORCE_DIAGNOSE fisso).
            force_action: false,
            nudge_text: Some(nudge.clone()),
            stop_reason: None,
            reason: "meta-reasoner: diagnosi forzata".to_string(),
        },
        RecoveryMove::EscalateModel => ProgressDecision {
            action: Action::Escalate,
            axis: None,
            force_action: false,
            nudge_text: None,
            stop_reason: None,
            reason: "meta-reasoner: promuovi modello".to_string(),
        },
        RecoveryMove::AskUser { question } => ProgressDecision {
            action: Action::AskUser,
            axis: None,
            force_action: false,
            nudge_text: Some(question.clone()),
            stop_reason: None,
            reason: "meta-reasoner: domanda mirata all'utente".to_string(),
        },
        RecoveryMove::DeclareBlocked { blocker } => ProgressDecision {
            action: Action::DeclareBlocked,
            axis: None,
            force_action: false,
            nudge_text: Some(blocker.clone()),
            stop_reason: None,
            reason: format!("meta-reasoner: blocco dichiarato ({blocker})"),
        },
    };
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn work_epoch_avanza_solo_sui_cambi_macroscopici() {
        let base = work_epoch(2, 1, 0);
        assert_eq!(base, work_epoch(2, 1, 0), "deterministica a input invariati");
        assert!(work_epoch(3, 1, 0) > base, "nuovo todo avanza l'epoca");
        assert!(work_epoch(2, 2, 0) > base, "escalation avanza l'epoca");
        assert!(work_epoch(2, 1, 5) > base, "bump floor avanza l'epoca");
        // Valori negativi (input sporco) trattati come 0, mai panico/underflow.
        assert_eq!(work_epoch(-1, -1, -1), 0);
    }

    #[test]
    fn validate_move_forme_valide() {
        let m = validate_move(&json!({"move": "escalate_model"}));
        assert_eq!(m, RecoveryMove::EscalateModel);
        let m = validate_move(&json!({"move": "ask_user", "question": "Qual e' l'email reale?"}));
        assert_eq!(m, RecoveryMove::AskUser { question: "Qual e' l'email reale?".into() });
        let m = validate_move(&json!({"move": "declare_blocked", "blocker": "credential"}));
        assert_eq!(m, RecoveryMove::DeclareBlocked { blocker: "credential".into() });
    }

    #[test]
    fn validate_move_malformato_degrada_a_fallback() {
        // JSON non deserializzabile in RecoveryMove.
        assert_eq!(validate_move(&json!({"move": "sconosciuto"})), RecoveryMove::Fallback);
        assert_eq!(validate_move(&json!({"foo": "bar"})), RecoveryMove::Fallback);
        // Campi vuoti.
        assert_eq!(validate_move(&json!({"move": "ask_user", "question": "   "})), RecoveryMove::Fallback);
        assert_eq!(validate_move(&json!({"move": "continue_guided", "nudge": ""})), RecoveryMove::Fallback);
        // Blocker fuori dal vocabolario ADR 0034.
        assert_eq!(
            validate_move(&json!({"move": "declare_blocked", "blocker": "boh"})),
            RecoveryMove::Fallback
        );
    }

    #[test]
    fn translate_mappa_su_action_esistenti() {
        assert_eq!(
            translate(&RecoveryMove::EscalateModel).unwrap().action,
            Action::Escalate
        );
        assert_eq!(
            translate(&RecoveryMove::ShiftStrategy { nudge: "cambia".into() }).unwrap().action,
            Action::ChangeStrategy
        );
        let ask = translate(&RecoveryMove::AskUser { question: "email?".into() }).unwrap();
        assert_eq!(ask.action, Action::AskUser);
        assert_eq!(ask.nudge_text.as_deref(), Some("email?"));
        let blk = translate(&RecoveryMove::DeclareBlocked { blocker: "credential".into() }).unwrap();
        assert_eq!(blk.action, Action::DeclareBlocked);
        assert_eq!(blk.nudge_text.as_deref(), Some("credential"));
        // Fallback -> None (il chiamante usa pc::decide).
        assert!(translate(&RecoveryMove::Fallback).is_none());
    }

    #[test]
    fn build_stall_context_da_segnali_strutturati() {
        let mut signals = ProgressSignals {
            repeated_action: Some(("run_service: pnpm dev".to_string(), 4)),
            escalations: 1,
            max_escalations: 3,
            ..Default::default()
        };
        signals.already_guided.insert("repeated_action".to_string());
        let ctx = build_stall_context(
            Axis::RepeatedAction,
            &signals,
            &["read_file|abc".to_string()],
            Some("error"),
            false,
            0,
            Some("debug login"),
            &[],
            false,
            5,
        );
        assert_eq!(ctx.axis, "repeated_action");
        assert_eq!(ctx.label.as_deref(), Some("run_service: pnpm dev"));
        assert_eq!(ctx.count, 4);
        assert!(ctx.already_guided);
        assert!(!ctx.already_diagnosed);
        assert_eq!(ctx.escalations, 1);
        assert_eq!(ctx.last_tool_outcome.as_deref(), Some("error"));
        assert_eq!(ctx.work_epoch, 5);
    }

    #[test]
    fn build_stall_context_redazione_e_clarify_cross_run() {
        let signals = ProgressSignals::default();
        let ctx = build_stall_context(
            Axis::RepeatedUserQuestion,
            &signals,
            &[],
            Some("redaction_rejected"),
            true,
            3,
            Some("perche non fa login costantino@..."),
            &[],
            true,
            0,
        );
        assert_eq!(ctx.axis, "repeated_user_question");
        assert!(ctx.redaction_rejected);
        assert_eq!(ctx.repeated_clarify_count, 3);
        assert_eq!(ctx.count, 3);
        assert!(ctx.already_asked_user);
    }
}
