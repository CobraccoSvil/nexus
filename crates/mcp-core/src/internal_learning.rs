//! Endpoint internal `/api/internal/learning/*`.
//!
//! Questo modulo espone la persistenza Q-learning del nexus-orchestrator al
//! brain Python (e ad altri client interni). Sostituisce la chiamata gRPC
//! `AgentRouterClient.submit_feedback`, eliminando la dipendenza Python sul
//! protobuf `agent_router_pb2` per il path di feedback.
//!
//! Vantaggi:
//! - **Un solo writer Q-table**: Rust e' l'unica autorita' che scrive su
//!   `nexus_q_values`. Niente race condition tra Python (gRPC) e Rust (DB
//!   diretto).
//! - **Zero proto regen**: il brain non deve piu' rigenerare gli stub Python
//!   ad ogni cambio dello schema FeedbackRequest. Aggiungere un campo qui
//!   (es. `confidence`) richiede solo update di `LearningFeedbackRequest`.
//! - **Coerente con `/api/internal/routing/decide`** (Fase A consolidamento).
//!
//! La logica gRPC originale resta in `agent_router_server.rs::submit_feedback`
//! per backward-compat, ma diventera' deprecata: una volta confermato che
//! tutti i caller usano il REST, il gRPC handler puo' essere rimosso.

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Body della richiesta `POST /api/internal/learning/feedback`.
#[derive(Debug, Deserialize)]
pub struct LearningFeedbackRequest {
    /// Identificatore univoco del task / agent run.
    pub task_id: String,
    /// Tipo del task (es. "fix", "refactor", "system_admin").
    pub task_type: String,
    /// Nome dell'agente che ha eseguito il task (snake_case o PascalCase).
    /// Es. "coder_base", "Architect", "github_pr_manager".
    pub agent_type: String,
    /// Reward osservato in [0.0, 1.0]. Valori fuori range vengono clamped.
    /// Tipica logica brain: 0.0 errore, 0.3 max iterazioni, 0.4 default,
    /// 1.0 successo. Modulato da reflection_score se attivo.
    pub reward: f32,
    /// Durata totale dell'esecuzione (millisecondi).
    #[serde(default)]
    pub duration_ms: u64,
    /// True se l'episodio e' terminale (run completato). Falso per
    /// step intermedi (oggi sempre true; mantenuto per compatibility col
    /// vecchio gRPC `FeedbackRequest`).
    #[serde(default = "default_true")]
    pub is_terminal: bool,
}

fn default_true() -> bool { true }

/// Risposta: nuovo Q-value calcolato per (task_type, agent_type).
#[derive(Debug, Serialize)]
pub struct LearningFeedbackResponse {
    pub new_q_value: f32,
    pub recorded: bool,
}

/// Handler `POST /api/internal/learning/feedback`.
/// Riceve il reward, lo propaga al `NexusBridge` (q_learning + reactive workers).
pub async fn submit_feedback(
    State(_state): State<AppState>,
    Json(body): Json<LearningFeedbackRequest>,
) -> Result<Json<LearningFeedbackResponse>, (StatusCode, String)> {
    if body.task_id.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "campo `task_id` vuoto".to_string()));
    }
    if body.agent_type.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "campo `agent_type` vuoto".to_string()));
    }

    let bridge = match crate::nexus_bridge::NexusBridge::global() {
        Some(b) => b,
        None => {
            tracing::debug!(
                "internal_learning: feedback ricevuto ma NexusBridge assente, no-op (task_id={})",
                body.task_id,
            );
            return Ok(Json(LearningFeedbackResponse {
                new_q_value: 0.0,
                recorded: false,
            }));
        }
    };

    // Stessa logica del gRPC handler in agent_router_server.rs:
    // 1) snake_case -> PascalCase
    // 2) AgentType::from_name (fallback a "Unknown" per nomi non riconosciuti)
    // 3) success = reward >= 0.5
    let pascal = snake_to_pascal(&body.agent_type);
    let agent_type = nexus_orchestrator::AgentType::from_name(&pascal);
    let reward = body.reward.clamp(0.0, 1.0);
    let success = reward >= 0.5;
    let new_q = bridge.record_outcome(
        &body.task_id,
        &body.task_type,
        agent_type,
        success,
        reward,
        body.duration_ms,
        if success { None } else { Some("reward<0.5".to_string()) },
    );
    tracing::info!(
        "internal_learning: feedback task_id={} task_type={} agent={} reward={:.2} -> Q={:.3}",
        body.task_id, body.task_type, body.agent_type, reward, new_q,
    );

    Ok(Json(LearningFeedbackResponse {
        new_q_value: new_q,
        recorded: true,
    }))
}

/// Conversione snake_case → PascalCase. Duplica il helper privato in
/// `agent_router_server.rs::snake_to_pascal` perche' quello e' privato al
/// modulo gRPC. Manteniamo la logica identica per coerenza tra i due path
/// (gRPC legacy + REST nuovo).
pub(crate) fn snake_to_pascal(name: &str) -> String {
    if name.chars().next().map(char::is_uppercase).unwrap_or(false) && !name.contains('_') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut capitalize_next = true;
    for ch in name.chars() {
        if ch == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            out.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }
    out.replace("Github", "GitHub")
}
