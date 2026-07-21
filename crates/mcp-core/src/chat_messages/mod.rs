use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    agent_types::{AgentRunStatus, AgentStep, AgentStepEvent, AgentStepStatus, SupervisorMode},
    auth::Claims,
    chat_learning::{
        api_error, apply_project_learning, dedup_on_write, ensure_project_access, hash_hint,
        normalize_text, parse_project_id, parse_user_id, ApiError, ApiResult,
    },
    chat_sessions::{load_session_context, update_user_active_project},
    orchestrator::{AutomationMode, ChatAttachment, OrchestratorRequest, OrchestratorResult},
    profiles::fetch_profile_context,
    projects::load_project_context,
    vector_memory, AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendChatMessageRequest {
    pub content: String,
    pub profile_id: Option<String>,
    #[serde(default)]
    pub active_files: Vec<String>,
    pub provider_override: Option<String>,
    pub model_override: Option<String>,
    pub automation_mode: Option<String>,
    pub supervisor_mode: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ChatAttachmentRequest>,
    /// Hint opzionale: nome dell'AgentType da usare (es. "Coder", "Tester").
    /// Se presente, bypassa il Q-Learning router e forza quel tipo di agente.
    pub agent_type_hint: Option<String>,
    /// Se true, il messaggio e' generato automaticamente dal sistema
    /// (es. auto-continuazione in modalita' "automatic") e NON deve essere
    /// mostrato nella UI come messaggio utente. Viene comunque persistito
    /// nel DB e usato per triggerare l'agent run.
    #[serde(default)]
    pub synthetic: bool,
    /// Segnale STRUTTURATO di RIATTIVAZIONE (regola N: identificatore esplicito,
    /// non la stringa magica "riprendi"): il pulsante "Riattiva" del banner "chat
    /// sospesa" lo imposta a true per continuare l'ultimo run `interrupted` dallo
    /// stato salvato (`messages_json`), a prescindere dal `content`. Default false.
    #[serde(default)]
    pub resume: bool,
    /// Chiave di idempotenza generata dal client (mig progetto 0008): un retry
    /// di rete della stessa POST porta lo stesso UUID e il backend, se il
    /// messaggio risulta gia' persistito nella sessione, fa replay della
    /// risposta invece di duplicare messaggio e agent run.
    pub client_message_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachmentRequest {
    pub name: String,
    pub mime_type: String,
    pub size_bytes: i64,
    #[serde(default)]
    pub text_content: String,
    #[serde(default)]
    pub base64_content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackErrorRequest {
    pub comment: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackPositiveRequest {
    /// Commento opzionale (es. "perfetto", "soluzione elegante"). Salvato per audit ma non genera correzioni.
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyChatRequest {
    pub project_id: String,
    pub profile_id: String,
    pub message: String,
    #[serde(default)]
    pub active_files: Vec<String>,
}

// ---------------------------------------------------------------------------
// Sottomoduli: il file originale chat_messages.rs (5853 righe) e stato spezzato
// per responsabilita coesa. L API pubblica del crate resta IDENTICA: gli handler
// axum e le struct di richiesta sono ri-esportati qui sotto, percio i call site
// esterni (routes/public.rs, chat_attachments.rs) non cambiano.
// ---------------------------------------------------------------------------

pub(crate) mod agent_run;
mod auto_compact;
mod context;
mod handlers;
mod intent;
mod persistence;
mod run;

// Re-export interni: i sottomoduli usano  e accedono cosi ai
// simboli pub(crate) condivisi (helper di contesto, persistenza, intent, ecc.).
pub(crate) use agent_run::*;
pub(crate) use auto_compact::*;
pub(crate) use context::*;
pub(crate) use intent::*;
pub(crate) use persistence::*;
pub(crate) use run::*;

// Contratto pubblico verso l esterno: handler axum + struct di richiesta usate
// nelle route. Stesso insieme di nomi del file monolitico originale.
pub use handlers::{
    delete_chat_message, feedback_assist_handler, feedback_error, feedback_positive, legacy_chat,
    list_chat_messages, precheck_chat_message, resend_chat_message, send_chat_message,
};
