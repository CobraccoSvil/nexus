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

/// Meta-step pubblicato in chat per dare visibilità a passaggi interni del
/// graph (plan del planner, decisione di routing, richiesta di chiarimento,
/// fallback provider, riflessione post-hoc). Discriminato da `kind`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMetaStep {
    /// `plan` | `routing` | `clarify` | `fallback` | `reflection`.
    pub kind: String,
    /// Titolo umano sintetico mostrato in UI (collassato).
    pub title: String,
    /// Payload strutturato dipendente da `kind`.
    pub payload: Value,
    /// Collega il meta_step a un evento precedente (es. fallback ↔ tool_use).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub created_at: String,
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
    /// Meta-step (plan/routing/clarify/fallback/reflection). Mutuamente
    /// esclusivo con `step`/`trace`/`token_delta` ma non vincolato.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_step: Option<AgentMetaStep>,
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

    // Fix M27 (auto-commit locale): a fine run completato con successo, fa
    // git add -A + commit nel project_root se ci sono cambiamenti.
    // Idempotente (skip se git status pulito), best-effort (errori solo loggati),
    // NO push automatico. L'utente decide se/quando pushare via UI.
    if matches!(status, AgentRunStatus::Completed) {
        let db_clone = db.clone();
        tokio::spawn(async move {
            auto_commit_project_changes(&db_clone, run_id).await;
        });
    }
}

/// Esegue `git add -A && git commit -m ...` nel project_root del run.
/// Idempotente: se non ci sono modifiche staged, non commit niente.
/// Best-effort: errori loggati ma non propagati.
pub async fn auto_commit_project_changes(db: &PgPool, run_id: Uuid) {
    // 1. Recupera project_id e project_root
    let row = match sqlx::query_as::<_, (Uuid, Option<String>)>(
        r#"
        SELECT r.project_id, w.absolute_path
        FROM agent_runs r
        LEFT JOIN workspaces w ON w.project_id = r.project_id AND w.is_primary = true
        WHERE r.id = $1
        LIMIT 1
        "#,
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("auto_commit: lookup run fallito: {}", e);
            return;
        }
    };
    let (_project_id, root_opt) = row;
    let root = match root_opt {
        Some(p) if !p.is_empty() => p,
        _ => return,
    };
    let root_path = std::path::PathBuf::from(&root);
    if !root_path.is_dir() {
        return;
    }
    // 2. Verifica che sia un repo git
    let is_git = tokio::process::Command::new("git")
        .args(["-C", &root, "rev-parse", "--is-inside-work-tree"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !is_git {
        return;
    }
    // Fix M34: se manca .gitignore, scrive un default minimo prima del git add
    // per evitare di committare node_modules / build artifacts / secrets.
    // Patterns scelti per coprire i default Node/Python/Rust/Next/Vite generici.
    let gitignore_path = root_path.join(".gitignore");
    if !gitignore_path.exists() {
        let default_gitignore = "\
# Auto-generato da Nexus al primo commit. Personalizza se serve.

# Dependencies
node_modules/
.pnpm-store/
__pycache__/
*.pyc
.venv/
venv/

# Build artifacts
dist/
build/
out/
.next/
.nuxt/
.turbo/
target/
*.tsbuildinfo

# Test artifacts
coverage/
.nyc_output/
test-results/
playwright-report/

# Logs
*.log
logs/
npm-debug.log*
yarn-debug.log*
yarn-error.log*

# Env / secrets
.env
.env.*
!.env.example
*.pem
*.key

# IDE
.vscode/
.idea/
*.swp
*.swo
.DS_Store
Thumbs.db
";
        if let Err(e) = tokio::fs::write(&gitignore_path, default_gitignore).await {
            tracing::warn!(
                "auto_commit: scrittura .gitignore default fallita: {} (continuo lo stesso)",
                e
            );
        } else {
            tracing::info!("auto_commit: scritto .gitignore default in {}", root);
        }
    }
    // 3. Stage all changes
    let _ = tokio::process::Command::new("git")
        .args(["-C", &root, "add", "-A"])
        .output()
        .await;
    // 4. Skip se niente staged (idempotente)
    let any_staged = tokio::process::Command::new("git")
        .args(["-C", &root, "diff", "--cached", "--quiet"])
        .output()
        .await
        .map(|o| !o.status.success()) // exit code != 0 => changes staged
        .unwrap_or(false);
    if !any_staged {
        return;
    }
    // 5. Conta i file staged per il messaggio
    let count = tokio::process::Command::new("git")
        .args(["-C", &root, "diff", "--cached", "--name-only"])
        .output()
        .await
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        })
        .unwrap_or(0);
    let msg = format!(
        "feat: modifiche da Nexus run {} ({} file)\n\n\
         Auto-commit generato da Nexus al completamento del run agente.\n\
         Per pubblicare su GitHub usa il pannello Source Control (Crea repo / Push).\n",
        run_id, count
    );
    // 6. Commit con user.email/name fallback per evitare fail se git config vuota
    let output = tokio::process::Command::new("git")
        .args([
            "-C", &root,
            "-c", "user.email=nexus@local",
            "-c", "user.name=Nexus Agent",
            "commit", "-m", &msg,
        ])
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => {
            tracing::info!(
                "auto_commit: {} file committati nel run {} (root={})",
                count, run_id, root
            );
        }
        Ok(o) => {
            tracing::warn!(
                "auto_commit: git commit fallito (exit {}): stderr={}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(e) => tracing::warn!("auto_commit: spawn git commit fallito: {}", e),
    }
}

