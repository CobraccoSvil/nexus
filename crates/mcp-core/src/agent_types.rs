//! Tipi pubblici condivisi del loop agente e helper DB.
//!
//! Estratto da `agent_loop.rs` durante la Fase 4 del refactor Nexus: il loop
//! vero e proprio e' ora nel brain LangGraph (Python), ma questi tipi (step,
//! run result, eventi broadcast, helper DB, ecc.) sono ancora consumati dal
//! ponte `brain_agent_client` e dall'SSE del frontend.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[allow(dead_code)]
pub const AGENT_MAX_ITERATIONS: u32 = 60;
#[allow(dead_code)]
pub const AGENT_TIMEOUT_SECS: u64 = 480;

// ---------------------------------------------------------------------------
// SupervisorMode — modalità di supervisione AI del worker
// ---------------------------------------------------------------------------

/// Modalità di supervisione del processo agente.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorMode {
    /// Nessuna supervisione (default)
    #[default]
    None,
    /// Il supervisor viene chiamato solo quando rileva anomalie:
    /// loop, errori ripetuti, step count > 20 (Modalità C — più economica)
    Anomaly,
    /// Il supervisor controlla ogni N iterazioni (Modalità A)
    #[serde(rename = "interleaved")]
    Interleaved,
    /// Il supervisor controlla dopo ogni iterazione (Modalità B — più precisa)
    Continuous,
}