// ---------------------------------------------------------------------------
// G4 — Memorizzazione startup_command dopo avvio servizio riuscito
// ---------------------------------------------------------------------------

/// G4: dopo un run completato con successo cerca l'ultimo `shell_exec` che ha
/// eseguito `docker compose up` e salva il comando esatto in `memory_entries`
/// con ns_type='project'. Al turno successivo, `build_project_context_block`
/// lo mostrerà in "Memoria di progetto" → l'agente sa già cosa eseguire.
///
/// Fire-and-forget: gli errori non bloccano il return del run.
pub async fn save_startup_command_if_needed(
    db: &PgPool,
    project_id: Uuid,
    steps: &[AgentStep],
) {
    // Cerca l'ultimo step shell_exec riuscito con `docker compose up`
    let docker_cmd = steps
        .iter()
        .rev()
        .filter(|s| {
            s.tool_name == "shell_exec"
                && s.status == AgentStepStatus::Completed
        })
        .find_map(|s| {
            let cmd = s.tool_input.get("command").and_then(|v| v.as_str())?;
            if cmd.contains("docker") && cmd.contains("compose") && cmd.contains("up") {
                Some(cmd.to_string())
            } else {
                None
            }
        });

    let cmd = match docker_cmd {
        Some(c) => c,
        None => return, // nessun docker compose up → niente da salvare
    };

    // Cerca o crea il namespace di tipo 'project' per questo progetto
    let ns_key = format!("project:{project_id}");
    let ns_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM memory_namespaces WHERE ns_key = $1 AND active = TRUE LIMIT 1",
    )
    .bind(&ns_key)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    let ns_id = match ns_id {
        Some(id) => id,
        None => {
            let new_id = Uuid::new_v4();
            let _ = sqlx::query(
                "INSERT INTO memory_namespaces \
                 (id, ns_key, ns_type, project_id, merge_strategy, active) \
                 VALUES ($1, $2, 'project', $3, 'lww', TRUE) \
                 ON CONFLICT (ns_key) DO NOTHING",
            )
            .bind(new_id)
            .bind(&ns_key)
            .bind(project_id)
            .execute(db)
            .await;
            match sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM memory_namespaces WHERE ns_key = $1 AND active = TRUE LIMIT 1",
            )
            .bind(&ns_key)
            .fetch_optional(db)
            .await
            {
                Ok(Some(id)) => id,
                _ => {
                    tracing::debug!(
                        "G4: impossibile creare/trovare namespace per {ns_key}"
                    );
                    return;
                }
            }
        }
    };

    let value = serde_json::json!(cmd);
    let _ = sqlx::query(
        "INSERT INTO memory_entries \
         (id, namespace_id, entry_key, value, written_by, version, vector_clock) \
         VALUES (gen_random_uuid(), $1, 'startup_command', $2, 'agent_run', 1, '{}') \
         ON CONFLICT (namespace_id, entry_key) WHERE deleted = FALSE \
         DO UPDATE SET value = EXCLUDED.value, \
                       written_by = EXCLUDED.written_by, \
                       version = memory_entries.version + 1, \
                       updated_at = NOW()",
    )
    .bind(ns_id)
    .bind(&value)
    .execute(db)
    .await
    .map(|_| {
        tracing::info!(
            "G4: startup_command salvato per progetto {project_id}: {cmd:?}"
        );
    })
    .map_err(|e| tracing::warn!("G4: salvataggio startup_command fallito: {e}"));
}