impl SupervisorMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Anomaly => "anomaly",
            Self::Interleaved => "interleaved",
            Self::Continuous => "continuous",
        }
    }

    #[allow(dead_code)]
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "anomaly" | "c" => Self::Anomaly,
            "interleaved" | "a" => Self::Interleaved,
            "continuous" | "b" => Self::Continuous,
            _ => Self::None,
        }
    }

    /// Ogni quante iterazioni controllare (per Interleaved)
    #[allow(dead_code)]
    fn check_interval(self) -> u32 { 5 }

    /// Se il supervisor deve essere chiamato a questa iterazione
    #[allow(dead_code)]
    pub fn should_check(self, iteration: u32, anomaly: bool) -> bool {
        match self {
            Self::None => false,
            Self::Anomaly => anomaly,
            Self::Interleaved => iteration > 0 && iteration % self.check_interval() == 0,
            Self::Continuous => iteration > 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tipi pubblici dell'agent run
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStep {
    pub run_id: String,
    pub step_index: u32,
    pub tool_name: String,
    pub tool_input: Value,
    pub tool_result: Option<String>,
    pub status: AgentStepStatus,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStepStatus {
    Running,
    Completed,
    Failed,
    AwaitingConfirmation,
    Skipped,
    /// Tutti i provider configurati sono in cooldown / non disponibili.
    /// Stato emesso da `chat_messages.rs::spawn_agent_run` quando il
    /// routing ritorna `no_capable_provider=true`. La UI deve mostrare un
    /// banner di alert (vedi `chat-panel.tsx` gestione `provider_unavailable`)
    /// e NON avviare il run agente.
    ProviderUnavailable,
}

impl AgentStepStatus {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Skipped => "skipped",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPendingAction {
    pub index: usize,
    pub tool_name: String,
    pub tool_input: Value,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Running,
    Completed,
    AwaitingConfirmation,
    Failed,
    TimedOut,
    Cancelled,
    /// Il brain ha rilevato un loop (stessa tool call ripetuta >= LOOP_THRESHOLD volte)
    /// e tutti i tentativi di escalation intra-provider e cross-provider sono esauriti.
    LoopAborted,
    /// Nessun provider disponibile: tutti in cooldown (billing_error / rate_limit)
    /// o non configurati. Il turno non ha potuto essere elaborato.
    ProviderUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunResult {
    pub run_id: String,
    pub status: AgentRunStatus,
    pub steps: Vec<AgentStep>,
    pub pending_actions: Vec<AgentPendingAction>,
    pub final_answer: Option<String>,
    pub provider: String,
    pub model: String,
    pub iteration_count: u32,
    /// `true` se il Nexus Q-Learning router ha sovrascritto provider/model
    /// e iniettato il system prompt per questo run.
    pub nexus_override_applied: bool,
    /// AgentType suggerito dal Q-Learning router (es. `"Coder"`, `"Architect"`).
    /// `None` se il bridge non era disponibile o non ha prodotto una decisione.
    pub nexus_agent_type: Option<String>,
    /// Q-value del router per questa decisione. `None` se assente.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nexus_q_value: Option<f32>,
    /// Avviso privacy per provider non-EU/non-locali. Mostrato prima della risposta.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_privacy_notice: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub total_cost: f64,
    /// Classe errore propagata dal brain (es. "billing_error", "rate_limit",
    /// "overloaded", "provider_error"). Permette al chiamante in chat_messages.rs
    /// di decidere se ritentare con altro provider e di applicare il cooldown
    /// corretto (lungo per billing, breve per transient 5xx/429).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    /// Stop reason finale: end_turn | tool_use | error | loop_detected | timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Task type rilevato dal router (es. "fix", "code_read", "architecture").
    /// Propagato dal brain Python nell'evento SSE end_turn per popolare
    /// la colonna `nexus_task_type` in agent_runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nexus_task_type: Option<String>,
    /// `true` se l'agente ha dichiarato di aver completato senza invocare
    /// alcun tool nonostante avesse tool disponibili (0 step, iteration <= 1).
    /// Tipico di modelli piccoli che "allucinano il completamento".
    #[serde(default)]
    pub hollow_completion: bool,
}

/// Evento di trace LLM: mostra i messaggi inviati al provider e la risposta ricevuta.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AITraceEvent {
    pub run_id: String,
    pub iteration: u32,
    pub provider: String,
    pub model: String,
    pub messages_sent: u32,       // quanti messaggi nella conversazione
    pub tools_count: u32,          // quanti tool disponibili
    pub response_text: String,     // testo della risposta (troncato)
    pub tool_calls: Vec<Value>,    // tool call names + inputs
    pub stop_reason: String,
    pub timestamp: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
}

/// Evento trasmesso via broadcast per l'SSE del frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStepEvent {
    pub run_id: String,
    pub step: Option<AgentStep>,
    pub trace: Option<AITraceEvent>,
    pub is_final: bool,
    /// Token parziale durante la generazione (streaming). Se presente, è evento `agent_token`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_delta: Option<String>,
}

// ---------------------------------------------------------------------------
// Helper detection testo utente
// ---------------------------------------------------------------------------

/// Rileva se il messaggio utente richiede un'azione operativa (build, deploy, run, ecc.).
#[allow(dead_code)]
pub(crate) fn detect_action_request(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    let action_patterns: &[&str] = &[
        // Italiano — imperativo / infinito / futuro
        "avvia", "avviare", "lancia", "lanciare",
        "esegui", "eseguire",
        "builda", "buildare",
        "crea ", "creare", "crea il", "crea la",
        "installa", "installare",
        "configura", "configurare",
        "deploya", "deployare",
        "compila", "compilare",
        "fai partire", "metti in piedi", "porta in su", "metti online",
        "avvia i servizi", "avvia il servizio", "avvia il backend", "avvia il frontend",
        "avvia il server", "avvia i container",
        "testa il", "testa la",
        // Inglese — imperativo / common forms
        "start ", "start the", "launch ", "launch the",
        " run ", "run the", "run it",
        " build", "build the", "build it",
        " create ", "create the",
        "install ", "install the",
        "setup ", "set up ", "configure ",
        "deploy ", "deploy the",
        "compile ", "compile the",
        // Tool / tecnologie specifiche (alta probabilità d'azione)
        "docker", "docker-compose", "docker compose",
        "npm install", "npm run", "pnpm install", "pnpm run",
        "cargo build", "cargo run",
        "dotnet run", "dotnet build", "dotnet watch",
        "pip install", "pip3 install",
        "apt install", "apt-get install",
        "systemctl start", "service start",
        "make ", "make\t",
    ];
    action_patterns.iter().any(|p| lower.contains(p))
}

// ---------------------------------------------------------------------------
// DB helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub async fn insert_agent_run(
    db: &PgPool,
    run_id: Uuid,
    session_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
    message_id: Uuid,
    automation_mode: &str,
    provider: &str,
    model: &str,
    supervisor_mode: SupervisorMode,
) {
    let _ = sqlx::query(
        r#"
        INSERT INTO agent_runs (id, session_id, project_id, user_id, run_message_id, status, automation_mode, provider, model, supervisor_mode)
        VALUES ($1, $2, $3, $4, $5, 'running', $6, $7, $8, $9)
        "#,
    )
    .bind(run_id)
    .bind(session_id)
    .bind(project_id)
    .bind(user_id)
    .bind(message_id)
    .bind(automation_mode)
    .bind(provider)
    .bind(model)
    .bind(supervisor_mode.as_str())
    .execute(db)
    .await;
}

/// Funzione pubblica per finalizzare un run (usata da chat_messages per la ripresa).
#[allow(dead_code)]
pub async fn finalize_agent_run(
    db: &PgPool,
    run_id: Uuid,
    status: AgentRunStatus,
    final_answer: Option<&str>,
    iteration_count: u32,
) {
    let status_str = match &status {
        AgentRunStatus::Completed => "completed",
        AgentRunStatus::Failed => "failed",
        AgentRunStatus::TimedOut => "timed_out",
        AgentRunStatus::AwaitingConfirmation => "awaiting_confirmation",
        AgentRunStatus::Cancelled => "cancelled",
        AgentRunStatus::Running => "running",
        AgentRunStatus::LoopAborted => "loop_aborted",
        AgentRunStatus::ProviderUnavailable => "provider_unavailable",
    };
    let _ = sqlx::query("UPDATE agent_runs SET status = $2, final_answer = $3, iteration_count = $4, completed_at = NOW() WHERE id = $1")
    .bind(run_id)
    .bind(status_str)
    .bind(final_answer)
    .bind(iteration_count as i32)
    .execute(db)
    .await;
}
